use std::fs::File;

// superblock
struct Superblock {
    block_size: u32,
    num_inodes: u32,
    num_free_inodes: u32,
    num_blocks: u32,
    num_free_blocks: u32
}

// TODO: Think more about how to represent bitmaps in memory. Maybe use bitmap crate?
type InodeBitmap = [bool];
type DataBitmap = [bool];

// table/map with inodes (inode number -> inode structure)
// inode structure
// -- pointers to data blocks
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
    pub xattrs: BTreeMap<Vec<u8>, Vec<u8>>, // TODO: Figure out if we need this.
}

// Options for inode table data structure:
// - hashmap
// - b-tree
// - straight line data structure: vector or array
type InodeTable = [Option<InodeAttributes>];

// actual data region
type DataRegion = File;

struct FuseFS {
    superblock: Superblock,
    inode_bitmap: InodeBitmap,
    data_bitmap: DataBitmap,
    inode_table: InodeTable,
    data_region: DataRegion,
}

impl FuseFS {
    fn new() -> FuseFS {
        debug!("Creating filesystem..");
        let superblock = Superblock::new();
        let inode_bitmap = InodeBitmap::new();
        let data_bitmap = DataBitmap::new();
        let inode_table = InodeTable::new();
        let data_region = DataRegion::new();
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
