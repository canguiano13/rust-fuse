use std::fs::File;
use std::io;
use std::io::Error;
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::io::ErrorKind;
use std::io::BufReader;
use std::io::BufRead;
use std::path::PathBuf;
use std::path::Path;
use std::os::unix::fs::FileExt;
use std::fs::OpenOptions;
use std::time::Duration;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;
use std::sync::RwLock;

use log::LevelFilter;
use log::debug;
use log::error;
use log::info;
// use log::warn;

use clap::Parser;
use bitmaps::Bitmap;

use serde::Deserialize;
use serde::Serialize;
// use serde::Serializer;
// use serde::Deserializer;

use fuser::Filesystem;
use fuser::SessionACL;
use fuser::MountOption;
use fuser::Request;
use fuser::INodeNo;
use fuser::FileHandle;
use fuser::OpenFlags;
use fuser::LockOwner;
use fuser::ReplyData;
use fuser::Config;
use fuser::Errno;

const FSID: u32 = 0x55555;
const META_FILE_NAME: &str = "meta.fs";
const STORE_FILE_NAME: &str = "store.fs";

#[derive(Serialize, Deserialize, Clone, Debug)]
struct Superblock {
    // Magic number identifying the file system.
    fsid: u32,
    // Block size in bytes.
    block_size: u32,
    num_inodes: u64,
    num_blocks: u64,
}

impl Superblock {
    fn new(block_size: u32, num_inodes: u64, num_blocks: u64) -> Superblock {
        Superblock {
            fsid: FSID,
            block_size,
            num_inodes,
            num_blocks,
        }
    }
}

// The first value is the start location; the second value is the extent length.
type Extent = (u64, u64);

// NOTE: Only support three basic kinds of files.
#[derive(Serialize, Deserialize, Copy, Clone, PartialEq, Default, Debug)]
enum FileKind {
    #[default]
    File,
    Directory,
    Symlink,
}

impl From<FileKind> for fuser::FileType {
    fn from(kind: FileKind) -> Self {
        match kind {
            FileKind::File => fuser::FileType::RegularFile,
            FileKind::Directory => fuser::FileType::Directory,
            FileKind::Symlink => fuser::FileType::Symlink,
        }
    }
}

// Some helper functions for time.
// =============================================================================
fn time_now() -> (i64, u32) {
    time_from_system_time(&SystemTime::now())
}

fn system_time_from_time(secs: i64, nsecs: u32) -> SystemTime {
    if secs >= 0 {
        UNIX_EPOCH + Duration::new(secs as u64, nsecs)
    } else {
        UNIX_EPOCH - Duration::new((-secs) as u64, nsecs)
    }
}

fn time_from_system_time(system_time: &SystemTime) -> (i64, u32) {
    // Convert to signed 64-bit time with epoch at 0
    match system_time.duration_since(UNIX_EPOCH) {
        Ok(duration) => (duration.as_secs() as i64, duration.subsec_nanos()),
        Err(before_epoch_error) => (
            -(before_epoch_error.duration().as_secs() as i64),
            before_epoch_error.duration().subsec_nanos(),
        ),
    }
}
// =============================================================================

#[derive(Serialize, Deserialize, Default, Clone, Debug)]
struct InodeAttributes {
    pub inode: u64,
    // Ref count of open file handles to this inode
    pub open_file_handles: u64,
    pub size: u64,
    pub last_accessed: (i64, u32),
    pub last_modified: (i64, u32),
    pub last_metadata_changed: (i64, u32),
    pub kind: FileKind,
    // Permissions and special mode bits
    pub mode: u16,
    pub hardlinks: u32,
    pub uid: u32,
    pub gid: u32,
    // NOTE: Giving up on fixed array for extents, and fixed metadata size in
    // general (for ease-of-implementation). This no longer corresponds to the
    // design of vsfs in OSTEP.
    pub extent_index: Vec<Extent>,
}

// Table (flat data structure: a vector) with inodes.
type InodeTable = Vec<InodeAttributes>;

#[derive(Serialize, Deserialize)]
struct MetaSerializable {
    superblock: Superblock,
    // NOTE: It would actually be great to have a u8 vec for higher data density,
    // but I'm really strapped for time and don't want to deal with even the
    // minor complexity of conversion from bits to u8. So bools it is!
    // TODO: You should really make a u8 vec; metadata size is a bit crazy
    // right now lol.
    inode_bitmap: Vec<bool>,
    data_bitmap: Vec<bool>,
    inode_table: InodeTable,
}

impl MetaSerializable {
    fn to_meta(&self) -> Meta {
        let inode_bitmap_len = usize::try_from(self.superblock.num_inodes / 1024).unwrap();
        let mut inode_bitmap = vec![Bitmap::<1024>::new(); inode_bitmap_len];
        for (i, bit) in self.inode_bitmap.iter().enumerate() {
            if *bit {
                let chunk_index = i / 1024;
                let bit_index = i % 1024;
                // Set bit in the actual bitmap
                inode_bitmap[chunk_index].set(bit_index, true);
            }
        };
        let data_bitmap_len = usize::try_from(self.superblock.num_blocks / 1024).unwrap();
        let mut data_bitmap = vec![Bitmap::<1024>::new(); data_bitmap_len];
        for (i, bit) in self.data_bitmap.iter().enumerate() {
            if *bit {
                let chunk_index = i / 1024;
                let bit_index = i % 1024;
                // Set bit in the actual bitmap
                data_bitmap[chunk_index].set(bit_index, true);
            }
        };
        Meta {
            superblock: self.superblock.clone(),
            inode_bitmap,
            data_bitmap,
            inode_table: RwLock::new(self.inode_table.clone()),
        }
    }
}

const BITMAP_CHUNK_BITS: usize  = 1024;

struct Meta {
    superblock: Superblock,
    inode_bitmap: Vec<Bitmap<BITMAP_CHUNK_BITS>>,
    data_bitmap: Vec<Bitmap<BITMAP_CHUNK_BITS>>,
    inode_table: RwLock<InodeTable>,
}

impl Meta {
    fn to_meta_serializable(&self) -> MetaSerializable {
        let mut inode_bmap_bool: Vec<bool> = Vec::new();
        for chunk in &self.inode_bitmap {
            for i in 0..BITMAP_CHUNK_BITS {
                if chunk.get(usize::try_from(i).unwrap()) {
                    inode_bmap_bool.push(true)
                } else {
                    inode_bmap_bool.push(false)
                }
            }
        };
        let mut data_bmap_bool: Vec<bool> = Vec::new();
        for chunk in &self.data_bitmap {
            for i in 0..BITMAP_CHUNK_BITS {
                if chunk.get(usize::try_from(i).unwrap()) {
                    data_bmap_bool.push(true)
                } else {
                    data_bmap_bool.push(false)
                }
            }
        };
        MetaSerializable {
            superblock: self.superblock.clone(),
            inode_bitmap: inode_bmap_bool,
            data_bitmap: data_bmap_bool,
            inode_table: self.inode_table.read().unwrap().clone(),
        }
    }
}

// A B-tree map relating file name to a tuple of (inode number, file kind).
type DirectoryEntries = BTreeMap<String, (u64, FileKind)>;

struct FuseFS {
    // fs_dir: PathBuf,
    meta: Meta,
    meta_fd: File,
    store_fd: File,
}

// Implement methods specific to FuseFS design and structure.
impl FuseFS {
    fn new(fs_dir_path: PathBuf,
           block_size: u32,
           num_inodes: u64,
           num_blocks: u64
    ) -> Result<FuseFS, Error> {
        info!("Creating filesystem..");
        // Construct paths to expected files
        let mut meta_file_path: PathBuf = fs_dir_path.clone();
        meta_file_path.push(Path::new(META_FILE_NAME));
        let mut store_file_path: PathBuf = fs_dir_path.clone();
        store_file_path.push(Path::new(STORE_FILE_NAME));

        // If the filesystem backing files already exist, load in existing
        // metadata. Otherwise, initialize new defaults.
        let meta = if meta_file_path.exists() && store_file_path.exists() {
            info!("Loading existing filesystem...");
            let fd = File::open(&meta_file_path)?;
            let reader = BufReader::new(fd);
            // Update metadata with existing information from file.
            let meta_ser: MetaSerializable = match serde_json::from_reader(reader) {
                Ok(m) => m,
                Err(e) => return Err(Error::new(ErrorKind::InvalidData, e)),
            };
            meta_ser.to_meta()
        } else {
            info!("Creating new filesystem...");
            Meta {
                superblock: Superblock::new(block_size, num_inodes, num_blocks),
                // TODO: Check if this syntax actually does what you want it to do.
                inode_bitmap: vec![Bitmap::<BITMAP_CHUNK_BITS>::new(); num_inodes as usize / BITMAP_CHUNK_BITS],
                data_bitmap: vec![Bitmap::<BITMAP_CHUNK_BITS>::new(); num_blocks as usize / BITMAP_CHUNK_BITS],
                inode_table: RwLock::new(vec![InodeAttributes::default(); num_inodes as usize]),
            }
        };
        // Open corresponding filesystem backing files; this will open the files
        // if they already exist, or create new files if they don't.
        let meta_fd = OpenOptions::new()
            .create(true)
            .write(true)
            .read(true)
            .open(meta_file_path)?;
        // TODO: Statically allocate the maximum size for the filesystem from
        // num_blocks * block_size.
        let store_fd = OpenOptions::new()
            .create(true)
            .write(true)
            .read(true)
            .open(store_file_path)?;

        info!("Created filesystem.");
        Ok(FuseFS {
            // fs_dir: fs_dir_path,
            meta,
            meta_fd,
            store_fd,
        })
    }

    // NOTE: This just panics if something goes wrong. Should be fine, since
    // we will probably only call this when shutting down the filesystem. Also,
    // I'm lazy. Maybe you should handle errors nicely at some point.
    fn flush_meta(&self) {
        let meta_ser = self.meta.to_meta_serializable();
        // Small hack to zero the file so we actually overwrite everything.
        self.meta_fd.set_len(0).unwrap();
        // TODO: Should really use rkyv and not serde_json for higher data density.
        // Right now, the size of your metadata is pretty insane (due to JSON
        // encoding overhead, I presume).
        serde_json::to_writer(&self.meta_fd, &meta_ser).unwrap();
    }

    // If an inode is available, set the inode bit and return the offset of the
    // free block from the start of data region. Otherwise, do nothing and return
    // None. We currently just use a simple linear search.
    // NOTE (side-effect): This function may change inode bitmap state.
    fn allocate_inode(&self) -> Option<INodeNo> {
        for (chunk_idx, chunk) in self.meta.inode_bitmap.iter().enumerate() {
            // check each bit until we find a free space
            match chunk.first_false_index() {
                Some(i) => {
                    // Get mutable chunk here
                    let mut mutable_chunk = self.meta.inode_bitmap[chunk_idx];
                    mutable_chunk.set(i, true);
                    // Wrap in INodeNo (which fuser provides)
                    return Some(INodeNo(((chunk_idx * BITMAP_CHUNK_BITS) + i) as u64));
                },
                None => continue,
            }
        };
        return None
    }

    fn free_inode(&self, inode: INodeNo) {
        let chunk_idx = inode.0 as usize / BITMAP_CHUNK_BITS;
        let bit_idx = inode.0 as usize % BITMAP_CHUNK_BITS;
        let mut mutable_chunk = self.meta.inode_bitmap[chunk_idx];
        mutable_chunk.set(bit_idx, false);
    }

    fn set_inode_attr(&self, inode: INodeNo, attr: InodeAttributes) {
        self.meta.inode_table.write().unwrap()[inode.0 as usize] = attr;
    }

    fn get_inode_attr(&self, inode: INodeNo) -> Result<InodeAttributes, Errno> {
        let chunk = inode.0 as usize / BITMAP_CHUNK_BITS;
        let bit = inode.0 as usize % BITMAP_CHUNK_BITS;
        // Check if inode is actually allocated in the bitmap
        match self.meta.inode_bitmap.get(chunk) {
            None => {
                return Err(Errno::ENOENT)
            }
            Some(bitmap) => {
                if !bitmap.get(bit) {
                    return Err(Errno::ENOENT);
                }
            }
        }
        // If it is, get inode attributes from the inode table
        let itable = self.meta.inode_table.read().unwrap();
        Ok(itable[inode.0 as usize].clone())
    }

    // If space is available of the desired size, allocate that space by setting
    // all corresponding data bitmap bits and return the associatd extent(s).
    // Otherwise, return None.
    // NOTE (side-effect): This function may change data bitmap state.
    // TODO: This function is not correct, need to debug. Generally, you might
    // need to rewrite this function --- the second value in an extent is the
    // size of the extent, not the ending index.
    fn allocate_blocks(&self, size: usize) -> Option<Vec<Extent>> {
        // Return immediately with empty extent vector if size is 0
        if size == 0 {
            return Some(vec![])
        };
        let mut num_blocks_needed = size / (self.meta.superblock.block_size as usize) + 1;
        let mut extents: Vec<Extent> = Vec::new();
        // Find list of extents with total desired size
        for (chunk_idx, chunk) in self.meta.data_bitmap.iter().enumerate() {
            let base_offset = chunk_idx * BITMAP_CHUNK_BITS;
            let mut ex_open: usize;
            let mut ex_size: usize = 0;
            // check each bit until we find a free space
            // Assertion here should be that num_blocks_needed > 0
            ex_open = match chunk.first_false_index() {
                Some(i) => {
                    // Decrement number of blocks needed
                    num_blocks_needed -= 1;
                    // Add to extent size
                    ex_size += 1;
                    i
                },
                None => continue,
            };
            loop {
                let next_filled_i = match chunk.next_index(ex_open) {
                    Some(i) => i,
                    // A bit weird, but we want to re-use the logic below. If
                    // there is no next 'true' bit, then we are free until the
                    // very end of the chunk, and we want to loop all the way
                    // up until the very last bit in the chunk.
                    None => BITMAP_CHUNK_BITS + 1,
                };
                let free_size = next_filled_i - ex_open;
                if free_size > num_blocks_needed {
                    // TODO: You're conflating the size of the extent with the
                    // ending index of the extent, which is causing off-by-one
                    // problems. Need to fix this!
                    // NOTE: At this point, we know that we have found enough
                    // space for the full allocation.
                    ex_size += num_blocks_needed;
                    extents.push(((base_offset + ex_open) as u64, ex_size as u64));
                    // Only actually allocate/set inode bits when we definitely
                    // have enough space for the full allocation.
                    for ex in &extents {
                        let open = ex.0;
                        let size = ex.1;
                        let mut mutable_chunk = self.meta.data_bitmap[open as usize / BITMAP_CHUNK_BITS];
                        let bit_offset_open = open as usize % BITMAP_CHUNK_BITS;
                        for i in bit_offset_open..(bit_offset_open + size as usize) {
                            mutable_chunk.set(i as usize, true);
                        }
                    }
                    return Some(extents)
                } else {
                    // Allocate next free section of blocks to our extent.
                    ex_size += free_size;
                    // Decrease number of free blocks still needed accordingly.
                    num_blocks_needed -= free_size;
                    extents.push(((base_offset + ex_open) as u64, ex_size as u64));
                }
                ex_open = match chunk.next_false_index(ex_open + ex_size) {
                    // If we still have free blocks left in the current chunk,
                    // set the new initial extent offset to the next free block
                    // and continue the loop in the current chunk.
                    Some(i) => {
                        // Reset extent size. NOTE: this code is truly horrible.
                        ex_size = 1;
                        num_blocks_needed -= 1;
                        i
                    },
                    // If we're out of free blocks in the current chunk, break
                    // out of this loop and go to the next chunk (in the outer
                    // for-loop).
                    None => break,
                };
            }
        };
        return None
    }

    fn free_blocks() {

    }

    fn read_extent(&self, ex: &Extent) -> Result<Vec<u8>, Errno> {
        let start_byte = ex.0 * self.meta.superblock.block_size as u64;
        let extent_size_bytes = ex.1 * self.meta.superblock.block_size as u64;
        // Read specified chunk of file using read_at
        // NOTE: read_at is specific to Unix systems, so this means, of course,
        // that our implementation will only work on Unix systems.
        let mut buf: Vec<u8> = vec![0u8; extent_size_bytes as usize];
        let _read_bytes = self.store_fd.read_at(&mut buf, start_byte)?;
        Ok(buf)
    }

    fn write_extent(&self, ex: &Extent, data: Vec<u8>) -> Result<Vec<u8>, Errno> {
        let start_byte = ex.0 * self.meta.superblock.block_size as u64;
        let extent_size_bytes = ex.1 * self.meta.superblock.block_size as u64;
        self.store_fd.write_at(&data[..], start_byte)?;
        // If there are more bytes in the data byte vector, return the remaining
        // bytes for further processing. Otherwise, we've written all of our data
        // and we return an empty byte vector signaling completion.
        if data.len() > extent_size_bytes as usize {
            Ok(data[extent_size_bytes as usize..].to_vec())
        } else {
            Ok(vec![])
        }
    }

    // TODO: Generally, we should be able to dynamically increase file sizes
    // when needed, ideally by extending the last extent in the file.
    // NOTE: None of this stuff is thread-safe (for now). Concurrent writes to
    // a file are possible, and data may be corrupted.
    // fn write_file(&self, inode_attr: InodeAttributes, data: &Vec<u8>, offset: u64) -> Result<u64, Errno> {

    // }

    // fn read_file(&self, inode_attr: InodeAttributes, offset: u64, size: u64) -> Result<Vec<u8>, Errno> {}


    fn read_directory(&self, inode: INodeNo) -> Result<DirectoryEntries, Errno> {
        // Get inode attributes from inode table
        let attr = match self.get_inode_attr(inode) {
            Ok(i) => i,
            Err(e) => return Err(e),
        };
        // Check that inode is actually a directory
        if attr.kind != FileKind::Directory {
            return Err(Errno::ENOTDIR)
        };
        // Read in and deserialize the entries data structure from disk
        // Loop over array of extents and collect all bytes in directory file.
        let mut dir_bytes: Vec<u8> = Vec::new();
        debug!("current extents: {:?}", attr.extent_index);
        for ex in &attr.extent_index {
            let read_b = match self.read_extent(ex) {
                Ok(r) => r,
                Err(e) => return Err(e),
            };
            dir_bytes.extend(read_b)
        };
        // NOTE: This should just be done by (de)serializing to JSON using the
        // default serde serializer. We will try to implement/add in a more
        // efficient serialization procedure later on.
        // NOTE: Use the size of the file to only read appropriate bytes.
        println!("{}", attr.size);
        let entries = match serde_json::from_slice(&dir_bytes[..(attr.size as usize)]) {
            Ok(e) => e,
            Err(_) => return Err(Errno::EINVAL),
        };
        // Update inode attributes (last accessed time)
        let new_attr = InodeAttributes {
            last_accessed: time_now(),
            ..attr
        };
        self.set_inode_attr(inode, new_attr);
        return Ok(entries)
    }

    // Completely replace existing directory entries with new entries.
    fn write_directory(&self, inode: INodeNo, entries: &DirectoryEntries) -> Result<(), Errno> {
        // Get inode attributes from inode table
        let attr = match self.get_inode_attr(inode) {
            Ok(i) => i,
            Err(e) => return Err(e),
        };
        // Check that inode is actually a directory
        if attr.kind != FileKind::Directory {
            return Err(Errno::ENOTDIR)
        };
        // Serialize entries into bytes
        let mut b_entries = serde_json::to_vec(entries).unwrap();
        // NOTE: This is some cursed imperative code. But this whole project is
        // cursed now, so who cares.
        // Write pieces of serialized entries into block extents
        debug!("original extents: {:?}", attr.extent_index);
        for ex in &attr.extent_index {
            b_entries = match self.write_extent(ex, b_entries) {
                Ok(remaining_b) => remaining_b,
                Err(e) => return Err(e),
            };
            if b_entries.len() == 0 {
                let new_attr = InodeAttributes {
                    size: b_entries.len() as u64,
                    last_modified: time_now(),
                    last_metadata_changed: time_now(),
                    ..attr
                };
                self.set_inode_attr(inode, new_attr);
                return Ok(())
            }
        }
        // If we get here, there must be more bytes to write and we need to
        // add some more extents
        let new_extents = match self.allocate_blocks(b_entries.len()) {
            Some(exs) => exs,
            None => return Err(Errno::ENOSPC),
        };
        let mut all_extents = attr.extent_index.clone();
        all_extents.extend(new_extents);
        // TODO: The problem is here: we are not actually allocating extents
        // properly. (brief update: yeah, the extent allocation code is wrong
        // and is not properly constructing extents.)
        debug!("all extents: {:?}", all_extents);
        let new_attr = InodeAttributes {
            size: b_entries.len() as u64,
            last_modified: time_now(),
            last_metadata_changed: time_now(),
            extent_index: all_extents,
            ..attr
        };
        self.set_inode_attr(inode, new_attr);
        return Ok(())
    }
}

// Implement the Filesystem trait to integrate FuseFS with fuser.
impl Filesystem for FuseFS {
    fn init(&mut self,
            _req: &Request,
            _config: &mut fuser::KernelConfig,
    ) -> io::Result<()> {
        info!("Filesystem mounted successfully");
        // Set up root directory as inode 0. Let's see if this causes any
        // problems --- see note below.
        // NOTE: It looks like POSIX filesystems reserve inode 0 for other
        // purposes, such as marking deleted directory entries. Very weird.
        // https://utcc.utoronto.ca/~cks/space/blog/unix/POSIXAllowsZeroInode
        // https://news.ycombinator.com/item?id=44142955
        let root_inode: u64 = INodeNo::ROOT.0;
        // Allocate first inode for root
        // TODO: If root is 1, this causes other problems for inode allocation above.
        self.meta.inode_bitmap[0].set(root_inode as usize, true);
        let root_inode_attr = InodeAttributes {
            inode: root_inode,
            open_file_handles: 0,
            size: 0,
            kind: FileKind::Directory,
            last_accessed: time_now(),
            last_modified: time_now(),
            last_metadata_changed: time_now(),
            mode: 0o777,
            hardlinks: 2,
            uid: 0,
            gid: 0,
            extent_index: Vec::new(),
        };
        self.set_inode_attr(INodeNo::ROOT, root_inode_attr);
        let mut entries = BTreeMap::new();
        entries.insert(".".to_string(), (root_inode, FileKind::Directory));
        // Just unwrap this --- this should never return an error, and if it
        // does, we should probably panic anyway.
        self.write_directory(INodeNo(root_inode), &entries).unwrap();
        Ok(())
    }

    fn destroy(&mut self) {
        // Flush metadata to disk
        self.flush_meta();
        // fsync both metadata and block data
        // NOTE: We just ignore if there's an error here for now; I guess we
        // could make an error here more explicit, but there's also not much
        // to do even if there is an error.
        let _ = self.meta_fd.sync_all();
        let _ = self.store_fd.sync_all();
    }

    fn getattr(&self,
               _req: &Request,
               ino: INodeNo,
               _fh: Option<FileHandle>,
               reply: fuser::ReplyAttr,
    ) {
        // Look up the inode attributes in the inode table
        let inode = match self.get_inode_attr(ino) {
            Ok(i) => i,
            Err(e) => {
                reply.error(e);
                return;
            }
        };
        // Convert InodeAttributes to fuser::FileAttr
        let attrs = fuser::FileAttr {
            ino,
            size: inode.size,
            blocks: inode.size.div_ceil(u64::from(self.meta.superblock.block_size as u64)),
            atime: system_time_from_time(inode.last_accessed.0, inode.last_accessed.1),
            mtime: system_time_from_time(inode.last_modified.0, inode.last_modified.1),
            ctime: system_time_from_time(inode.last_metadata_changed.0, inode.last_metadata_changed.1),
            crtime: std::time::UNIX_EPOCH,
            kind: inode.kind.into(),
            perm: inode.mode,
            nlink: inode.hardlinks,
            uid: inode.uid,
            gid: inode.gid,
            rdev: 0,
            blksize: self.meta.superblock.block_size,
            flags: 0,
        };
        // Return attributes in the appropriate way
        reply.attr(&std::time::Duration::from_secs(1), &attrs);
    }

    // TODO: Get more of this stuff working. Also, implement static sizing of
    // the backing file in new(). Also, serialization with postcard (once you
    // test things with JSON).
    fn readdir(&self, _req: &Request,
               ino: INodeNo,
               _fh: FileHandle,
               offset: u64,
               mut reply: fuser::ReplyDirectory
    ) {
        debug!("readdir() called with {ino:?}");
        let entries = match self.read_directory(ino) {
            Ok(entries) => entries,
            Err(error_code) => {
                reply.error(error_code);
                return;
            }
        };

        for (index, entry) in entries.iter().skip(offset as usize).enumerate() {
            let (name, (inode, file_type)) = entry;
            let buffer_full: bool = reply.add(
                INodeNo(*inode),
                offset + index as u64 + 1,
                (*file_type).into(),
                OsStr::new(name),
            );

            if buffer_full {
                break;
            }
        }

        reply.ok();
    }

    // Adding lookup, need lookup before implementing create
    // fn lookup(&self, _req: &Request, parent: INodeNo, _name: &OsStr, reply: fuser::ReplyEntry) {
    //     // For now only handle lookups in the root directory
    //     if parent.0 != 1 {
    //         reply.error(Errno::ENOENT);
    //         return;
    //     }

    //     // Search the inode table for a matching name, still need work
    //     for inode in &self.inode_table {
    //         if inode.hardlinks > 0 {

    //         }
    //     }

    //     // No file found
    //     reply.error(Errno::ENOENT);
    // }

    // //Adding create
    // fn create(&self, _req: &Request, parent: INodeNo, name: &OsStr,
    //       _mode: u32, _umask: u32, _flags: i32, reply: fuser::ReplyCreate) {

    //     // Only support creating files in root directory for now
    //     if parent.0 != 1 {
    //         reply.error(Errno::ENOENT);
    //         return;
    //     }

    //     let now = std::time::SystemTime::now()
    //         .duration_since(std::time::UNIX_EPOCH)
    //         .unwrap();

    //     // Hardcode inode 2 for now just to verify create is working
    //     let attrs = fuser::FileAttr {
    //         ino: INodeNo(2),
    //         size: 0,
    //         blocks: 0,
    //         atime: std::time::UNIX_EPOCH + std::time::Duration::new(now.as_secs(), now.subsec_nanos()),
    //         mtime: std::time::UNIX_EPOCH + std::time::Duration::new(now.as_secs(), now.subsec_nanos()),
    //         ctime: std::time::UNIX_EPOCH + std::time::Duration::new(now.as_secs(), now.subsec_nanos()),
    //         crtime: std::time::UNIX_EPOCH,
    //         kind: fuser::FileType::RegularFile,
    //         perm: 0o644,
    //         nlink: 1,
    //         uid: _req.uid(),
    //         gid: _req.gid(),
    //         rdev: 0,
    //         blksize: self.superblock.block_size,
    //         flags: 0,
    //     };

    //     self.dir_entries
    //         .write()
    //         .unwrap()
    //         .entry(parent.0)
    //         .or_insert_with(Vec::new)
    //         .push((2, name.to_string_lossy().to_string()));

    //     info!("Created file {:?}", name);
    //     reply.created(
    //         &std::time::Duration::from_secs(1),
    //         &attrs,
    //         fuser::Generation(0),
    //         fuser::FileHandle(0),
    //         fuser::FopenFlags::empty(),
    //     );
    // }

    // // Adding read and write, still needs much work
    // fn read(&self, _req: &Request, ino: INodeNo, _fh: FileHandle, offset: u64,
    //         size: u32, _flags: OpenFlags, _lock: Option<LockOwner>, reply: ReplyData) {
    //     // check if inode really exist
    //     if self.meta.inode_bitmap.get(ino.0 as usize).is_none() {
    //         // If the bit is 0, file doesn't exist
    //         reply.error(Errno::ENOENT);
    //         return;
    //     }

    //     // calculate start location, start from superblock and jump slots
    //     let file_base_address = self.superblock.data_start + (ino.0 * self.superblock.block_size as u64);

    //     // create buffer, value starts at 0, type is u8
    //     let mut buffer = vec![0u8; size as usize];

    //     // Adding file to buffer at exact offset address, check if read is
    //     // successful and return data, otherwise just return error
    //     match self.block_store_fd.read_at(&mut buffer, file_base_address + offset as u64) {
    //         Ok(bytes_read) => reply.data(&buffer[..bytes_read]),
    //         Err(_) => reply.error(Errno::EIO),
    //     }
    // }
}

#[derive(Parser)]
#[command(version, author = "Carlos Anguiano, Lucas Du, Simon Zheng")]
struct Args {
    /// Act as a client, and mount FUSE at given path
    mount_point: PathBuf,

    /// Declares the name of the directory holding the metadata file and the
    /// backing file that simulates "block" storage
    fs_dir: PathBuf,

    /// Mount FUSE with direct IO
    #[arg(long, requires = "mount_point")]
    direct_io: bool,

    /// Automatically unmount FUSE when process exits
    #[arg(long)]
    auto_unmount: bool,

    /// Enable setuid support when run as root
    #[arg(long)]
    suid: bool,

    #[arg(long, default_value_t = 1)]
    n_threads: usize,

    /// Sets the level of verbosity
    #[arg(short, action = clap::ArgAction::Count)]
    v: u8,

    #[arg(long, default_value_t = 4096, value_parser = valid_block_size)]
    block_size: u32,

    #[arg(long, default_value_t = 32768, value_parser = valid_bitmap_size)]
    num_inodes: u64,
    #[arg(long, default_value_t = 32768, value_parser = valid_bitmap_size)]
    num_blocks: u64,
}

fn valid_block_size(s: &str) -> Result<u32, String> {
    let bl_size: usize = s.parse().map_err(|_| format!("`{s}` is not a number"))?;
    let multiple_of = 1024;
    if bl_size % multiple_of == 0 {
        Ok(bl_size as u32)
    } else {
        Err(format!("`{s}` must be a multiple of {}", multiple_of))
    }
}

fn valid_bitmap_size(s: &str) -> Result<u64, String> {
    let bm_size: usize = s.parse().map_err(|_| format!("`{s}` is not a number"))?;
    let multiple_of = 1024;
    if bm_size % multiple_of == 0 {
        Ok(bm_size as u64)
    } else {
        Err(format!("`{s}` must be a multiple of {}", multiple_of))
    }
}

// Reads directly from /etc/fuse.conf file for additional configuration
// regarding mount policy. Currently, reads the user_allow_other option (if
// present), which allows non-root users to specify allow_other or allow_root
// mount options.
fn fuse_allow_other_enabled() -> io::Result<bool> {
    let file = File::open("/etc/fuse.conf")?;
    for line in BufReader::new(file).lines() {
        if line?.trim_start().starts_with("user_allow_other") {
            return Ok(true);
        }
    }
    Ok(false)
}

fn main() {
    // Initialize clap parser for Args struct.
    let args = Args::parse();

    // Set up default logging framework.
    let log_level = match args.v {
        0 => LevelFilter::Error,
        1 => LevelFilter::Warn,
        2 => LevelFilter::Info,
        3 => LevelFilter::Debug,
        _ => LevelFilter::Trace,
    };
    env_logger::builder()
        .format_timestamp_nanos()
        .filter_level(log_level)
        .init();

    // Initialize Config struct (from fuser) for FUSE configuration knobs.
    let mut cfg = Config::default();
    cfg.mount_options = vec![MountOption::FSName("fuser".to_string())];

    // Handle CLI arguments.
    // NOTE: Many of these CLI arguments are used to push values into Config
    // mount_options Vec. The Config struct is then directly used in the call
    // to fuser::mount.
    if args.suid {
        info!("setuid bit support enabled");
        cfg.mount_options.push(MountOption::Suid);
    }

    if args.auto_unmount {
        cfg.mount_options.push(MountOption::AutoUnmount);
    }

    // We require that user_allow_other is set in /etc/fuse.conf. This, I
    // believe, is necessary for a non-root user to actually mount and use the
    // FUSE filesystem.
    if let Ok(enabled) = fuse_allow_other_enabled() {
        if enabled {
            cfg.acl = SessionACL::All;
        }
    } else {
        eprintln!("Unable to read /etc/fuse.conf");
    }

    if cfg.mount_options.contains(&MountOption::AutoUnmount) &&
        cfg.acl != SessionACL::RootAndOwner
    {
        cfg.acl = SessionACL::All;
    }

    cfg.n_threads = Some(args.n_threads);

    let block_size = args.block_size;
    let num_inodes = args.num_inodes;
    let num_blocks = args.num_blocks;

    // Check if the backing filestore exists. If it does, attempt to load the
    // existing filesystem. Otherwise, create a new file as the backing store.
    let fs = match FuseFS::new(args.fs_dir, block_size, num_inodes, num_blocks) {
        Ok(f) => f,
        Err(e) => panic!("Failed to create filesystem: {}", e),
    };

    // Mount filesystem at declared mount point.
    let result = fuser::mount2(fs, &args.mount_point, &cfg);
    if let Err(e) = result {
        // Return a special error code for permission denied, which usually
        // indicates that "user_allow_other" is missing from /etc/fuse.conf
        if e.kind() == ErrorKind::PermissionDenied {
            error!("{e}");
            std::process::exit(2);
        } else {
            error!("{e}");
        }
    }
}
