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
use fuser::TimeOrNow;
use fuser::TimeOrNow::Now;

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

// Small helper to convert standard Linux "mode" integers into the filetypes
// they represent, at least for the 3 filetypes we implement.
// NOTE: Why do all these lower-level systems use all these magic numbers...we
// should be able to pass real types around lol.
fn as_file_kind(mut mode: u32) -> FileKind {
    mode &= libc::S_IFMT as u32;

    if mode == libc::S_IFREG as u32 {
        return FileKind::File;
    } else if mode == libc::S_IFLNK as u32 {
        return FileKind::Symlink;
    } else if mode == libc::S_IFDIR as u32 {
        return FileKind::Directory;
    }
    unimplemented!("{mode}");
}

// Helper to calculate the GID of a created file under a parent directory.
fn creation_gid(parent: &InodeAttributes, gid: u32) -> u32 {
    if parent.mode & libc::S_ISGID as u16 != 0 {
        return parent.gid;
    }
    gid
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
            inode_bitmap: RwLock::new(inode_bitmap.clone()),
            data_bitmap: RwLock::new(data_bitmap.clone()),
            inode_table: RwLock::new(self.inode_table.clone()),
        }
    }
}

const BITMAP_CHUNK_BITS: usize  = 1024;

struct Meta {
    superblock: Superblock,
    inode_bitmap: RwLock<Vec<Bitmap<BITMAP_CHUNK_BITS>>>,
    data_bitmap: RwLock<Vec<Bitmap<BITMAP_CHUNK_BITS>>>,
    inode_table: RwLock<InodeTable>,
}

impl Meta {
    fn to_meta_serializable(&self) -> MetaSerializable {
        let mut inode_bmap_bool: Vec<bool> = Vec::new();
        let inode_bitmap_binding = self.inode_bitmap.read().unwrap();
        for chunk in inode_bitmap_binding.iter() {
            for i in 0..BITMAP_CHUNK_BITS {
                if chunk.get(usize::try_from(i).unwrap()) {
                    inode_bmap_bool.push(true)
                } else {
                    inode_bmap_bool.push(false)
                }
            }
        };
        let mut data_bmap_bool: Vec<bool> = Vec::new();
        let data_bitmap_binding = self.data_bitmap.read().unwrap();
        for chunk in data_bitmap_binding.iter() {
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
                inode_bitmap: RwLock::new(
                    vec![Bitmap::<BITMAP_CHUNK_BITS>::new(); num_inodes as usize / BITMAP_CHUNK_BITS]),
                data_bitmap: RwLock::new(
                    vec![Bitmap::<BITMAP_CHUNK_BITS>::new(); num_blocks as usize / BITMAP_CHUNK_BITS]),
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
        let store_fd = OpenOptions::new()
            .create(true)
            .write(true)
            .read(true)
            .open(store_file_path)?;
        // Statically allocate the maximum size for the filesystem:
        // num_blocks * block_size.
        let max_size = num_inodes * block_size as u64;
        store_fd.set_len(max_size)?;

        info!("Created filesystem.");
        Ok(FuseFS {
            // fs_dir: fs_dir_path,
            meta,
            meta_fd,
            store_fd,
        })
    }

    // Calculate creation mode from some u32 mode (NOTE: I actually don't know
    // what this does exactly, but it's taken from the fuser examples simple.rs
    // program and seems to work).
    fn creation_mode(&self, mode: u32) -> u16 {
        (mode & !(libc::S_ISUID | libc::S_ISGID) as u32) as u16
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
        let mut inode_bitmap_binding = self.meta.inode_bitmap.write().unwrap();
        for (chunk_idx, chunk) in inode_bitmap_binding.iter().enumerate() {
            // check each bit until we find a free space, skipping index 0
            // NOTE: inode 0 is just not allowed in Linux filesystems it seems,
            // so avoid ever allocating it and just keep it false/empty in the
            // bitmap. inode 1 is reserved as the root inode. This should not
            // be a problem in practice because it is allocated on init and
            // never deallocated, but we also skip it here for completeness.
            match chunk.next_false_index(1) {
                Some(i) => {
                    inode_bitmap_binding[chunk_idx].set(i, true);
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
        let mut inode_bitmap_binding = self.meta.inode_bitmap.write().unwrap();
        inode_bitmap_binding[chunk_idx].set(bit_idx, false);
    }

    fn set_inode_attr(&self, inode: INodeNo, attr: InodeAttributes) {
        self.meta.inode_table.write().unwrap()[inode.0 as usize] = attr;
    }

    fn get_inode_attr(&self, inode: INodeNo) -> Result<InodeAttributes, Errno> {
        let chunk = inode.0 as usize / BITMAP_CHUNK_BITS;
        let bit = inode.0 as usize % BITMAP_CHUNK_BITS;
        // Check if inode is actually allocated in the bitmap
        match self.meta.inode_bitmap.read().unwrap().get(chunk) {
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

    // Number of blocks allocated for file
    fn blocks_allocated(&self, size: u64) -> u64 {
        return size.div_ceil(u64::from(self.meta.superblock.block_size as u64))
    }

    // Helper to convert InodeAttributes to FileAttr
    fn inode_attr_to_file_attr(&self, i: &InodeAttributes) -> fuser::FileAttr {
        fuser::FileAttr {
            ino: INodeNo(i.inode),
            size: i.size,
            blocks: i.size.div_ceil(u64::from(self.meta.superblock.block_size as u64)),
            atime: system_time_from_time(i.last_accessed.0, i.last_accessed.1),
            mtime: system_time_from_time(i.last_modified.0, i.last_modified.1),
            ctime: system_time_from_time(i.last_metadata_changed.0, i.last_metadata_changed.1),
            crtime: std::time::UNIX_EPOCH,
            kind: i.kind.into(),
            perm: i.mode,
            nlink: i.hardlinks,
            uid: i.uid,
            gid: i.gid,
            rdev: 0,
            blksize: self.meta.superblock.block_size,
            flags: 0,
        }
    }

    // If space is available of the desired size, allocate that space by setting
    // all corresponding data bitmap bits and return the associatd extent(s).
    // Otherwise, return None.
    // NOTE (side-effect): This function may change data bitmap state.
    fn allocate_blocks(&self, size: usize) -> Option<Vec<Extent>> {
        // Return immediately with empty extent vector if size is 0
        if size == 0 {
            return Some(vec![])
        };
        let mut num_blocks_needed = size / (self.meta.superblock.block_size as usize) + 1;
        let mut extents: Vec<Extent> = Vec::new();
        // Find list of extents with total desired size
        let mut data_bitmap_binding = self.meta.data_bitmap.write().unwrap();
        for (chunk_idx, chunk) in data_bitmap_binding.iter().enumerate() {
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
                    // NOTE: At this point, we know that we have found enough
                    // space for the full allocation.
                    ex_size += num_blocks_needed;
                    extents.push(((base_offset + ex_open) as u64, ex_size as u64));
                    // Only actually allocate/set inode bits when we definitely
                    // have enough space for the full allocation.
                    for ex in &extents {
                        let open = ex.0;
                        let size = ex.1;
                        let chunk_idx = open as usize / BITMAP_CHUNK_BITS;
                        let bit_offset_open = open as usize % BITMAP_CHUNK_BITS;
                        for i in bit_offset_open..(bit_offset_open + size as usize) {
                            data_bitmap_binding[chunk_idx].set(i as usize, true);
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

    // NOTE: None of this stuff is thread-safe (for now). Concurrent writes to
    // a file are possible, and data may be corrupted.
    fn write_file(&self, inode: INodeNo, data: &Vec<u8>, offset: u64) -> Result<u64, Errno> {
        // Get inode attributes from inode table
        let attr = match self.get_inode_attr(inode) {
            Ok(i) => i,
            Err(e) => return Err(e),
        };

        // Disallow the write if the offset is beyond the size of the file
        if offset > attr.size {
            return Err(Errno::EINVAL)
        };

        // Compute mapping of offset and size to specific slices of file extents
        // Compute index of block (within the file) that the offset starts in
        let mut remaining_blocks_to_offset = offset / self.meta.superblock.block_size as u64;
        // Compute byte index in the block (calculated above) that the offset starts at
        let byte_in_offset_block = offset % self.meta.superblock.block_size as u64;
        let mut bytes_written = 0;
        // Compute mapping of offset and size of the data to specific slices
        // file extents; we may need to add extents to the file.
        // Write data to the slices of the extents
        // TODO: Update metadata in this loop before returning something.
        // TODO: Do we really need to clone here? Figure this out next, seems
        // extremely wasteful to do this.
        let mut remaining_bytes_to_write = data.clone();
        for ex in &attr.extent_index {
            // TODO: There might be an off-by-one error here, so watch out.
            // Ideally write some tests for this, but time may not allow.
            let ex_size = ex.1;
            if remaining_blocks_to_offset != 0 && ex_size > remaining_blocks_to_offset {
                // In this case, we know that our offset is in this extent
                let offset_block_i = ex.0 + remaining_blocks_to_offset;
                let offset_byte_i =
                    offset_block_i * self.meta.superblock.block_size as u64
                    + byte_in_offset_block;
                let bytes_left_in_extent =
                    (ex_size - remaining_blocks_to_offset) * self.meta.superblock.block_size as u64
                    - byte_in_offset_block;
                // Set remaining blocks until offset to 0, since we've now found
                // the offset block
                remaining_blocks_to_offset = 0;
                // Check if we can complete the write in this extent
                if bytes_left_in_extent >= remaining_bytes_to_write.len() as u64 {
                    // We can complete the write in this extent
                    self.store_fd.write_at(&mut remaining_bytes_to_write[..], offset_byte_i);
                    remaining_bytes_to_write = remaining_bytes_to_write[bytes_left_in_extent as usize..].to_vec();
                    // Increase number of bytes written and break from loop
                    bytes_written += remaining_bytes_to_write.len();
                    break
                } else {
                    // Cannot complete write in this extent
                    self.store_fd.write_at(
                        &mut remaining_bytes_to_write[..bytes_left_in_extent as usize],
                        offset_byte_i);
                    // NOTE: What is this conversion hack...why do I need to the to_vec()?
                    // I presume I'm not understanding something about slices vs. vectors.
                    remaining_bytes_to_write = &remaining_bytes_to_write[bytes_left_in_extent as usize..].to_vec();
                    // Increase number of bytes written
                    bytes_written += bytes_left_in_extent as usize;
                    // And then continue the loop over extents
                }
            } else if remaining_blocks_to_offset != 0 {
                // In this case, we've already passed the offset block and are
                // in the write section
                let ex_start_byte_i = ex.0 * self.meta.superblock.block_size as u64;
                let bytes_in_extent = ex_size * self.meta.superblock.block_size as u64;
                if bytes_in_extent >= remaining_bytes_to_write.len() as u64 {
                    // We can complete the write in this extent
                    self.store_fd.write_at(&mut remaining_bytes_to_write[..], ex_start_byte_i);
                    remaining_bytes_to_write = &remaining_bytes_to_write[bytes_in_extent as usize..].to_vec();
                    // Increase number of bytes written and break from loop
                    bytes_written += remaining_bytes_to_write.len();
                    break
                } else {
                    // Cannot complete write in this extent
                    self.store_fd.write_at(
                        &mut remaining_bytes_to_write[..bytes_in_extent as usize],
                        ex_start_byte_i);
                    remaining_bytes_to_write = &remaining_bytes_to_write[bytes_in_extent as usize..].to_vec();
                    // Increase number of bytes written
                    bytes_written += bytes_in_extent as usize;
                    // And then continue the loop over extents
                }
            } else {
                // In this case, we haven't yet found the offset block and we
                // need to continue looking for it before we can start reading
                // NOTE: At this point, we know that ex_size <= remaining_blocks_to_offset
                // so we can mark this extent as covered --- decreasing the
                // remaining blocks counter by its size (in blocks) --- and
                // continuing with the loop.
                remaining_blocks_to_offset -= ex_size;
            }
        };
        if remaining_bytes_to_write.len() == 0 {
            // TODO: Should we return bytes written here or just return () like write_dir?
            return Ok(bytes_written as u64)
        };
        // If we still have bytes to write, we'll need to add more extents
        let extra_bytes_needed = remaining_bytes_to_write.len();
        let new_extents = match self.allocate_blocks(extra_bytes_needed) {
            Some(exs) => exs,
            None => return Err(Errno::ENOSPC),
        };
        for ex in &new_extents {
            // NOTE: This is the same code as in the second if-block earlier.
            let ex_size = ex.1;
            let ex_start_byte_i = ex.0 * self.meta.superblock.block_size as u64;
            let bytes_in_extent = ex_size * self.meta.superblock.block_size as u64;
            if bytes_in_extent >= remaining_bytes_to_write.len() as u64 {
                // We can complete the write in this extent
                self.store_fd.write_at(&mut remaining_bytes_to_write[..], ex_start_byte_i);
                remaining_bytes_to_write = &remaining_bytes_to_write[bytes_in_extent as usize..].to_vec();
                // Increase number of bytes written and break from loop
                bytes_written += remaining_bytes_to_write.len();
                break
            } else {
                // Cannot complete write in this extent
                self.store_fd.write_at(
                    &mut remaining_bytes_to_write[..bytes_in_extent as usize],
                    ex_start_byte_i);
                remaining_bytes_to_write = &remaining_bytes_to_write[bytes_in_extent as usize..].to_vec();
                // Increase number of bytes written
                bytes_written += bytes_in_extent as usize;
                // And then continue the loop over extents
            }
        }
        // Merge old extents with new extents and add to attributes
        let mut all_extents = attr.extent_index.clone();
        all_extents.extend(new_extents);

        // Update metadata and return
        let new_attr = InodeAttributes {
            size: attr.size + extra_bytes_needed as u64,
            last_modified: time_now(),
            last_metadata_changed: time_now(),
            extent_index: all_extents,
            ..attr
        };
        self.set_inode_attr(inode, new_attr);
        return Ok(bytes_written as u64)
    }

    fn read_file(&self, inode: INodeNo, offset: u64, size: u32) -> Result<Vec<u8>, Errno> {
        // Get inode attributes from inode table
        let attr = match self.get_inode_attr(inode) {
            Ok(i) => i,
            Err(e) => return Err(e),
        };
        // Check if the read will go beyond the size of the file
        if offset + size as u64 > attr.size {
            return Err(Errno::EINVAL)
        };

        // Compute mapping of offset and size to specific slices of file extents
        // Compute index of block (within the file) that the offset starts in
        let mut remaining_blocks_to_offset = offset / self.meta.superblock.block_size as u64;
        // Compute byte index in the block (calculated above) that the offset starts at
        let byte_in_offset_block = offset % self.meta.superblock.block_size as u64;

        // Store and track read data bytes
        let mut data_bytes: Vec<u8> = Vec::new();
        let mut remaining_bytes = size as u64;
        // Loop through extents and read slices from extents that are in the
        // section of the file to be read
        for ex in &attr.extent_index {
            // TODO: There might be an off-by-one error here, so watch out.
            // Ideally write some tests for this, but time may not allow.
            let ex_size = ex.1;
            if remaining_blocks_to_offset != 0 && ex_size > remaining_blocks_to_offset {
                // In this case, we know that our offset is in this extent
                let offset_block_i = ex.0 + remaining_blocks_to_offset;
                let offset_byte_i =
                    offset_block_i * self.meta.superblock.block_size as u64
                    + byte_in_offset_block;
                let bytes_left_in_extent =
                    (ex_size - remaining_blocks_to_offset) * self.meta.superblock.block_size as u64
                    - byte_in_offset_block;
                if bytes_left_in_extent >= remaining_bytes {
                    // We can complete the read in this extent
                    let mut buf = vec![0u8; remaining_bytes as usize];
                    self.store_fd.read_at(&mut buf, offset_byte_i);
                    data_bytes.extend(&buf);
                    break
                } else {
                    let mut buf = vec![0u8; bytes_left_in_extent as usize];
                    self.store_fd.read_at(&mut buf, offset_byte_i);
                    data_bytes.extend(&buf);
                    // Decrease remaining bytes
                    remaining_bytes -= bytes_left_in_extent;
                    // And then continue the loop over extents
                }
            } else if remaining_blocks_to_offset != 0 {
                // In this case, we've already passed the offset block and are
                // on our way in the section to be read
                let ex_start_byte_i = ex.0 * self.meta.superblock.block_size as u64;
                let bytes_in_extent = ex_size * self.meta.superblock.block_size as u64;
                if bytes_in_extent >= remaining_bytes {
                    // We can complete the read in this extent
                    let mut buf = vec![0u8; remaining_bytes as usize];
                    self.store_fd.read_at(&mut buf, ex_start_byte_i);
                    data_bytes.extend(&buf);
                    break
                } else {
                    let mut buf = vec![0u8; bytes_in_extent as usize];
                    self.store_fd.read_at(&mut buf, ex_start_byte_i);
                    data_bytes.extend(&buf);
                    // Decrease remaining bytes
                    remaining_bytes -= bytes_in_extent;
                    // And then continue the loop over extents
                }
            } else {
                // In this case, we haven't yet found the offset block and we
                // need to continue looking for it before we can start reading
                // NOTE: At this point, we know that ex_size <= remaining_blocks_to_offset
                // so we can mark this extent as covered --- decreasing the
                // remaining blocks counter by its size (in blocks) --- and
                // continuing with the loop.
                remaining_blocks_to_offset -= ex_size;
            }
        };
        // Update metadata and return
        // Update inode attributes (last accessed time)
        let new_attr = InodeAttributes {
            last_accessed: time_now(),
            ..attr
        };
        self.set_inode_attr(inode, new_attr);
        Ok(data_bytes)
    }

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
        // debug!("current extents: {:?}", attr.extent_index);
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
        // println!("{}", attr.size);
        // debug!("serialized entries in bytes: {:?}", &dir_bytes[..(attr.size) as usize]);
        let entries = match serde_json::from_slice(&dir_bytes[..(attr.size as usize)]) {
            Ok(e) => e,
            // TODO: Serialization here is not working as expected, your serde_json
            // is not able to successfully deserialize from slice. Need to fix
            // this, or just give up on serialization.
            Err(_) => return Err(Errno::EINVAL),
        };
        // debug!("entries: {:?}", entries);
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
        // Save full size of serialized byte vector before we consume it below.
        let full_size = b_entries.len() as u64;
        // debug!("original entries in bytes: {:?}", b_entries);
        // NOTE: This is some cursed imperative code. But this whole project is
        // cursed now, so who cares.
        // Write pieces of serialized entries into block extents
        // debug!("original extents: {:?}", attr.extent_index);
        // TODO: Should we be updating inode attributes here? It seems like we
        // can also do this later in the Filesystem implementation, but it's
        // worth being consistent. Seems like we should. We should remove any
        // further updates later on when we call this.
        for ex in &attr.extent_index {
            b_entries = match self.write_extent(ex, b_entries) {
                Ok(remaining_b) => remaining_b,
                Err(e) => return Err(e),
            };
            // debug!("remaining bytes: {:?}", b_entries);
            if b_entries.len() == 0 {
                let new_attr = InodeAttributes {
                    size: full_size,
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
        // Write remaining data to new extents
        // TODO: There must be some way to duplicate and simplify some of this
        // code.
        for ex in &new_extents {
            b_entries = match self.write_extent(ex, b_entries) {
                Ok(remaining_b) => remaining_b,
                Err(e) => return Err(e),
            };
            // debug!("remaining bytes: {:?}", b_entries);
            if b_entries.len() == 0 {
                break
            }
        }
        // Merge old extents with new extents and add to attributes
        let mut all_extents = attr.extent_index.clone();
        all_extents.extend(new_extents);
        // debug!("all extents: {:?}", all_extents);
        let new_attr = InodeAttributes {
            size: full_size,
            last_modified: time_now(),
            last_metadata_changed: time_now(),
            extent_index: all_extents,
            ..attr
        };
        self.set_inode_attr(inode, new_attr);
        return Ok(())
    }

    fn lookup_name(&self, parent: INodeNo, name: &OsStr) -> Result<InodeAttributes, Errno> {
        let entries = self.read_directory(parent)?;
        let name_string = match name.to_str() {
            Some(s) => s.to_string(),
            None => return Err(Errno::EINVAL),
        };
        if let Some((inode, _)) = entries.get(&name_string) {
            return self.get_inode_attr(INodeNo(*inode));
        }
        return Err(Errno::ENOENT);
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
        self.meta.inode_bitmap.write().unwrap()[0].set(root_inode as usize, true);
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
        let attrs = self.inode_attr_to_file_attr(&inode);
        // Return attributes in the appropriate way
        reply.attr(&std::time::Duration::from_secs(1), &attrs);
    }

    fn setattr(
        &self,
        _req: &Request,
        ino: INodeNo,
        mode: Option<u32>,
        uid: Option<u32>,
        gid: Option<u32>,
        size: Option<u64>,
        _atime: Option<TimeOrNow>,
        _mtime: Option<TimeOrNow>,
        _ctime: Option<SystemTime>,
        fh: Option<FileHandle>,
        _crtime: Option<SystemTime>,
        _chgtime: Option<SystemTime>,
        _bkuptime: Option<SystemTime>,
        _flags: Option<fuser::BsdFileFlags>,
        reply: fuser::ReplyAttr,
    ) {
        // NOTE: What if we just made this a noop for now?
        let inode = match self.get_inode_attr(ino) {
            Ok(i) => i,
            Err(e) => {
                reply.error(e);
                return;
            }
        };
        let attrs = self.inode_attr_to_file_attr(&inode);
        reply.attr(&Duration::from_secs(1), &attrs)
        // TODO: Consider actually implementing some of this stuff, i.e. for
        // chmod, chown, and whatever else uses this function.
    }

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
        // debug!("entries: {:?}", entries);

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

    // Look up a directory entry by name and get its attributes.
    fn lookup(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: fuser::ReplyEntry) {
        // Lookup specific name in parent directory
        let inode = match self.lookup_name(parent, name) {
            Ok(a) => a,
            Err(e) => {
                reply.error(e);
                return
            }
        };

        // Convert InodeAttributes to fuser::FileAttr
        let attrs = self.inode_attr_to_file_attr(&inode);

        // NOTE: I don't really know what the Generation(0) thing is doing.
        reply.entry(&std::time::Duration::from_secs(1), &attrs, fuser::Generation(0));
    }

    fn create(&self,
              req: &Request,
              parent: INodeNo,
              name: &OsStr,
              mode: u32,
              _umask: u32,
              _flags: i32,
              reply: fuser::ReplyCreate
    ) {
        debug!("create() called with {parent:?} {name:?}");
        if self.lookup_name(parent, name).is_ok() {
            reply.error(Errno::EEXIST);
            return;
        }

        // Allocate next free inode
        let ino = match self.allocate_inode() {
            Some(idx) => idx,
            None => return reply.error(Errno::ENOSPC),
        };

        // Update parent inode attributes
        let mut parent_attrs = match self.get_inode_attr(parent) {
            Ok(attrs) => attrs,
            Err(error_code) => {
                reply.error(error_code);
                return;
            }
        };
        parent_attrs.last_modified = time_now();
        parent_attrs.last_metadata_changed = time_now();
        // You can tell things are dark when I'm just cloning everything...
        // One day I'll be better at Rust. I promise. (:
        self.set_inode_attr(parent, parent_attrs.clone());

        // Create new inode attributes
        let inode = InodeAttributes {
            inode: ino.0,
            open_file_handles: 1,
            size: 0,
            last_accessed: time_now(),
            last_modified: time_now(),
            last_metadata_changed: time_now(),
            kind: as_file_kind(mode),
            mode: self.creation_mode(mode),
            hardlinks: 1,
            uid: req.uid(),
            gid: creation_gid(&parent_attrs, req.gid()),
            extent_index: Vec::new(),
        };

        // Generate fuser::FileAttr from InodeAttributes for reply
        let attrs = self.inode_attr_to_file_attr(&inode);
        // Add new inode to inode table
        self.set_inode_attr(ino, inode);

        // If new inode is directory, add . and .. and write entries to disk.
        if as_file_kind(mode) == FileKind::Directory {
            let mut entries: DirectoryEntries = BTreeMap::new();
            entries.insert(".".to_string(), (ino.0, FileKind::Directory));
            entries.insert("..".to_string(), (parent.0, FileKind::Directory));
            match self.write_directory(ino, &entries) {
                Ok(()) => (),
                Err(e) => {
                    reply.error(e);
                    return;
                }
            };
        };

        // Add the specified name to the parent directory
        let name_string = match name.to_str() {
            Some(s) => s.to_string(),
            None => {
                reply.error(Errno::EINVAL);
                return
            },
        };
        let mut parent_entries = match self.read_directory(parent) {
            Ok(es) => es,
            Err(e) => {
                reply.error(e);
                return
            }
        };
        parent_entries.insert(name_string, (ino.0, as_file_kind(mode)));
        match self.write_directory(parent, &parent_entries) {
            Ok(()) => (),
            Err(e) => {
                reply.error(e);
                return;
            }
        };
        debug!("new parent entries {:?}", parent_entries);

        // Return successful completion
        info!("Created file {:?} with {ino:?}", name);
        reply.created(
            &Duration::from_secs(1),
            &attrs,
            fuser::Generation(0),
            // Not really doing anything with FileHandles in this implementation.
            fuser::FileHandle(0),
            fuser::FopenFlags::empty(),
        );
    }

    // // read data
    // fn read(&self, _req: &Request, ino: INodeNo, _fh: FileHandle, offset: u64,
    //     size: u32, _flags: OpenFlags, _lock: Option<LockOwner>, reply: ReplyData) {

    //     // check inode is allocated in bitmap
    //     let chunk = ino.0 as usize / 1024;
    //     let bit = ino.0 as usize % 1024;

    //     let inode_bitmap_binding = self.inode_bitmap.read().unwrap();
    //     let bitmap_chunk = match inode_bitmap_binding.get(chunk) {
    //         Some(slot) => slot,
    //         None => return reply.error(Errno::ENOENT),
    //     };
    //     if !bitmap_chunk.get(bit) {
    //         return reply.error(Errno::ENOENT);
    //     }
    //     drop(inode_bitmap_binding);

    //     // get inode from inode table
    //     let inode_table_binding = self.inode_table.read().unwrap();
    //     let inode = match inode_table_binding.get(ino.0 as usize) {
    //         Some(ino) => ino,
    //         None => return reply.error(Errno::ENOENT),
    //     };

    //     let block_size = self.superblock.block_size as u64;
    //     let mut bytes_read_total = 0u64;
    //     let mut result = vec![0u8; size as usize];

    //     // walk extents to read data
    //     let mut remaining = size as u64;
    //     let mut current_offset = offset;

    //     for (block_idx, _length) in inode.extent_index.iter() {
    //         if remaining == 0 {
    //             break;
    //         }
    //         if *block_idx == 0 {
    //             continue;
    //         }

    //         let block_num = current_offset / block_size;
    //         let block_offset = current_offset % block_size;
    //         let bytes_to_read = remaining.min(block_size - block_offset);

    //         let read_offset = self.offset_from_block_idx(*block_idx) + block_offset;
    //         let buf_start = bytes_read_total as usize;
    //         let buf_end = buf_start + bytes_to_read as usize;

    //         match self.block_store_fd.read_at(&mut result[buf_start..buf_end], read_offset) {
    //             Ok(n) => {
    //                 bytes_read_total += n as u64;
    //                 remaining -= n as u64;
    //                 current_offset += n as u64;
    //             }
    //             Err(_) => return reply.error(Errno::EIO),
    //         }

    //         let _ = block_num; // suppress unused warning
    //     }

    //     reply.data(&result[..bytes_read_total as usize]);
    // }

    // fn write(&self, _req: &Request, ino: INodeNo, _fh: FileHandle, offset: u64,
    //      data: &[u8], _write_flags: WriteFlags, _flags: OpenFlags,
    //      _lock_owner: Option<LockOwner>, reply: fuser::ReplyWrite) {

    //     // get the inode from the inode table
    //     let mut inode_table_binding= self.inode_table.write().unwrap();
    //     let inode = match inode_table_binding.get_mut(ino.0 as usize) {
    //         Some(ino) => ino,
    //         None => return reply.error(Errno::ENOENT),
    //     };

    //     // figure out which block the offset falls into
    //     let block_size = self.superblock.block_size as u64;
    //     let block_num = offset / block_size;
    //     let block_offset = offset % block_size;

    //     // check if we already have a block allocated for this position
    //     let block_idx = if inode.extent_index[block_num as usize].0 != 0 {
    //         // block already allocated, reuse it
    //         inode.extent_index[block_num as usize].0
    //     } else {
    //         // allocate a new block
    //         let new_block = match self.allocate_block() {
    //             Some(b) => b,
    //             None => return reply.error(Errno::ENOSPC),
    //         };
    //         inode.extent_index[block_num as usize] = (new_block, 1);
    //         new_block
    //     };

    //     // calculate the byte offset into the data region
    //     let write_offset = self.offset_from_block_idx(block_idx) + block_offset;

    //     // write data to disk
    //     match self.block_store_fd.write_at(data, write_offset) {
    //         Ok(bytes_written) => {
    //             // update inode size if needed
    //             let new_size = offset + bytes_written as u64;
    //             if new_size > inode.size {
    //                 inode.size = new_size;
    //             }

    //             // persist inode to disk
    //             if let Err(e) = self.write_inode(ino.0, inode) {
    //                 error!("failed to persist inode {}: {}", ino.0, e);
    //                 return reply.error(Errno::EIO);
    //             }

    //             reply.written(bytes_written as u32);
    //         }
    //         Err(_) => reply.error(Errno::EIO),
    //     }
    // }

    // TODO: Paste in simple.rs mkdir for reference.
    // fn mkdir(
    //     &self,
    //     _req: &Request,
    //     parent: INodeNo,
    //     name: &OsStr,
    //     mut mode: u32,
    //     _umask: u32,
    //     reply: ReplyEntry,
    // ) {
    //     debug!("mkdir() called with {parent:?} {name:?} {mode:o}");
    //     if self.lookup_name(parent, name).is_ok() {
    //         reply.error(Errno::EEXIST);
    //         return;
    //     }

    //     let mut parent_attrs = match self.get_inode(parent) {
    //         Ok(attrs) => attrs,
    //         Err(error_code) => {
    //             reply.error(error_code);
    //             return;
    //         }
    //     };

    //     if !check_access(
    //         parent_attrs.uid,
    //         parent_attrs.gid,
    //         parent_attrs.mode,
    //         _req.uid(),
    //         _req.gid(),
    //         AccessFlags::W_OK,
    //     ) {
    //         reply.error(Errno::EACCES);
    //         return;
    //     }
    //     parent_attrs.last_modified = time_now();
    //     parent_attrs.last_metadata_changed = time_now();
    //     self.write_inode(&parent_attrs);

    //     if _req.uid() != 0 {
    //         mode &= !(libc::S_ISUID | libc::S_ISGID) as u32;
    //     }
    //     if parent_attrs.mode & libc::S_ISGID as u16 != 0 {
    //         mode |= libc::S_ISGID as u32;
    //     }

    //     let inode = self.allocate_next_inode();
    //     let attrs = InodeAttributes {
    //         inode: inode.0,
    //         open_file_handles: 0,
    //         size: u64::from(BLOCK_SIZE),
    //         last_accessed: time_now(),
    //         last_modified: time_now(),
    //         last_metadata_changed: time_now(),
    //         kind: FileKind::Directory,
    //         mode: self.creation_mode(mode),
    //         hardlinks: 2, // Directories start with link count of 2, since they have a self link
    //         uid: _req.uid(),
    //         gid: creation_gid(&parent_attrs, _req.gid()),
    //         xattrs: BTreeMap::default(),
    //     };
    //     self.write_inode(&attrs);

    //     let mut entries = BTreeMap::new();
    //     entries.insert(b".".to_vec(), (inode.0, FileKind::Directory));
    //     entries.insert(b"..".to_vec(), (parent.0, FileKind::Directory));
    //     self.write_directory_content(inode, &entries);

    //     let mut entries = self.get_directory_content(parent).unwrap();
    //     entries.insert(name.as_bytes().to_vec(), (inode.0, FileKind::Directory));
    //     self.write_directory_content(parent, &entries);

    //     reply.entry(&Duration::new(0, 0), &attrs.into(), fuser::Generation(0));
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
