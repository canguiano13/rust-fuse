use std::fs::File;
use std::mem;
use bitmaps::Bitmap;


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
    fn new(bsize_bytes: u32, bmapsize_bytes: u64) {
        // Use std::mem.size_of to get an aligned size calculation.
        let sb_size = size_of<Superblock>();
        Superblock {
            fsid: FSID,
            block_size: bsize_bytes,
            num_blocks: bmapsize_bytes * 8,
            num_inodes: bmapsize_bytes * 8,
            bitmap_start: sb_size,
            itable_start: bitmap_start + 2 * bmapsize,
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
type InodeTable = [Option<InodeAttributes>];

// actual data region
// TODO: basically just read sections from/write sections to the file when needed.
// TODO: we might not need a specific type for this if we just represent the
// entire block store as a file. Alternatively, it would be interesting if we
// wanted to keep pieces of these files in memory, in which case perhaps it would
// be useful to have a separate data structure to store the data region.
// type DataRegion = File;

struct FuseFS {
    superblock: Superblock,
    inode_bitmap: Bitmap,
    data_bitmap: Bitmap,
    inode_table: InodeTable,
    block_store_fd: File,
}

impl FuseFS {
    // TODO: new() needs to take some configuration arguments.
    fn new() -> FuseFS {
        debug!("Creating filesystem..");
        let superblock = Superblock::new();
        let inode_bitmap = InodeBitmap::new();
        let data_bitmap = DataBitmap::new();
        let inode_table = InodeTable::new();
        // TODO: Figure out how you want to manage the data region. I think it
        // makes sense to just have an open File object representing the entire
        // block, and then just use the offsets to write to the proper places
        // in the data region.
        let block_store_fd = File;
        FuseFS {
            superblock,
            inode_bitmap,
            data_bitmap,
            inode_table,
            data_region
        }
    }
}

// for FUSE:
// - implement the Filesystem trait
// - do basic setup stuff in main()
impl Filesystem for FuseFS {
    // TODO: Fill out this stuff.
}


fn main() {
    let args = Args::parse();
    // TODO: One of the arguments here should be the name of the file to allocate
    // as the backing "block store" for our filesystem. I guess this file could
    // have any suffix, although it would be nice to enforce a .fs format name.

    // let log_level = match args.v {
    //     0 => LevelFilter::Error,
    //     1 => LevelFilter::Warn,
    //     2 => LevelFilter::Info,
    //     3 => LevelFilter::Debug,
    //     _ => LevelFilter::Trace,
    // };
    // env_logger::builder()
    //     .format_timestamp_nanos()
    //     .filter_level(log_level)
    //     .init();

    // let mut cfg = Config::default();
    // cfg.mount_options = vec![MountOption::FSName("fuser".to_string())];

    // if args.suid {
    //     info!("setuid bit support enabled");
    //     cfg.mount_options.push(MountOption::Suid);
    // }
    // if args.auto_unmount {
    //     cfg.mount_options.push(MountOption::AutoUnmount);
    // }
    // if let Ok(enabled) = fuse_allow_other_enabled() {
    //     if enabled {
    //         cfg.acl = SessionACL::All;
    //     }
    // } else {
    //     eprintln!("Unable to read /etc/fuse.conf");
    // }
    // if cfg.mount_options.contains(&MountOption::AutoUnmount) && cfg.acl != SessionACL::RootAndOwner
    // {
    //     cfg.acl = SessionACL::All;
    // }

    cfg.n_threads = Some(args.n_threads);
    // TODO: Allow various additional configuration options for superblock
    // metadata, but give defaults.
    // TODO: Maybe allow groups, i.e. separate blocks
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
