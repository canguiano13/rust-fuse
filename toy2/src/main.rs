//Simple FUSE implementation, cargo run and open the mount point to see new file "hello.txt" created

use fuser::{
    FileAttr, FileType, Filesystem, ReplyAttr, ReplyData, ReplyDirectory, ReplyEntry, Request, 
    INodeNo, Generation, Errno, Config, FileHandle, OpenFlags, LockOwner // Added FileHandle here
};
use std::ffi::OsStr;
use std::time::{Duration, UNIX_EPOCH};

const TTL: Duration = Duration::from_secs(1);

struct SimpleFS;

impl Filesystem for SimpleFS {
    fn lookup(&self, _req: &Request, _parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        if name.to_str() == Some("hello.txt") {
            reply.entry(&TTL, &attr(INodeNo(2), 13), Generation(0));
        } else {
            reply.error(Errno::from_i32(libc::ENOENT));
        }
    }

    fn getattr(&self, _req: &Request, ino: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
        match ino.0 {
            1 => reply.attr(&TTL, &attr(INodeNo(1), 0)),
            2 => reply.attr(&TTL, &attr(INodeNo(2), 13)),
            _ => reply.error(Errno::from_i32(libc::ENOENT)),
        }
    }

    fn readdir(&self, _req: &Request, _ino: INodeNo, _fh: FileHandle, offset: u64, mut reply: ReplyDirectory) {
        let entries = vec![
            (INodeNo(1), FileType::Directory, "."),
            (INodeNo(2), FileType::RegularFile, "hello.txt"),
        ];
        
        for (i, entry) in entries.into_iter().enumerate().skip(offset as usize) {
            if reply.add(entry.0, (i + 1) as u64, entry.1, entry.2) {
                break;
            }
        }
        reply.ok();
    }

    fn read(&self, _req: &Request, ino: INodeNo, _fh: FileHandle, offset: u64, _size: u32, _flags: OpenFlags, _lock: Option<LockOwner>, reply: ReplyData) {
        if ino.0 == 2 {
            let content = "Hello World!\n";
            let bytes = content.as_bytes();
            if offset < bytes.len() as u64 {
                reply.data(&bytes[offset as usize..]);
            } else {
                reply.data(&[]);
            }
        } else {
            reply.error(Errno::from_i32(libc::ENOENT));
        }
    }
}

fn attr(ino: INodeNo, size: u64) -> FileAttr {
    FileAttr {
        ino, size, blocks: 1, atime: UNIX_EPOCH, mtime: UNIX_EPOCH, ctime: UNIX_EPOCH, crtime: UNIX_EPOCH,
        kind: if ino.0 == 1 { FileType::Directory } else { FileType::RegularFile },
        perm: if ino.0 == 1 { 0o755 } else { 0o644 },
        nlink: 1, uid: 1000, gid: 1000, rdev: 0, flags: 0, blksize: 512,
    }
}

fn main() {
    let mountpoint = std::env::args().nth(1).expect("Provide a mount point");
    fuser::mount2(SimpleFS, mountpoint, &Config::default()).unwrap();
}