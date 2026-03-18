# rust-fuse
Rust-based FUSE filesystem aimed to improved memory-safety and, ideally, adhere to SIM Commutativity principles.

## Authors
Carlos Anguiano, Lucas Du, Simon Zheng

## Overview
This filesystem is implemented as a FUSE userspace daemon. When a user application makes a filesystem call, the request travels through the Linux Virtual Filesystem (VFS) layer to the FUSE kernel module, which forwards it to our userspace daemon. The daemon processes the request, reads or writes data to a backing file as needed, and sends a response back through the kernel module to the calling application.

## minimalFS
The first filesystem is an extremely minimal implementation in the style of vsfs (from OSTEP). The following design choices are made:
- Simulate contiguous block storage by allocating a single large file (using the OS filesystem) to act as the backing store for the entire FUSE filesystem.
- Everything is stored on disk in that single large file, with the following structure: [superblock|inode-bitmap|data-bitmap|inode-table|data-region].
- The inode table is stored as a flat array, with indices denoting inode number.
- Use a list of extents to track data blocks for specific inodes.
- Use fixed size bitmaps to store free/allocated metadata for both inodes and data blocks.
- Extremely minimal access control: only mode bits and UID/GID.
- No support for xattrs.

### Supported Operations
- `create` — create a new file
- `read` — read file data using extent-based addressing
- `write` — write file data with automatic block allocation
- `lookup` — look up a file by name in a directory
- `readdir` — list directory contents
- `getattr` — get file attributes
- `symlink` / `readlink` — create and read symbolic links
- `link` / `unlink` — create and remove hard links

## Building
```bash
git clone 
cd rust-fuse
cargo build --release
```

### Mounting the Filesystem
```bash
# Create a mount point
mkdir /tmp/mnt

cargo run -- /tmp/mnt /tmp/blockfile -vvv

```

### Using the Filesystem
```bash
# Create a file
echo "hello world" > /tmp/mnt/hello.txt

# Read a file
cat /tmp/mnt/hello.txt

# List directory contents
ls /tmp/mnt

# Create a symbolic link
ln -s /tmp/mnt/hello.txt /tmp/mnt/hello_link.txt

# Create a hard link
ln /tmp/mnt/hello.txt /tmp/mnt/hello_hard.txt

# Remove a file
rm /tmp/mnt/hello.txt
```
### Unmounting
```bash
fusermount -u /tmp/mnt
```

## journalFS
An extension of minimalFS with journaling for a form of crash-recovery/consistency.

## mulcoreFS
A partial rewrite of minimalFS/journalFS that aims to use SIM Commutativity principles to guide multicore scalable design.
