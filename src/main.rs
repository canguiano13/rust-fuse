use std::fs::File;
use std::io;
use std::io::prelude::*;
use std::io::ErrorKind;
use std::io::BufReader;
use std::io::BufRead;
use std::path::PathBuf;
use std::path::Path;
use bitmaps::Bitmap;
use std::os::unix::fs::FileExt;
use clap::Parser;
use std::ffi::OsStr;
use std::collections::HashMap;
use std::sync::RwLock;


use log::LevelFilter;
use log::debug;
use log::error;
use log::info;
// use log::warn;

use serde::Deserialize;
use serde::Serialize;

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
use fuser::ReplyEmpty;
use fuser::ReplyEntry;

const FSID: u32 = 0x55555;

struct Superblock {
    // Magic number identifying the file system.
    fsid: u32,
    // Block size in bytes.
    block_size: u32,
    num_blocks: u64,
    num_inodes: u64,
    // Start location of the first bitmap.
    bitmap_start: u64,
    // Start location of the inode table.
    itable_start: u64,
    // Start location of the data blocks.
    data_start: u64,
}

impl Superblock {
    fn new(block_size: u32, num_inodes: u64, num_blocks: u64) -> Superblock {
        // Use std::mem.size_of to get an aligned size calculation.
        let sb_size = size_of::<Superblock>() as u64;
        let it_start = sb_size + (num_inodes / 8) + (num_blocks / 8);
        let da_start = it_start + (size_of::<InodeAttributes>() as u64) * num_inodes;
        Superblock {
            fsid: FSID,
            block_size,
            num_inodes,
            num_blocks,
            bitmap_start: sb_size,
            // We divide by 8 since addresses (basically pointers to locations
            // in a file) are in terms of bytes (not bits).
            itable_start: it_start,
            data_start: da_start,
        }
    }
}

// The first value is the start location; the second value is the extent length.
type Extent = (u64, u64);

// TODO: Why does simple.rs use FileKind and not just fuser::FileType? Should
// we continue to do this?
#[derive(Serialize, Deserialize, Copy, Clone, PartialEq, Default, Debug)]
enum FileKind {
    #[default]
    File,
    Directory,
    Symlink,
}

#[derive(Serialize, Deserialize, Default, Clone)]
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
    // A fixed array for extents.
    pub extent_index: [Extent; 8],
    // A pointer to the location of an indirect block of extents (if needed).
    pub extent_indirect: u64,
}

// Table (flat data structure: a vector) with inodes.
type InodeTable = Vec<InodeAttributes>;

struct FuseFS {
    superblock: Superblock,
    inode_bitmap: Vec<Bitmap<1024>>,
    data_bitmap: Vec<Bitmap<1024>>,
    inode_table: InodeTable,
    block_store_fd: File,
    dir_entries: RwLock<HashMap<u64, Vec<(u64, String)>>>,
}

// Implement methods specific to FuseFS design and structure.
impl FuseFS {
    // TODO: Use PathBuf here instead of string.
    // TODO: Am I supposed to have a dedicated Error component to the Result type?
    fn new(fd: File, block_size: u32, num_inodes: u64, num_blocks: u64) -> FuseFS {
        debug!("Creating filesystem..");
        let superblock = Superblock::new(block_size, num_inodes, num_blocks);
        // TODO: Check if this syntax actually does what you want it to do.
        let inode_bitmap = vec![Bitmap::<1024>::new(); num_inodes as usize / 1024];
        let data_bitmap = vec![Bitmap::<1024>::new(); num_blocks as usize / 1024];
        let inode_table = vec![InodeAttributes::default(); num_inodes as usize];
        FuseFS {
            superblock,
            inode_bitmap,
            data_bitmap,
            inode_table,
            block_store_fd: fd,
            dir_entries: RwLock::new(HashMap::new()),
        }
    }

    // TODO: Do this stuff after we get space allocation and serialization and
    // deserialization figured out. OK, or just figure out some of the basic
    // serialization/deserialization stuff to get this working.
    fn load(mut fd: File) -> FuseFS {
        // Read file superblock
        let mut sb_buf = [0; size_of::<Superblock>()];
        let _ = match fd.read(&mut sb_buf) {
            Ok(n) => n,
            Err(e) => panic!("could not read from backing file with error: {}", e),
        };
        // Read in all data needed to re-create the filesystem
        // Return the loaded filesystem; crash if ill-formatted

        // Temporary stub until deserialization is implemented
        FuseFS::new(fd, 4096, 32768, 32768)
    }

    // TODO search for next free space in the inode table using bitmap
    // if space is available, return the offset of the free block from the start of data region
    // return None if there is no space in the data region
    fn next_free_inode(&self) -> Option<u64> { 
        let bit_per_chunk = 1024;

        // search one bitmap chunk at a time
        for (chunk_idx, chunk) in self.inode_bitmap.iter().enumerate(){
            // check each bit until we find a free space
            let mut i = 0;
            while i < bit_per_chunk && chunk.get(i){
                i += 1
            }

            // check that it's actually free and not that the entire chunk is allocated
            if i < bit_per_chunk{
                return Some(((chunk_idx * bit_per_chunk) + i) as u64);
            }
        }

        //no free spots
        return None;
    }

    // search for next free space in the data region using bitmap
    // if space is available, return the offset of the free block from the start of data region
    // return None if there is no space in the data region
    fn next_free_block(&self) -> Option<u64> { 
        let bit_per_chunk = 1024;

        // search one bitmap chunk at a time
        for (chunk_idx, chunk) in self.data_bitmap.iter().enumerate(){
            // check each bit until we find a free space
            let mut i = 0;
            while i < bit_per_chunk && chunk.get(i){
                i += 1
            }

            // check that it's actually free and not that the entire chunk is allocated
            if i < bit_per_chunk{
                return Some(((chunk_idx * bit_per_chunk) + i) as u64);
            }
        }

        //no free spots
        return None;
    }

    // allocate an inode basd on available space in the inode table
    fn allocate_inode(&mut self) -> Option<u64>{
        // get the index of the next free inode
        let free_idx = self.next_free_inode();

        if let Some(idx) = free_idx{
            // mark it as allocated
            let chunk = (idx / 1024) as usize; // compiler doesn't like it without casting for some reason??
            let bit = (idx % 1024) as usize;
            self.inode_bitmap[chunk].set(bit, true);

            // return the location that the data was allocated at
            return Some(idx);

        } else{
            // no free space available in inode table
            return None
        }
    }

    // TODO allocate data block based on available space in the data region
    fn allocate_block(&mut self) -> Option<u64>{
        // get the index of the next free data block 
        let free_idx = self.next_free_block();

        if let Some(idx) = free_idx{
            // allocate block it
            //TODO need to make some logic to write to data region
    
            // mark it as allocated
            let chunk = (idx / 1024) as usize; // compiler doesn't like it without casting for some reason??
            let bit = (idx % 1024) as usize;
            self.data_bitmap[chunk].set(bit, true);

            // return the location that the data was allocated at
            return Some(idx);

        } else{
            // no free space available in data region
            return None
        }
    }

    //number of blocks allocated for file 
    fn blocks_allocated(&self, inode_size: u64) -> u64{
        (inode_size + self.superblock.block_size as u64 - 1) / self.superblock.block_size as u64
    }

    //calculate offset into data region for given block
    fn offset_from_block_offset(&self, block_idx: u64){
        self.superblock.data_start + (block_idx * self.superblock.block_size as u64);
    }


}

// Implement the Filesystem trait to integrate FuseFS with fuser.
impl Filesystem for FuseFS {
    // TODO: Fill out this stuff.

    //Adding init
    fn init(&mut self, _req: &Request, _config: &mut fuser::KernelConfig) -> Result<(), std::io::Error> {
        info!("Filesystem mounted successfully");

        // Set up root directory as inode 1
        let root_inode = 1usize;
        let chunk = root_inode / 1024;
        let bit = root_inode % 1024;
        self.inode_bitmap[chunk].set(bit, true);
        self.inode_table[root_inode] = InodeAttributes {
            inode: root_inode as u64,
            size: 0,
            kind: FileKind::Directory,
            mode: 0o755,
            hardlinks: 2,
            uid: 0,
            gid: 0,
            ..Default::default()
        };
        Ok(())
    }

    // Add getattr
    fn getattr(&self, _req: &Request, ino: INodeNo, _fh: Option<FileHandle>, reply: fuser::ReplyAttr) {
        let chunk = ino.0 as usize / 1024;
        let bit = ino.0 as usize % 1024;

        // Check if inode is allocated in the bitmap
        match self.inode_bitmap.get(chunk) {
            None => {
                reply.error(Errno::ENOENT);
                return;
            }
            Some(bitmap) => {
                if !bitmap.get(bit) {
                    reply.error(Errno::ENOENT);
                    return;
                }
            }
        }

        // Look up the inode in the table
        let inode = &self.inode_table[ino.0 as usize];

        let kind = match inode.kind {
            FileKind::File => fuser::FileType::RegularFile,
            FileKind::Directory => fuser::FileType::Directory,
            FileKind::Symlink => fuser::FileType::Symlink,
        };

        let attrs = fuser::FileAttr {
            ino: ino,
            size: inode.size,
            blocks: (inode.size + self.superblock.block_size as u64 - 1)
                / self.superblock.block_size as u64,
            atime: std::time::UNIX_EPOCH
                + std::time::Duration::new(inode.last_accessed.0 as u64, inode.last_accessed.1),
            mtime: std::time::UNIX_EPOCH
                + std::time::Duration::new(inode.last_modified.0 as u64, inode.last_modified.1),
            ctime: std::time::UNIX_EPOCH
                + std::time::Duration::new(inode.last_metadata_changed.0 as u64, inode.last_metadata_changed.1),
            crtime: std::time::UNIX_EPOCH,
            kind,
            perm: inode.mode,
            nlink: inode.hardlinks,
            uid: inode.uid,
            gid: inode.gid,
            rdev: 0,
            blksize: self.superblock.block_size,
            flags: 0,
        };

        reply.attr(&std::time::Duration::from_secs(1), &attrs);
    }

    //Adding readdir
    fn readdir(&self, _req: &Request, ino: INodeNo, _fh: FileHandle, offset: u64, mut reply: fuser::ReplyDirectory) {
        if ino.0 != 1 {
            reply.error(Errno::ENOENT);
            return;
        }

        let mut entries = vec![
            (INodeNo(1), fuser::FileType::Directory, ".".to_string()),
            (INodeNo(1), fuser::FileType::Directory, "..".to_string()),
        ];

        // Add any files that have been created
        if let Some(children) = self.dir_entries.read().unwrap().get(&ino.0) {
            for (child_ino, name) in children {
                entries.push((INodeNo(*child_ino), fuser::FileType::RegularFile, name.clone()));
            }
        }

        for (i, (inode, kind, name)) in entries.iter().enumerate().skip(offset as usize) {
            if reply.add(*inode, (i + 1) as u64, *kind, name) {
                break;
            }
        }

        reply.ok();
    }

    // Adding lookup, need lookup before implementing create
    fn lookup(&self, _req: &Request, parent: INodeNo, _name: &OsStr, reply: fuser::ReplyEntry) {
        // For now only handle lookups in the root directory
        if parent.0 != 1 {
            reply.error(Errno::ENOENT);
            return;
        }

        // Search the inode table for a matching name, still need work
        for inode in &self.inode_table {
            if inode.hardlinks > 0 {
                
            }
        }

        // No file found
        reply.error(Errno::ENOENT);
    }

    //Adding create
    fn create(&self, _req: &Request, parent: INodeNo, name: &OsStr,
          _mode: u32, _umask: u32, _flags: i32, reply: fuser::ReplyCreate) {

        // Only support creating files in root directory for now
        if parent.0 != 1 {
            reply.error(Errno::ENOENT);
            return;
        }

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap();

        // Hardcode inode 2 for now just to verify create is working
        let attrs = fuser::FileAttr {
            ino: INodeNo(2),
            size: 0,
            blocks: 0,
            atime: std::time::UNIX_EPOCH + std::time::Duration::new(now.as_secs(), now.subsec_nanos()),
            mtime: std::time::UNIX_EPOCH + std::time::Duration::new(now.as_secs(), now.subsec_nanos()),
            ctime: std::time::UNIX_EPOCH + std::time::Duration::new(now.as_secs(), now.subsec_nanos()),
            crtime: std::time::UNIX_EPOCH,
            kind: fuser::FileType::RegularFile,
            perm: 0o644,
            nlink: 1,
            uid: _req.uid(),
            gid: _req.gid(),
            rdev: 0,
            blksize: self.superblock.block_size,
            flags: 0,
        };

        self.dir_entries
            .write()
            .unwrap()
            .entry(parent.0)
            .or_insert_with(Vec::new)
            .push((2, name.to_string_lossy().to_string()));

        info!("Created file {:?}", name);
        reply.created(
            &std::time::Duration::from_secs(1),
            &attrs,
            fuser::Generation(0),
            fuser::FileHandle(0),
            fuser::FopenFlags::empty(),
        );
    }
    
    // Adding read and write, still needs much work
    fn read(&self, _req: &Request, ino: INodeNo, _fh: FileHandle, offset: u64,
            size: u32, _flags: OpenFlags, _lock: Option<LockOwner>, reply: ReplyData) {
        // check if inode really exist
        if self.inode_bitmap.get(ino.0 as usize).is_none() {
            // If the bit is 0, file doesn't exist
            reply.error(Errno::ENOENT);
            return;
        }

        // calculate start location, start from superblock and jump slots
        let file_base_address = self.superblock.data_start + (ino.0 * self.superblock.block_size as u64);

        // create buffer, value starts at 0, type is u8
        let mut buffer = vec![0u8; size as usize];

        // Adding file to buffer at exact offset address, check if read is
        // successful and return data, otherwise just return error
        match self.block_store_fd.read_at(&mut buffer, file_base_address + offset as u64) {
            Ok(bytes_read) => reply.data(&buffer[..bytes_read]),
            Err(_) => reply.error(Errno::EIO),
        }
    }

    //carlos todo
    //create a symbolic link
    //TODO i need a mutable reference to self here, but that goes against the trait requirement. what do?
    fn symlink(&self, _req: &Request, parent: INodeNo, link_name: &OsStr, target: &Path, reply: ReplyEntry){
        // check if there is space in the inode table, if there is reserve the space
        let free_inode_idx = self.allocate_inode();
        //no space in inode table
        if free_inode_idx.is_none(){
            return reply.error(Errno::ENOSPC);
        }
        let inode_idx = free_inode_idx.unwrap();

        // check if there is space in the data region and then try to allocate space for the symlink
        let free_block_idx = self.allocate_block();
        //no space in data region
        if free_block_idx.is_none(){
            return reply.error(Errno::ENOSPC);
        }
        let block_idx = free_block_idx.unwrap();

        //store target link path as slice of bytes in data region 
        let path_bytes = target.as_os_str().as_encoded_bytes();
        let offset = sefl.offset_from_block_idx(block_idx);

        //try to write the path into a data block
        let res = self.block_store_fd.write_at(path_bytes, offset);
        if res.is_err(){
            return reply.error(Errno::EIO);
        }

        //create an inode for the symlink
        let symlink_attrs = InodeAttributes{
            inode: inode_idx,
            open_file_handles: 0,
            size: path_bytes.len() as u64,
            kind: FileKind::Symlink,
            hardlinks: 1,
            mode: 0o644,
            uid: _req.uid(),
            gid: _req.gid(),
            //TODO not sure if this is right?
            extent_index: [(block_idx, 1), (0,0), (0,0), (0,0), (0,0), (0,0), (0,0), (0,0)],
            ..Default::default()
        };

        //need to return a fuser fileattr
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap();

        let attrs = fuser::FileAttr {
            ino: INodeNo(inode_idx),
            size: symlink_attrs.size,
            blocks: self.blocks_allocated(symlink_attrs.size),
            atime: std::time::UNIX_EPOCH + std::time::Duration::new(now.as_secs(), now.subsec_nanos()),
            mtime: std::time::UNIX_EPOCH + std::time::Duration::new(now.as_secs(), now.subsec_nanos()),
            ctime: std::time::UNIX_EPOCH + std::time::Duration::new(now.as_secs(), now.subsec_nanos()),
            crtime: std::time::UNIX_EPOCH,
            kind: fuser::FileType::Symlink,
            perm: 0o644,
            nlink: 1,
            uid: _req.uid(),
            gid: _req.gid(),
            rdev: 0,
            blksize: self.superblock.block_size,
            flags: 0
        };

        //store inode in inode table
        self.inode_table[inode_idx as usize] = symlink_attrs;

        //also add it to the directory
        self.dir_entries
            .write()
            .unwrap()
            .entry(parent.0)
            .or_insert_with(Vec::new)
            .push((inode_idx, link_name.to_string_lossy().to_string()));

        reply.entry(&std::time::Duration::from_secs(1), &attrs, fuser::Generation(0));
    }

    //create a hard link
    fn link(&self, _req: &Request, ino: INodeNo, newparent: INodeNo, newname: &OsStr, reply: ReplyEntry){
        let chunk = ino.0 as usize / 1024;
        let bit = ino.0 as usize % 1024;

        //check that the inode we're trying to link to already exists
        match self.inode_bitmap.get(chunk) {
            None => {
                reply.error(Errno::EINVAL);
                return;
            }
            Some(bitmap) => {
                if !bitmap.get(bit) {
                    reply.error(Errno::EINVAL);
                    return;
                }
            }
        }

        //get inode from inode table 
        let mut inode = &mut self.inode_table[ino.0 as usize];

        //increment the hardlinks counter in inode
        inode.hardlinks = inode.hardlinks + 1;

        //create a new entry in the directory as well
        self.dir_entries
            .write()
            .unwrap()
            .entry(newparent.0)
            .or_insert_with(Vec::new)
            .push((ino.0, newname.to_string_lossy().to_string()));

        //return hardlink as fuser fileattr
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap();
        
        let filekind = match inode.kind {
            FileKind::File => fuser::FileType::RegularFile,
            FileKind::Directory => fuser::FileType::Directory,
            FileKind::Symlink => fuser::FileType::Symlink,
        };

        let attrs = fuser::FileAttr{
            ino: ino,
            size: inode.size,
            blocks: self.blocks_allocated(inode.size),
            atime: std::time::UNIX_EPOCH + std::time::Duration::new(now.as_secs(), now.subsec_nanos()),
            mtime: std::time::UNIX_EPOCH + std::time::Duration::new(now.as_secs(), now.subsec_nanos()),
            ctime: std::time::UNIX_EPOCH + std::time::Duration::new(now.as_secs(), now.subsec_nanos()),
            crtime: std::time::UNIX_EPOCH,
            kind: filekind,
            perm: inode.mode,
            nlink: inode.hardlinks,
            uid: _req.uid(),
            gid: _req.gid(),
            rdev: 0,
            blksize: self.superblock.block_size,
            flags: 0
        };
        reply.entry(&std::time::Duration::from_secs(1), &attrs, fuser::Generation(0));
    }

    //read symbolic link
    fn readlink(&self, _req: &Request, ino: INodeNo, reply: ReplyData){
        //make sure the symlink inode exists
        //check if the inode is allocated in the bitmap
        let chunk = ino.0 as usize / 1024;
        let bit = ino.0 as usize % 1024;
        match self.inode_bitmap.get(chunk) {
            None => {
                reply.error(Errno::ENOENT);
                return;
            }
            Some(bitmap) => {
                if !bitmap.get(bit) {
                    reply.error(Errno::ENOENT);
                    return;
                }
            }
        }

        //also make sure its a symlink
        let inode = &self.inode_table[ino.0 as usize];
        if inode.kind != FileKind::Symlink{
            return reply.error(Errno::EINVAL);
        }

        //get the block address from the extent
        let block_idx = inode.extent_index[0].0;

        //calculate offset into data region
        let offset = self.offset_from_block_idx(block_idx);

        //buffer to read in symlink
        //symlink stored as array of bytes
        let mut path_bytes = vec![0u8; inode.size as usize];

        //try to read the information from the data region 
        let res = self.block_store_fd.read_at(&mut path_bytes, offset);
        if res.is_err(){
            return reply.error(Errno::EIO);
        }

        //return symlink path as replydata
        let bytes_read = res.unwrap();
        reply.data(&path_bytes[..bytes_read]);
    }

    //TODO remove a file
    fn unlink(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty){
        //get the file using lookup

        //mark the entry in the inode table storing this link as free
        //decrement the number of links to the inode
        //remove it from the directory it's in
        //if there are no more links, release the data
    }
  
    //TODO 
    fn release(&self, _req: &Request, _ino: INodeNo, _fh: FileHandle, _flags: OpenFlags, _lock_owner: Option<LockOwner>, _flush: bool, reply: ReplyEmpty){
        //nothing to release because we're using a file for our data region
        //just need to make sure the data in the data region is marked as free
        //this is done in unlink already
        reply.ok()
    }

    //Create file node. Create a regular file, character device, block device, fifo or socket node.
    fn mknod(&self, _req: &Request, parent: INodeNo, name: &OsStr,  mode: u32, umask: u32, rdev: u32, reply: ReplyEmpty){
        //don't need to deal with special files for now
        Errno::ENOSYS
    }
}

#[derive(Parser)]
#[command(version, author = "Carlos Anguiano, Lucas Du, Simon Zheng")]
struct Args {
    /// Act as a client, and mount FUSE at given path
    mount_point: PathBuf,

    /// Declares the name of the backing filestore
    block_file: PathBuf,

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
    if bl_size % 4096 == 0 {
        Ok(bl_size as u32)
    } else {
        Err(format!("`{s}` must be a multiple of 412"))
    }
}

fn valid_bitmap_size(s: &str) -> Result<u64, String> {
    let bm_size: usize = s.parse().map_err(|_| format!("`{s}` is not a number"))?;
    if bm_size % 1024 == 0 {
        Ok(bm_size as u64)
    } else {
        Err(format!("`{s}` must be a multiple of 1024"))
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
    let block_file: &Path = args.block_file.as_path();
    let fs = if block_file.exists() {
        info!("specified filesystem already exists");
        info!("loading existing filesystem");
        match File::open(block_file) {
            Ok(f) => FuseFS::load(f),
            Err(e) => panic!("could not open file {} with error: {}",
                             block_file.display(),
                             e.to_string()),
        }
    } else {
        info!("creating new filesystem at {}", block_file.display());
        match File::create(block_file) {
            Ok(f) => FuseFS::new(f, block_size, num_inodes, num_blocks),
            Err(e) => panic!("could not create file {} with error: {}",
                             block_file.display(),
                             e.to_string()),
        }
    };

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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[test]
    fn test_superblock_layout() {
        let sb = Superblock::new(4096, 1024, 1024);
        assert_eq!(sb.fsid, FSID);
        assert_eq!(sb.block_size, 4096);
        // bitmap_start should be immediately after the superblock
        assert_eq!(sb.bitmap_start, size_of::<Superblock>() as u64);
        // inode table should start after both bitmaps
        let expected_itable = sb.bitmap_start + (1024 / 8) + (1024 / 8);
        assert_eq!(sb.itable_start, expected_itable);
        // data should start after the inode table
        let expected_data = sb.itable_start + size_of::<InodeAttributes>() as u64 * 1024;
        assert_eq!(sb.data_start, expected_data);
    }

    #[test]
    fn test_new_filesystem_creation() {
        let tmp = NamedTempFile::new().unwrap();
        let fd = tmp.reopen().unwrap();
        let fs = FuseFS::new(fd, 4096, 1024, 1024);

        assert_eq!(fs.superblock.block_size, 4096);
        assert_eq!(fs.superblock.num_inodes, 1024);
        assert_eq!(fs.superblock.num_blocks, 1024);
        // All inodes should be unallocated initially
        assert_eq!(fs.inode_table.len(), 1024);
        assert!(fs.inode_table.iter().all(|i| i.inode == 0));
    }

    #[test]
    fn test_valid_block_size_accepts_multiples_of_4096() {
        assert!(valid_block_size("4096").is_ok());
        assert!(valid_block_size("500").is_err());
    }

    #[test]
    fn test_valid_bitmap_size_accepts_multiples_of_1024() {
        assert!(valid_bitmap_size("1024").is_ok());
        assert!(valid_bitmap_size("32768").is_ok());
        assert!(valid_bitmap_size("500").is_err());
    }

    #[test]
    fn test_inode_attributes_default() {
        let inode = InodeAttributes::default();
        assert_eq!(inode.kind, FileKind::File);
        assert_eq!(inode.size, 0);
        assert_eq!(inode.hardlinks, 0);
    }

    #[test]
    fn test_getattr_unallocated_inode_returns_none() {
        let tmp = NamedTempFile::new().unwrap();
        let fd = tmp.reopen().unwrap();
        let fs = FuseFS::new(fd, 4096, 1024, 1024);

        // No inodes allocated yet, every bit should be unset
        let chunk = 0;
        let bit = 0;
        assert!(!fs.inode_bitmap[chunk].get(bit), "bitmap should be empty on a new filesystem");
    }

    #[test]
    fn test_readdir_root_inode_is_directory() {
        let tmp = NamedTempFile::new().unwrap();
        let fd = tmp.reopen().unwrap();
        let mut fs = FuseFS::new(fd, 4096, 1024, 1024);

        // Simulate what init() does - set up root inode
        let root_inode = 1usize;
        let chunk = root_inode / 1024;
        let bit = root_inode % 1024;
        fs.inode_bitmap[chunk].set(bit, true);
        fs.inode_table[root_inode] = InodeAttributes {
            inode: root_inode as u64,
            size: 0,
            kind: FileKind::Directory,
            mode: 0o755,
            hardlinks: 2,
            uid: 0,
            gid: 0,
            ..Default::default()
        };

        // Verify root inode is allocated in bitmap
        assert!(fs.inode_bitmap[chunk].get(bit), "root inode should be allocated");

        // Verify root inode is a directory
        assert_eq!(fs.inode_table[root_inode].kind, FileKind::Directory);

        // Verify root inode has correct hardlinks
        assert_eq!(fs.inode_table[root_inode].hardlinks, 2);

        // Verify permissions
        assert_eq!(fs.inode_table[root_inode].mode, 0o755);
    }   


    #[test]
    fn test_next_free_inode_empty_fs() {
        let tmp = NamedTempFile::new().unwrap();
        let fd = tmp.reopen().unwrap();
        let mut fs = FuseFS::new(fd, 4096, 1024, 1024);

        //verify that the first space in inode table is free for a new fs
        let first_free_inode = fs.next_free_inode();
        assert_eq!(first_free_inode, Some(0));
        
    }
    #[test]
    fn test_next_free_inode_full_fs() {
        let tmp = NamedTempFile::new().unwrap();
        let fd = tmp.reopen().unwrap();
        let mut fs = FuseFS::new(fd, 4096, 1024, 1024);

        //fill the bitmap using allocate_indode()
        //TODO verify that this logic is correct
        for i in 0..fs.superblock.num_inodes{
            fs.allocate_inode();
        }

        let next_free_inode = fs.next_free_inode();
        assert_eq!(next_free_inode, None);
    }


    #[test]
    fn test_next_free_block_empty_fs() {
        let tmp = NamedTempFile::new().unwrap();
        let fd = tmp.reopen().unwrap();
        let mut fs = FuseFS::new(fd, 4096, 1024, 1024);

        let first_free_block = fs.next_free_block();
        assert_eq!(first_free_block, Some(0));
    }
    #[test]
    fn test_next_free_block_full_fs() {
        let tmp = NamedTempFile::new().unwrap();
        let fd = tmp.reopen().unwrap();
        let mut fs = FuseFS::new(fd, 4096, 1024, 1024);

        //fill the bitmap using allocate_block()
        //TODO verify that this logic is correct
        for i in 0..fs.superblock.num_blocks{
            fs.allocate_block();
        }

        let next_free_block = fs.next_free_block();
        assert_eq!(next_free_block, None);
    }

    #[test]
    fn test_allocate_inode_sets_bitmap() {
        let tmp = NamedTempFile::new().unwrap();
        let fd = tmp.reopen().unwrap();
        let mut fs = FuseFS::new(fd, 4096, 1024, 1024);

        // Allocate an inode and check the bit is set
        let inode_num = fs.allocate_inode().expect("should have free inodes");
        let chunk = (inode_num / 1024) as usize;
        let bit = (inode_num % 1024) as usize;
        assert!(fs.inode_bitmap[chunk].get(bit), "bitmap bit should be set after allocation");
    }

    #[test]
    fn test_allocate_inode_returns_different_inodes() {
        let tmp = NamedTempFile::new().unwrap();
        let fd = tmp.reopen().unwrap();
        let mut fs = FuseFS::new(fd, 4096, 1024, 1024);

        // Two allocations should return different inodes
        let inode1 = fs.allocate_inode().expect("should have free inodes");
        let inode2 = fs.allocate_inode().expect("should still have free inodes");
        assert_ne!(inode1, inode2, "should not allocate the same inode twice");
    }

    #[test]
    fn test_allocate_block_sets_bitmap() {
        let tmp = NamedTempFile::new().unwrap();
        let fd = tmp.reopen().unwrap();
        let mut fs = FuseFS::new(fd, 4096, 1024, 1024);

        // Allocate a block and check the bit is set
        let block_num = fs.allocate_block().expect("should have free blocks");
        let chunk = (block_num / 1024) as usize;
        let bit = (block_num % 1024) as usize;
        assert!(fs.data_bitmap[chunk].get(bit), "bitmap bit should be set after allocation");
    }

    #[test]
    fn test_allocate_block_returns_different_blocks() {
        let tmp = NamedTempFile::new().unwrap();
        let fd = tmp.reopen().unwrap();
        let mut fs = FuseFS::new(fd, 4096, 1024, 1024);

        // Two allocations should return different blocks
        let block1 = fs.allocate_block().expect("should have free blocks");
        let block2 = fs.allocate_block().expect("should still have free blocks");
        assert_ne!(block1, block2, "should not allocate the same block twice");
    }

    #[test]
    fn test_allocate_inode_returns_none_when_full() {
        let tmp = NamedTempFile::new().unwrap();
        let fd = tmp.reopen().unwrap();
        let mut fs = FuseFS::new(fd, 4096, 1024, 1024);

        // Fill up all inodes
        for _ in 0..1024 {
            fs.allocate_inode().expect("should have free inodes");
        }

        // Next allocation should return None
        assert!(fs.allocate_inode().is_none(), "should return None when all inodes are used");
    }
}
