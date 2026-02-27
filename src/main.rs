use std::fs::File;
use std::mem;
use bitmaps::Bitmap;
use std::os::unix::fs::FileExt; //for read write
use clap::Parser;

use log::LevelFilter;
use log::debug;
use log::error;
use log::info;
use log::warn;

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
    fn new(block_size: u32, num_inodes: u64, num_blocks: u64) {
        // Use std::mem.size_of to get an aligned size calculation.
        let sb_size = size_of<Superblock>();
        Superblock {
            fsid: FSID,
            block_size,
            num_inodes,
            num_blocks,
            bitmap_start: sb_size,
            // We divide by 8 since addresses (basically pointers to locations
            // in a file) are in terms of bytes (not bits).
            itable_start: bitmap_start + (num_inodes / 8) + (num_blocks / 8),
            data_start: itable_start + size_of<InodeAttributes>() * num_inodes,
        };
    }
}

// The first value is the start location; the second value is the extent length.
type Extent = (u64, u64);

// inode structure
// -- pointers to data blocks
// TODO: Figure out what's happening here with the derive thing --- was it assuming
// use of the serde crate? Also, we need to figure out how to get the exact size
// after serialization for these inode structures so we can properly compute
// offset values for data blocks.
#[derive(Serialize, Deserialize)]
struct InodeAttributes {
    pub inode: u64,
    pub open_file_handles: u64, // Ref count of open file handles to this inode
    pub size: u64,
    pub last_accessed: (i64, u32),
    pub last_modified: (i64, u32),
    pub last_metadata_changed: (i64, u32),
    pub kind: FileKind, // TODO: Why does simple.rs use FileKind and not just fuser::FileType?
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

// table/map with inodes (inode number -> inode structure)
// Options for inode table data structure:
// - hashmap
// - b-tree
// - flat data structure: vector or array
// NOTE: Let's start with a flat data structure. This should be sized
// according to the max # of inodes allowed in the inode bitmap, i.e 32K.
type InodeTable = [InodeAttributes];

struct FuseFS {
    superblock: Superblock,
    inode_bitmap: [Bitmap],
    data_bitmap: [Bitmap],
    inode_table: InodeTable,
    block_store_fd: File,
}

impl FuseFS {
    fn new(block_size: u32, num_inodes: u64, num_blocks: u64, store_fp: str) -> Result<FuseFS> {
        debug!("Creating filesystem..");
        let superblock = Superblock::new(block_size, num_inodes, num_blocks);
        // TODO: Check if this syntax actually does what you want it to do.
        let inode_bitmap = [Bitmap<1024>(); (num_inodes / 1024)];
        let data_bitmap = [Bitmap<1024>(); (num_blocks / 1024)];
        let inode_table = [InodeAttributes; num_inodes];
        let block_store_fd = File::create(store_fp)?;
        FuseFS {
            superblock,
            inode_bitmap,
            data_bitmap,
            inode_table,
            block_store_fd,
        }
    }

    fn load(store_fp: str) -> Result<FuseFS> {
        // Read file superblock
        // Read in all data needed to re-create the filesystem
        // Return the loaded filesystem
    }
}

// for FUSE:
// - implement the Filesystem trait
// - do basic setup stuff in main()
impl Filesystem for FuseFS {
    // TODO: Fill out this stuff.
    // Adding read and write
    fn read(&self, _req: &Request, ino: INodeNo, _fh: FileHandle, offset: u64, size: u32, _flags: OpenFlags, _lock: Option<LockOwner>, reply: ReplyData) {
        // check if inode really exist
        if !self.inode_bitmap.get(ino.0 as usize) {
            // If the bit is 0, file doesn't exist
            reply.error(libc::ENOENT);
            return;
        }

        // calculate start location, start from superblock and jump slots
        let file_base_address = self.superblock.data_start + (ino.0 * self.superblock.block_size as u64);

        // create buffer, value starts at 0, type is u8
        let mut buffer = vec![0u8; size as usize];

        // Adding file to buffer at exact offset address, check if read is successful and return data, otherwise just return error
        match self.block_store_fd.read_at(&mut buffer, file_base_address + offset as u64) {
            Ok(bytes_read) => reply.data(&buffer[..bytes_read]),
            Err(_) => reply.error(libc::EIO),
        }
    }
}

#[derive(Parser)]
#[command(version, author = "Lucas Du, Carlos Anguiano, Simon Zheng")]
// TODO: Need to add to this.
struct Args {
    /// Set local directory used to store data
    #[clap(long, default_value = "/tmp/fuser")]
    data_dir: String,

    // TODO: make positional like other examples.
    /// Act as a client, and mount FUSE at given path
    #[clap(long, default_value = "")]
    mount_point: String,

    /// Mount FUSE with direct IO
    #[clap(long, requires = "mount_point")]
    direct_io: bool,

    /// Automatically unmount FUSE when process exits
    #[clap(long)]
    auto_unmount: bool,

    /// Run a filesystem check
    #[clap(long)]
    fsck: bool,

    /// Enable setuid support when run as root
    #[clap(long)]
    suid: bool,

    #[clap(long, default_value_t = 1)]
    n_threads: usize,

    /// Sets the level of verbosity
    #[clap(short, action = clap::ArgAction::Count)]
    v: u8,

    #[clap(long, default_value_t = 4096)]
    block_size: u32,

    // TODO: Add in num inodes and blocks bitmaps.
}


fn main() {
    let args = Args::parse();
    // TODO: One of the arguments here should be the name of the file to allocate
    // as the backing "block store" for our filesystem. I guess this file could
    // have any suffix, although it would be nice to enforce a .fs format name.

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


    let mut cfg = Config::default();
    cfg.mount_options = vec![MountOption::FSName("fuser".to_string())];

    if args.suid {
        info!("setuid bit support enabled");
        cfg.mount_options.push(MountOption::Suid);
    }
    if args.auto_unmount {
        cfg.mount_options.push(MountOption::AutoUnmount);
    }
    if let Ok(enabled) = fuse_allow_other_enabled() {
        if enabled {
            cfg.acl = SessionACL::All;
        }
    } else {
        eprintln!("Unable to read /etc/fuse.conf");
    }
    if cfg.mount_options.contains(&MountOption::AutoUnmount) && cfg.acl != SessionACL::RootAndOwner
    {
        cfg.acl = SessionACL::All;
    }

    cfg.n_threads = Some(args.n_threads);

    let result = fuser::mount(
        SimpleFS::new(args.data_dir, args.direct_io, args.suid),
        &args.mount_point,
        &cfg,
    );
    if let Err(e) = result {
        // Return a special error code for permission denied, which usually indicates that
        // "user_allow_other" is missing from /etc/fuse.conf
        if e.kind() == ErrorKind::PermissionDenied {
            error!("{e}");
            std::process::exit(2);
        } else {
            error!("{e}");
        }
    }
}
