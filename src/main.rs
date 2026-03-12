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
use std::mem::size_of;
use fuser::WriteFlags;

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
type InodeTable = RwLock<Vec<InodeAttributes>>;

struct FuseFS {
    superblock: Superblock,
    inode_bitmap: RwLock<Vec<Bitmap<1024>>>,
    data_bitmap: RwLock<Vec<Bitmap<1024>>>,
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
        let inode_bitmap = RwLock::new(vec![Bitmap::<1024>::new(); num_inodes as usize / 1024]);
        let data_bitmap = RwLock::new(vec![Bitmap::<1024>::new(); num_blocks as usize / 1024]);
        let inode_table = RwLock::new(vec![InodeAttributes::default(); num_inodes as usize]);
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

    // search for next free space in the inode table using bitmap
    // if space is available, return the offset of the free block from the start of data region
    // return None if there is no space in the data region
    fn next_free_inode(&self) -> Option<u64> { 
        let bit_per_chunk = 1024;

        let inode_bitmap_binding = self.inode_bitmap.read().unwrap();
        // search one bitmap chunk at a time
        for (chunk_idx, chunk) in inode_bitmap_binding.iter().enumerate(){
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


        // search bitmap one chunk at a time
        let data_bitmap_binding = self.data_bitmap.read().unwrap();
        for (chunk_idx, chunk) in data_bitmap_binding.iter().enumerate(){
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
    fn allocate_inode(&self) -> Option<u64>{
        // get the index of the next free inode
        let free_idx = self.next_free_inode();

        if let Some(idx) = free_idx{
            let chunk = (idx / 1024) as usize; // compiler doesn't like it without casting for some reason??
            let bit = (idx % 1024) as usize;

            // mark inode bitmap slot as allocated
            let mut bitmap_binding = self.inode_bitmap.write().unwrap();
            let bitmap_chunk = match bitmap_binding.get_mut(chunk){
                Some(slot) => slot,
                None => return None
            };
            bitmap_chunk.set(bit, true);

            // return the location that the data was allocated at
            return Some(idx);

        } else{
            // no free space available in inode table
            return None
        }
    }

    // allocate data block based on available space in the data region
    fn allocate_block(&self) -> Option<u64>{
        // get the index of the next free data block 
        let free_idx = self.next_free_block();

        if let Some(idx) = free_idx{
            // mark it as allocated
            let chunk = (idx / 1024) as usize; 
            let bit = (idx % 1024) as usize;

            let mut bitmap_binding = self.data_bitmap.write().unwrap();
            let bitmap_chunk = match bitmap_binding.get_mut(chunk){
                Some(slot) => slot,
                None => return None
            };
            
            bitmap_chunk.set(bit, true);

            // return the location that the data was allocated at
            return Some(idx);

        } else{
            // no free space available in data region
            return None
        }
    }

    //clear the bitmap bit for a space in the inode table
    fn free_inode(&self, inode_idx: u64){
        let chunk = (inode_idx / 1024) as usize; 
        let bit = (inode_idx % 1024) as usize;

        //bitmap structure uses chunks of 1024 bits
        let inodes_per_bitmap_chunk = 1024usize;

        //dont deallocate anyhing out of bounds
        if chunk >= (self.superblock.num_inodes as usize / inodes_per_bitmap_chunk){
            debug!("couldn't free out of bounds chunk from inode table");
            return; 
        } 

        //get inode bitmap chunk
        let mut inode_bitmap_binding = self.inode_bitmap.write().unwrap();
        let bitmap_chunk = match inode_bitmap_binding.get_mut(chunk){
            Some(slot) => slot,
            None => {
                debug!("couldn't free bit in inode bitmap");
                return;
            }
        };


        //clear bit in inode bitmap
        bitmap_chunk.set(bit, false);
        return 
    }

    //clear the data bitmap bit for some space in the data region
    fn free_block(&self, block_idx: u64){
        let chunk = (block_idx / 1024) as usize; 
        let bit = (block_idx % 1024) as usize;

        let blocks_per_bitmap_chunk= 1024usize;

        //dont deallocate anyhing out of bounds
        if chunk >= (self.superblock.num_blocks as usize / blocks_per_bitmap_chunk){
            debug!("chunk out of bounds, couldn't free data bitmap chunk");
            return
        }

        //get data bitmap chunk
        let mut data_bitmap_binding = self.data_bitmap.write().unwrap();
        let bitmap_chunk = match data_bitmap_binding.get_mut(chunk){
            Some(slot) => slot,
            None => {
                debug!("couldn't free bit in data bitmap");
                return
            }
        };

        //clear bit in data bitmap
        bitmap_chunk.set(bit, false);
        return;
    }

    //number of blocks allocated for file 
    fn blocks_allocated(&self, inode_size: u64) -> u64{
        return (inode_size + self.superblock.block_size as u64 - 1) / self.superblock.block_size as u64
    }

    //calculate offset into data region for given block
    fn offset_from_block_idx(&self, block_idx: u64) -> u64{
       return self.superblock.data_start + (block_idx * self.superblock.block_size as u64);
    }

    //store inode in inode table at inode_idx
    fn set_inode_table(&self, inode_idx: u64, data: InodeAttributes){
        if let Some(ino) = self.inode_table.write().unwrap().get_mut(inode_idx as usize){
            *ino = data.clone();
        }
        // persist to disk
        if let Err(e) = self.write_inode(inode_idx, &data) {
            error!("failed to write inode {} to disk: {}", inode_idx, e);
        }
    }

    //find a entry in a directory
    fn find_dir_entry(&self, parent: u64, target_name: &OsStr) -> Option<u64>{
        //get files in directory based on directory inode number
        let binding = self.dir_entries.read().unwrap();
        let entries = binding.get(&parent);

        //search through directory entries for target file
        if let Some(entries) = entries {
            for (inode_number, entry_name) in entries.iter(){
                if entry_name.as_str() == target_name.to_string_lossy().as_ref(){
                    return Some(*inode_number);
                }
            }
        }

        return None;
    }

    fn decrement_links(&self,inode_no: u64) -> Option<(u32, [Extent; 8])>{
      //get the inode from the inode table
        let mut inode_table_binding = self.inode_table.write().unwrap();
        let inode = match inode_table_binding.get_mut(inode_no as usize){
            Some(ino) => ino,
            None => return None
        };

        let hardlinks = inode.hardlinks;
        let extent_idx = inode.extent_index;

        inode.hardlinks = hardlinks - 1;

        return Some((hardlinks, extent_idx));
    }

    fn write_inode(&self, inode_idx: u64, inode: &InodeAttributes) -> io::Result<()> {
        // Convert to bytes using bincode
        let bytes = bincode::serialize(inode)
            .map_err(|e| io::Error::new(ErrorKind::Other, e))?;
        
        // Calculate where inode is written
        let offset = self.superblock.itable_start 
            + (inode_idx * size_of::<InodeAttributes>() as u64);
        // Write at the correct location
        self.block_store_fd.write_at(&bytes, offset)?;
        Ok(())
    }

    fn read_inode(&self, inode_idx: u64) -> io::Result<InodeAttributes> {
        let offset = self.superblock.itable_start
            + (inode_idx * size_of::<InodeAttributes>() as u64);
        
        // Create buffer and read
        let mut buf = vec![0u8; size_of::<InodeAttributes>()];
        self.block_store_fd.read_at(&mut buf, offset)?; 
        
        //Convert bytes back, basically reverse of a step in write_inode
        bincode::deserialize(&buf)
            .map_err(|e| io::Error::new(ErrorKind::Other, e))
    }
}

// Implement the Filesystem trait to integrate FuseFS with fuser.
impl Filesystem for FuseFS {
    // TODO: Fill out this stuff.

    //initialize filesystem. called before any other filesystem method.
    fn init(&mut self, _req: &Request, _config: &mut fuser::KernelConfig) -> Result<(), std::io::Error> {
        info!("Filesystem mounted successfully");

        // Set up root directory as inode 1
        let root_inode_idx = 1usize;
        let chunk = root_inode_idx / 1024;
        let bit = root_inode_idx % 1024;


        let mut inode_bitmap_binding = self.inode_bitmap.write().unwrap();
        let bitmap_chunk = match inode_bitmap_binding.get_mut(chunk){
            Some(slot) => slot,
            None => return Err(std::io::Error::new(ErrorKind::Other, "failed to get inode bitmap chunk"))
        };

        //mark bitmap entry as allocated
        bitmap_chunk.set(bit, true);

        let root_inode = InodeAttributes{
            inode: root_inode_idx as u64,
            size: 0,
            kind: FileKind::Directory,
            mode: 0o755,
            hardlinks: 2,
            uid: 0,
            gid: 0,
            ..Default::default()
        };
        //insert into inode table
        self.set_inode_table(root_inode_idx as u64, root_inode);
        Ok(())
    }

    // get file attributes
    fn getattr(&self, _req: &Request, ino: INodeNo, _fh: Option<FileHandle>, reply: fuser::ReplyAttr) {
        let chunk = ino.0 as usize / 1024;
        let bit = ino.0 as usize % 1024;

        let inode_bitmap_binding = self.inode_bitmap.read().unwrap();
        let bitmap_chunk = match inode_bitmap_binding.get(chunk){
            Some(slot) => slot,
            None => return reply.error(Errno::ENOENT) 
        };

        // Check if inode is allocated in the bitmap
        if !bitmap_chunk.get(bit){
            reply.error(Errno::ENOENT);
            return;
        }

        // Look up the inode in the table
        let binding: &std::sync::RwLockReadGuard<'_, Vec<InodeAttributes>> = &self.inode_table.read().unwrap();
        let inode = match binding.get(ino.0 as usize){
            Some(ino) => ino,
            None => return reply.error(Errno::EINVAL)
        };
        

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

    // read directory
    fn readdir(&self, _req: &Request, ino: INodeNo, _fh: FileHandle, offset: u64, mut reply: fuser::ReplyDirectory) {
        // Error for non-existent directory
        if ino.0 != 1 {
            reply.error(Errno::ENOENT);
            return;
        }

        // Directory entries
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

        // Send back to kernel
        for (i, (inode, kind, name)) in entries.iter().enumerate().skip(offset as usize) {
            if reply.add(*inode, (i + 1) as u64, *kind, name) {
                break;
            }
        }

        reply.ok();
    }

    

    // look up a directory entry by name and get it's attributes
    // need lookup before implementing create
    fn lookup(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: fuser::ReplyEntry) {
        if parent.0 != 1 {
            reply.error(Errno::ENOENT);
            return;
        }

        // search dir_entries for the file
        let inode_no = self.find_dir_entry(parent.0, name);
        if inode_no.is_none() {
            reply.error(Errno::ENOENT);
            return;
        }
        let inode_no = inode_no.unwrap();

        // get the inode from the inode table
        let inode_table_binding = self.inode_table.read().unwrap();
        let inode = match inode_table_binding.get(inode_no as usize) {
            Some(ino) => ino,
            None => return reply.error(Errno::ENOENT),
        };

        // convert fuser::FileType, important!
        let kind = match inode.kind {
            FileKind::File => fuser::FileType::RegularFile,
            FileKind::Directory => fuser::FileType::Directory,
            FileKind::Symlink => fuser::FileType::Symlink,
        };

        // build file attributes from inode data
        let attrs = fuser::FileAttr {
            ino: INodeNo(inode_no),
            size: inode.size,
            blocks: self.blocks_allocated(inode.size),
            atime: std::time::UNIX_EPOCH + std::time::Duration::new(inode.last_accessed.0 as u64, inode.last_accessed.1),
            mtime: std::time::UNIX_EPOCH + std::time::Duration::new(inode.last_modified.0 as u64, inode.last_modified.1),
            ctime: std::time::UNIX_EPOCH + std::time::Duration::new(inode.last_metadata_changed.0 as u64, inode.last_metadata_changed.1),
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

        reply.entry(&std::time::Duration::from_secs(1), &attrs, fuser::Generation(0));
    }

    //create and open a file
    fn create(&self, _req: &Request, parent: INodeNo, name: &OsStr,
      _mode: u32, _umask: u32, _flags: i32, reply: fuser::ReplyCreate) {

        if parent.0 != 1 {
            reply.error(Errno::ENOENT);
            return;
        }

        // allocate free inode
        let inode_idx = match self.allocate_inode() {
            Some(idx) => idx,
            None => return reply.error(Errno::ENOSPC),
        };

        // timestamp
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap();

        // create and store the inode
        let new_inode = InodeAttributes {
            inode: inode_idx,
            size: 0,
            kind: FileKind::File,
            hardlinks: 1,
            mode: 0o644,
            uid: _req.uid(),
            gid: _req.gid(),
            ..Default::default()
        };
        self.set_inode_table(inode_idx, new_inode);

        let attrs = fuser::FileAttr {
            ino: INodeNo(inode_idx),
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
            .push((inode_idx, name.to_string_lossy().to_string()));

        info!("Created file {:?}", name);
        reply.created(
            &std::time::Duration::from_secs(1),
            &attrs,
            fuser::Generation(0),
            fuser::FileHandle(0),
            fuser::FopenFlags::empty(),
        );
    }
    
    // read data
    fn read(&self, _req: &Request, ino: INodeNo, _fh: FileHandle, offset: u64,
        size: u32, _flags: OpenFlags, _lock: Option<LockOwner>, reply: ReplyData) {

        // check inode is allocated in bitmap
        let chunk = ino.0 as usize / 1024;
        let bit = ino.0 as usize % 1024;

        let inode_bitmap_binding = self.inode_bitmap.read().unwrap();
        let bitmap_chunk = match inode_bitmap_binding.get(chunk) {
            Some(slot) => slot,
            None => return reply.error(Errno::ENOENT),
        };
        if !bitmap_chunk.get(bit) {
            return reply.error(Errno::ENOENT);
        }
        drop(inode_bitmap_binding);

        // get inode from inode table
        let inode_table_binding = self.inode_table.read().unwrap();
        let inode = match inode_table_binding.get(ino.0 as usize) {
            Some(ino) => ino,
            None => return reply.error(Errno::ENOENT),
        };

        let block_size = self.superblock.block_size as u64;
        let mut bytes_read_total = 0u64;
        let mut result = vec![0u8; size as usize];

        // walk extents to read data
        let mut remaining = size as u64;
        let mut current_offset = offset;

        for (block_idx, _length) in inode.extent_index.iter() {
            if remaining == 0 {
                break;
            }
            if *block_idx == 0 {
                continue;
            }

            let block_num = current_offset / block_size;
            let block_offset = current_offset % block_size;
            let bytes_to_read = remaining.min(block_size - block_offset);

            let read_offset = self.offset_from_block_idx(*block_idx) + block_offset;
            let buf_start = bytes_read_total as usize;
            let buf_end = buf_start + bytes_to_read as usize;

            match self.block_store_fd.read_at(&mut result[buf_start..buf_end], read_offset) {
                Ok(n) => {
                    bytes_read_total += n as u64;
                    remaining -= n as u64;
                    current_offset += n as u64;
                }
                Err(_) => return reply.error(Errno::EIO),
            }

            let _ = block_num; // suppress unused warning
        }

        reply.data(&result[..bytes_read_total as usize]);
    }

    fn write(&self, _req: &Request, ino: INodeNo, _fh: FileHandle, offset: u64,
         data: &[u8], _write_flags: WriteFlags, _flags: OpenFlags,
         _lock_owner: Option<LockOwner>, reply: fuser::ReplyWrite) {

        // get the inode from the inode table
        let mut inode_table_binding= self.inode_table.write().unwrap();
        let inode = match inode_table_binding.get_mut(ino.0 as usize) {
            Some(ino) => ino,
            None => return reply.error(Errno::ENOENT),
        };

        // figure out which block the offset falls into
        let block_size = self.superblock.block_size as u64;
        let block_num = offset / block_size;
        let block_offset = offset % block_size;

        // check if we already have a block allocated for this position
        let block_idx = if inode.extent_index[block_num as usize].0 != 0 {
            // block already allocated, reuse it
            inode.extent_index[block_num as usize].0
        } else {
            // allocate a new block
            let new_block = match self.allocate_block() {
                Some(b) => b,
                None => return reply.error(Errno::ENOSPC),
            };
            inode.extent_index[block_num as usize] = (new_block, 1);
            new_block
        };

        // calculate the byte offset into the data region
        let write_offset = self.offset_from_block_idx(block_idx) + block_offset;

        // write data to disk
        match self.block_store_fd.write_at(data, write_offset) {
            Ok(bytes_written) => {
                // update inode size if needed
                let new_size = offset + bytes_written as u64;
                if new_size > inode.size {
                    inode.size = new_size;
                }

                // persist inode to disk
                if let Err(e) = self.write_inode(ino.0, inode) {
                    error!("failed to persist inode {}: {}", ino.0, e);
                    return reply.error(Errno::EIO);
                }

                reply.written(bytes_written as u32);
            }
            Err(_) => reply.error(Errno::EIO),
        }
    }

    //create a symbolic link
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
        let offset = self.offset_from_block_idx(block_idx);

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
        self.set_inode_table(inode_idx, symlink_attrs);

        //also add it to the parent directory
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

        let inode_bitmap_binding = self.inode_bitmap.read().unwrap();
        let bitmap_chunk = match inode_bitmap_binding.get(chunk){
            Some(slot) => slot,
            None => return reply.error(Errno::ENOENT) 
        };

        //check that the inode we're trying to link to already exists
        if !bitmap_chunk.get(bit) {
                reply.error(Errno::EINVAL);
                return;
        }

        //get inode from inode table 
        let mut inode_table_binding = self.inode_table.write().unwrap();
        let inode: &mut InodeAttributes = match inode_table_binding.get_mut(ino.0 as usize){
            Some(ino) => ino,
            None => return reply.error(Errno::EINVAL)
        };

        //increment the hardlinks counter in inode
        inode.hardlinks = inode.hardlinks + 1;

        //create a new entry in the parent directory
        self.dir_entries
            .write()
            .unwrap()
            .entry(newparent.0)
            .or_insert_with(Vec::new)
            .push((ino.0, newname.to_string_lossy().to_string()));

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap();
        
        let filekind = match inode.kind {
            FileKind::File => fuser::FileType::RegularFile,
            FileKind::Directory => fuser::FileType::Directory,
            FileKind::Symlink => fuser::FileType::Symlink,
        };


        //return hardlink as fuser fileattr
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

        let inode_bitmap_binding = self.inode_bitmap.read().unwrap();
        let bitmap_chunk = match inode_bitmap_binding.get(chunk){
            Some(slot) => slot,
            None => return reply.error(Errno::ENOENT) 
        };

        if !bitmap_chunk.get(bit) {
            reply.error(Errno::ENOENT);
            return;
        }

        //also make sure its a symlink
        let inode_table_binding = self.inode_table.read().unwrap();
        let inode = match inode_table_binding.get(ino.0 as usize){
            Some(ino) => ino,
            None => return reply.error(Errno::EINVAL)
        };
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

    //remove a file
    fn unlink(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty){
        //lookup the file to get the inode number
        let target_inode_no = self.find_dir_entry(parent.0, name);

        //if target does not exist, can't unlink
        if target_inode_no.is_none(){
            return reply.error(Errno::ENOENT);
        }
        let inode_no = target_inode_no.unwrap();

  
        //decrement the number of hardlinks
        let (hardlinks, extent_idx)= match self.decrement_links(inode_no){
            Some((link_no, extent_idx)) => (link_no, extent_idx),
            None => return reply.error(Errno::EINVAL)
        };

        //remove from parent directory
        let mut dir_entries_binding = self.dir_entries.write().unwrap();
        if let Some(entries) = dir_entries_binding.get_mut(&parent.0){
            let mut i = 0;
            while i < entries.len(){
                if entries[i].1.as_str() == name.to_string_lossy().as_ref(){
                    entries.remove(i);
                    break;
                }

                i += 1;
            }
        }
        //drop the RwLock for dir_entries
        drop(dir_entries_binding);


        //if this was the last link, release the data
        if hardlinks == 1{
            //remove the inode from the inode table
            self.free_inode(inode_no);

            //clear all data in data region used by file
            for(start_block, length) in extent_idx{
                if start_block != 0{
                    for block_idx in start_block..(start_block + length){
                        self.free_block(block_idx);
                    }
                }
            }
        }

        reply.ok();
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
    //verify properties of newly-initialized fs
    fn test_new_filesystem_creation() {
        let tmp = NamedTempFile::new().unwrap();
        let fd = tmp.reopen().unwrap();
        let fs = FuseFS::new(fd, 4096, 1024, 1024);

        assert_eq!(fs.superblock.block_size, 4096);
        assert_eq!(fs.superblock.num_inodes, 1024);
        assert_eq!(fs.superblock.num_blocks, 1024);
        let inode_table_binding = fs.inode_table.read().unwrap();
        // All inodes should be unallocated initially
        assert_eq!(inode_table_binding.len(), 1024);
        assert!(inode_table_binding.iter().all(|i| i.inode == 0));
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
    //check attributes of default inode
    fn test_inode_attributes_default() {
        let inode = InodeAttributes::default();
        assert_eq!(inode.kind, FileKind::File);
        assert_eq!(inode.size, 0);
        assert_eq!(inode.hardlinks, 0);
    }

    #[test]
    //ensure empty inode bitmap for a newly-initialized fs
    fn test_getattr_unallocated_inode_returns_none() {
        let tmp = NamedTempFile::new().unwrap();
        let fd = tmp.reopen().unwrap();
        let fs = FuseFS::new(fd, 4096, 1024, 1024);

        //no inodes allocated yet, every bit should be unset
        let chunk = 0;
        let bit = 0;

        let bitmap_binding = fs.inode_bitmap.read().unwrap();
        if let Some(bitmap_chunk) = bitmap_binding.get(chunk) {
            assert!(!bitmap_chunk.get(bit), "bitmap should be empty on a new filesystem");
        } else{
            assert!(false)
        }
    }

    #[test]
    //simulate initialization of root inode
    fn test_readdir_root_inode_is_directory() {
        let tmp = NamedTempFile::new().unwrap();
        let fd = tmp.reopen().unwrap();
        let fs = FuseFS::new(fd, 4096, 1024, 1024);

        // Simulate what init() does - set up root inode
        let root_inode_idx = 1usize;
        let chunk = root_inode_idx / 1024;
        let bit = root_inode_idx % 1024;

        let mut inode_bitmap_binding = fs.inode_bitmap.write().unwrap();
        if  let Some(bitmap_chunk) = inode_bitmap_binding.get_mut(chunk){
            bitmap_chunk.set(bit, true);
        } else{
            assert!(false);
        }

        
        let root_inode = InodeAttributes {
            inode: root_inode_idx as u64,
            size: 0,
            kind: FileKind::Directory,
            mode: 0o755,
            hardlinks: 2,
            uid: 0,
            gid: 0,
            ..Default::default()
        };
        fs.set_inode_table(root_inode_idx as u64, root_inode);

        // Verify root inode is allocated in bitmap
        if let Some(bitmap_chunk) = inode_bitmap_binding.get(chunk){
            assert!(bitmap_chunk.get(bit), "root inode should be allocated");
        }
        else{
            assert!(false);
        }


        let inode_table_binding = fs.inode_table.read().unwrap();
        let expect_root_inode = match inode_table_binding.get(root_inode_idx){
            Some(ino) => ino,
            None => &InodeAttributes { 
                inode: 0, 
                hardlinks: 0,
                mode: 0,
                ..Default::default()
            }
        };
        // Verify root inode is a directory
        assert_eq!(expect_root_inode.kind, FileKind::Directory);

        // Verify root inode has correct hardlinks
        assert_eq!(expect_root_inode.hardlinks, 2);

        // Verify permissions
        assert_eq!(expect_root_inode.mode, 0o755);
    }   


    #[test]
    //try to allocate space for an inode in an empty inode table
    //first space should be empty
    fn test_next_free_inode_empty_fs() {
        let tmp = NamedTempFile::new().unwrap();
        let fd = tmp.reopen().unwrap();
        let fs = FuseFS::new(fd, 4096, 1024, 1024);

        //verify that the first space in inode table is free for a new fs
        let first_free_inode = fs.next_free_inode();
        assert_eq!(first_free_inode, Some(0));
        
    }
    #[test]
    //try to allocate space for an inode in a full inode table
    //should indicate that no spaces are free
    fn test_next_free_inode_full_fs() {
        let tmp = NamedTempFile::new().unwrap();
        let fd = tmp.reopen().unwrap();
        let fs = FuseFS::new(fd, 4096, 1024, 1024);

        //fill the bitmap using allocate_inode()
        for _i in 0..fs.superblock.num_inodes{
            fs.allocate_inode();
        }

        let next_free_inode = fs.next_free_inode();
        assert_eq!(next_free_inode, None);
    }


    #[test]
    //try to allocate a data block in an empty data region
    fn test_next_free_block_empty_fs() {
        let tmp = NamedTempFile::new().unwrap();
        let fd = tmp.reopen().unwrap();
        let fs = FuseFS::new(fd, 4096, 1024, 1024);

        let first_free_block = fs.next_free_block();
        assert_eq!(first_free_block, Some(0));
    }
    #[test]
    //try to allocate a data block in a full data region
    fn test_next_free_block_full_fs() {
        let tmp = NamedTempFile::new().unwrap();
        let fd = tmp.reopen().unwrap();
        let fs = FuseFS::new(fd, 4096, 1024, 1024);

        //fill the bitmap using allocate_block()
        for _i in 0..fs.superblock.num_blocks{
            fs.allocate_block();
        }

        let next_free_block = fs.next_free_block();
        assert_eq!(next_free_block, None);
    }

    #[test]
    //allocate an inode and check that the bitmap bit is set
    //remaining nodes should be free
    fn test_allocate_inode_sets_bitmap() {
        let tmp = NamedTempFile::new().unwrap();
        let fd = tmp.reopen().unwrap();
        let fs = FuseFS::new(fd, 4096, 1024, 1024);

        let inode_num = fs.allocate_inode().expect("should have free inodes");
        let chunk = (inode_num / 1024) as usize;
        let bit = (inode_num % 1024) as usize;


        let inode_bitmap_binding = fs.inode_bitmap.read().unwrap();
        if let Some(bitmap_chunk) = inode_bitmap_binding.get(chunk){
            //allocate_inode should mark bit as allocated in bitmap
            assert!(bitmap_chunk.get(bit), "bitmap bit should be set after allocation");
        }
        else{
            assert!(false);
        }
    }

    #[test]
    //two inode allocations should return different inodes
    fn test_allocate_inode_returns_different_inodes() {
        let tmp = NamedTempFile::new().unwrap();
        let fd = tmp.reopen().unwrap();
        let fs = FuseFS::new(fd, 4096, 1024, 1024);

        let inode1 = fs.allocate_inode().expect("should have free inodes");
        let inode2 = fs.allocate_inode().expect("should still have free inodes");
        assert_ne!(inode1, inode2, "should not allocate the same inode twice");
    }

    #[test]
    fn test_allocate_block_sets_bitmap() {
        let tmp = NamedTempFile::new().unwrap();
        let fd = tmp.reopen().unwrap();
        let fs = FuseFS::new(fd, 4096, 1024, 1024);

        // Allocate a block and check the bit is set
        let block_num = fs.allocate_block().expect("should have free blocks");
        let chunk = (block_num / 1024) as usize;
        let bit = (block_num % 1024) as usize;

        let data_bitmap_binding= fs.data_bitmap.read().unwrap();
        if let Some(bitmap_chunk) = data_bitmap_binding.get(chunk){
            //allocate_block should mark bit as allocated in bitmap
            assert!(bitmap_chunk.get(bit), "bitmap bit should be set after allocation");
        }
        else{
            assert!(false);
        }
    }

    #[test]
    fn test_allocate_block_returns_different_blocks() {
        let tmp = NamedTempFile::new().unwrap();
        let fd = tmp.reopen().unwrap();
        let fs = FuseFS::new(fd, 4096, 1024, 1024);

        // Two allocations should return different blocks
        let block1 = fs.allocate_block().expect("should have free blocks");
        let block2 = fs.allocate_block().expect("should still have free blocks");
        assert_ne!(block1, block2, "should not allocate the same block twice");
    }

    #[test]
    fn test_allocate_inode_returns_none_when_full() {
        let tmp = NamedTempFile::new().unwrap();
        let fd = tmp.reopen().unwrap();
        let fs = FuseFS::new(fd, 4096, 1024, 1024);

        //fill up all inodes
        for _ in 0..1024 {
            fs.allocate_inode().expect("should have free inodes");
        }

        //attempt at allocation should return None
        assert!(fs.allocate_inode().is_none(), "should return None when all inodes are used");
    }

    #[test]
    fn test_write_inode_persists_to_disk() {
        let tmp = NamedTempFile::new().unwrap();
        let fd = tmp.reopen().unwrap();
        let fs = FuseFS::new(fd, 4096, 1024, 1024);

        let inode = InodeAttributes {
            inode: 5,
            size: 1234,
            kind: FileKind::File,
            hardlinks: 1,
            mode: 0o644,
            ..Default::default()
        };

        fs.write_inode(5, &inode).expect("write_inode should succeed");

        // Read back from disk and verify
        let offset = fs.superblock.itable_start 
            + (5 * size_of::<InodeAttributes>() as u64);
        let mut buf = vec![0u8; size_of::<InodeAttributes>()];
        fs.block_store_fd.read_at(&mut buf, offset).unwrap();

        let recovered: InodeAttributes = bincode::deserialize(&buf).unwrap();
        assert_eq!(recovered.inode, 5);
        assert_eq!(recovered.size, 1234);
        assert_eq!(recovered.kind, FileKind::File);
    }

    #[test]
    fn test_read_write_inode_roundtrip() {
        let tmp = NamedTempFile::new().unwrap();
        let fd = tmp.reopen().unwrap();
        let fs = FuseFS::new(fd, 4096, 1024, 1024);

        let inode = InodeAttributes {
            inode: 5,
            size: 1234,
            kind: FileKind::File,
            hardlinks: 1,
            mode: 0o644,
            ..Default::default()
        };

        fs.write_inode(5, &inode).expect("write should succeed");
        let recovered = fs.read_inode(5).expect("read should succeed");

        assert_eq!(recovered.inode, 5);
        assert_eq!(recovered.size, 1234);
        assert_eq!(recovered.kind, FileKind::File);
    }

    #[test]
    fn test_write_stores_data_to_disk() {
        let tmp = NamedTempFile::new().unwrap();
        let fd = tmp.reopen().unwrap();
        let fs = FuseFS::new(fd, 4096, 1024, 1024);

        // allocate an inode manually
        let inode_idx = fs.allocate_inode().expect("should have free inodes");

        let inode = InodeAttributes {
            inode: inode_idx,
            size: 0,
            kind: FileKind::File,
            hardlinks: 1,
            mode: 0o644,
            ..Default::default()
        };
        fs.set_inode_table(inode_idx, inode);

        // write some data
        let data = b"hello world";
        let block_idx = fs.allocate_block().expect("should have free blocks");
        let write_offset = fs.offset_from_block_idx(block_idx);
        fs.block_store_fd.write_at(data, write_offset).expect("write should succeed");

        // read it back and verify
        let mut buf = vec![0u8; data.len()];
        fs.block_store_fd.read_at(&mut buf, write_offset).expect("read should succeed");
        assert_eq!(&buf, data);
    }

    #[test]
    fn test_read_write_extent_roundtrip() {
        let tmp = NamedTempFile::new().unwrap();
        let fd = tmp.reopen().unwrap();
        let fs = FuseFS::new(fd, 4096, 1024, 1024);

        // allocate an inode and a block
        let inode_idx = fs.allocate_inode().expect("should have free inodes");
        let block_idx = fs.allocate_block().expect("should have free blocks");

        // set up inode with extent pointing to the block
        let inode = InodeAttributes {
            inode: inode_idx,
            size: 11,
            kind: FileKind::File,
            hardlinks: 1,
            mode: 0o644,
            extent_index: [(block_idx, 1), (0,0), (0,0), (0,0), (0,0), (0,0), (0,0), (0,0)],
            ..Default::default()
        };
        fs.set_inode_table(inode_idx, inode);

        // write data directly to the block
        let data = b"hello world";
        let write_offset = fs.offset_from_block_idx(block_idx);
        fs.block_store_fd.write_at(data, write_offset).expect("write should succeed");

        // now read it back using the extent system
        let inode_table_binding = fs.inode_table.read().unwrap();
        let inode = inode_table_binding.get(inode_idx as usize).unwrap();

        let mut result = vec![0u8; data.len()];
        let read_offset = fs.offset_from_block_idx(inode.extent_index[0].0);
        fs.block_store_fd.read_at(&mut result, read_offset).expect("read should succeed");

        assert_eq!(&result, data);
    }

    #[test]
    fn test_create_allocates_unique_inodes() {
        let tmp = NamedTempFile::new().unwrap();
        let fd = tmp.reopen().unwrap();
        let fs = FuseFS::new(fd, 4096, 1024, 1024);

        // allocate first inode
        let inode1 = fs.allocate_inode().expect("should have free inodes");
        let inode2 = fs.allocate_inode().expect("should have free inodes");

        // verify they are different
        assert_ne!(inode1, inode2, "each file should get a unique inode");

        // set up inodes in the inode table
        let new_inode1 = InodeAttributes {
            inode: inode1,
            size: 0,
            kind: FileKind::File,
            hardlinks: 1,
            mode: 0o644,
            ..Default::default()
        };
        let new_inode2 = InodeAttributes {
            inode: inode2,
            size: 0,
            kind: FileKind::File,
            hardlinks: 1,
            mode: 0o644,
            ..Default::default()
        };
        fs.set_inode_table(inode1, new_inode1);
        fs.set_inode_table(inode2, new_inode2);

        // verify both inodes are in the inode table
        let inode_table_binding = fs.inode_table.read().unwrap();
        let stored_inode1 = inode_table_binding.get(inode1 as usize).unwrap();
        let stored_inode2 = inode_table_binding.get(inode2 as usize).unwrap();

        assert_eq!(stored_inode1.inode, inode1);
        assert_eq!(stored_inode2.inode, inode2);
        assert_eq!(stored_inode1.kind, FileKind::File);
        assert_eq!(stored_inode2.kind, FileKind::File);

        // verify both are added to dir_entries
        fs.dir_entries
            .write()
            .unwrap()
            .entry(1)
            .or_insert_with(Vec::new)
            .push((inode1, "file1.txt".to_string()));

        fs.dir_entries
            .write()
            .unwrap()
            .entry(1)
            .or_insert_with(Vec::new)
            .push((inode2, "file2.txt".to_string()));

        let dir_entries_binding = fs.dir_entries.read().unwrap();
        let entries = dir_entries_binding.get(&1).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].1, "file1.txt");
        assert_eq!(entries[1].1, "file2.txt");
    }

    #[test]
    fn test_lookup_finds_existing_file() {
        let tmp = NamedTempFile::new().unwrap();
        let fd = tmp.reopen().unwrap();
        let fs = FuseFS::new(fd, 4096, 1024, 1024);

        // allocate an inode and set it up
        let inode_idx = fs.allocate_inode().expect("should have free inodes");
        let new_inode = InodeAttributes {
            inode: inode_idx,
            size: 0,
            kind: FileKind::File,
            hardlinks: 1,
            mode: 0o644,
            ..Default::default()
        };
        fs.set_inode_table(inode_idx, new_inode);

        // add the file to dir_entries
        fs.dir_entries
            .write()
            .unwrap()
            .entry(1)
            .or_insert_with(Vec::new)
            .push((inode_idx, "test.txt".to_string()));

        // verify lookup finds the file
        let found = fs.find_dir_entry(1, std::ffi::OsStr::new("test.txt"));
        assert!(found.is_some(), "lookup should find the file");
        assert_eq!(found.unwrap(), inode_idx);

        // verify lookup returns none for a file that doesn't exist
        let not_found = fs.find_dir_entry(1, std::ffi::OsStr::new("missing.txt"));
        assert!(not_found.is_none(), "lookup should return none for missing file");
    }
}
