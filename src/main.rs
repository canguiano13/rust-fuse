use std::fs::File;
use std::io;
use std::io::Error;
// use std::io::prelude::*;
use std::io::ErrorKind;
use std::io::BufReader;
use std::io::BufRead;
use std::path::PathBuf;
use std::path::Path;
use std::os::unix::fs::FileExt;
use std::fs::OpenOptions;

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

// TODO: Why does simple.rs use FileKind and not just fuser::FileType? Should
// we continue to do this?
#[derive(Serialize, Deserialize, Copy, Clone, PartialEq, Default)]
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

// #[derive(PartialOrd, Ord, PartialEq, Eq, Clone, Debug, Serialize, Deserialize)]
// struct FSBitmap(Bitmap<1024>);

// TODO: You might just need to implement your own bitmap. This seems very
// painful. OK, or here's a hack: just iterate over each set bit in the bitmap
// and construct u8s (or something like that), add them all into an array, then
// just convert them to and from that representation before calling out to the
// default serde serializers.
// You can either do this in here, or for the even hackier way: just have another
// function that converts the regular Meta struct to one that's serializable
// (basically just bitmaps converted in the way described above) and then
// directly call serialize on that.
// The alternative is also just to say fuck it and either create bitmaps out of
// bools or just serialize to a vec of bools.
// NOTE UPDATE (March 4, 2026): Said fuck it.
// impl Serialize for FSBitmap {
//     fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
//     where
//         S: Serializer,
//     {
//         let mut u8vec: Vec<u8>;
//         let max_bit_index = match self.last_index() {
//             None => return
//         };
//         let u8_bit_i = 0;
//         let u8_construct = 0;
//         for i in 0..(max_bit_index+1) {
//             if self.get(i) {

//             } else {

//             }
//         }

//     }
// }

// // Helper functions to make dealing with the wrapped version of Bitmap easier.
// use std::ops::{Deref, DerefMut};
// impl Deref for FSBitmap {
//     type Target = Bitmap<1024>;

//     fn deref(&self) -> &Self::Target {
//         &self.0
//     }
// }
// impl DerefMut for FSBitmap {
//     fn deref_mut(&mut self) -> &mut Self::Target {
//         &mut self.0
//     }
// }

// NOTE: Let's just do something very stupid and hacky for now, and convert all
// Bitmaps into vectors of u8s, then just directly call serde_json serialization
// on that (which should work out-of-the-box). Ideally, we would have a custom
// serializer (i.e see above), but...I can't be bothered right now. I just want
// serialization to work so that at least we can persist to disk.

#[derive(Serialize, Deserialize)]
struct MetaSerializable {
    superblock: Superblock,
    // NOTE: It would actually be great to have a u8 vec for higher data density,
    // but I'm really strapped for time and don't want to deal with even the
    // minor complexity of conversion from bits to u8. So bools it is!
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
            inode_table: self.inode_table.clone(),
        }
    }
}

struct Meta {
    superblock: Superblock,
    inode_bitmap: Vec<Bitmap<1024>>,
    data_bitmap: Vec<Bitmap<1024>>,
    inode_table: InodeTable,
}

impl Meta {
    fn to_meta_serializable(&self) -> MetaSerializable {
        let mut inode_bmap_bool: Vec<bool> = Vec::new();
        for chunk in &self.inode_bitmap {
            for i in 0..1024 {
                if chunk.get(usize::try_from(i).unwrap()) {
                    inode_bmap_bool.push(true)
                } else {
                    inode_bmap_bool.push(false)
                }
            }
        };
        let mut data_bmap_bool: Vec<bool> = Vec::new();
        for chunk in &self.data_bitmap {
            for i in 0..1024 {
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
            inode_table: self.inode_table.clone(),
        }
    }
}

struct FuseFS {
    fs_dir: PathBuf,
    meta: Meta,
    meta_fd: File,
    store_fd: File,
}

// Implement methods specific to FuseFS design and structure.
impl FuseFS {
    fn new(fs_dir_path: PathBuf, block_size: u32, num_inodes: u64, num_blocks: u64) -> Result<FuseFS, Error> {
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
                inode_bitmap: vec![Bitmap::<1024>::new(); num_inodes as usize / 1024],
                data_bitmap: vec![Bitmap::<1024>::new(); num_blocks as usize / 1024],
                inode_table: vec![InodeAttributes::default(); num_inodes as usize],
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

        info!("Created filesystem.");
        Ok(FuseFS {
            fs_dir: fs_dir_path,
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
        serde_json::to_writer(&self.meta_fd, &meta_ser).unwrap();
    }
}

// Implement the Filesystem trait to integrate FuseFS with fuser.
impl Filesystem for FuseFS {
    // Adding read and write
    // fn read(&self, _req: &Request, ino: INodeNo, _fh: FileHandle, offset: u64,
    //         size: u32, _flags: OpenFlags, _lock: Option<LockOwner>, reply: ReplyData) {
    //     // check if inode really exist
    //     if !self.inode_bitmap.get(ino.0 as usize) {
    //         // If the bit is 0, file doesn't exist
    //         reply.error(libc::ENOENT);
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
    //         Err(_) => reply.error(libc::EIO),
    //     }
    // }

    fn init(&mut self, _req: &Request, _config: &mut fuser::KernelConfig) -> Result<(), io::Error> {
        info!("Filesystem mounted successfully");
        // Set up root directory as inode 1
        let root_inode: usize = 0;
        let chunk = root_inode / 1024;
        let bit = root_inode % 1024;
        self.meta.inode_bitmap[chunk].set(bit, true);
        self.meta.inode_table[root_inode] = InodeAttributes {
            inode: root_inode as u64,
            size: 0,
            kind: FileKind::Directory,
            mode: 0o755,
            hardlinks: 2,
            uid: 0,
            gid: 0,
            // TODO: Do we really need this?
            ..Default::default()
        };
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
