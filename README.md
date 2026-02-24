# rust-fuse
Rust-based FUSE filesystem aimed to improved memory-safety and, ideally, adhere to SIM Commutativity principles.

## minimalFS
The first filesystem is an extremely minimal implementation in the style of vsfs (from OSTEP). The following design choices are made:
- Simulate contiguous block storage by allocating a single large file (using the OS filesystem) to act as the backing store for the entire FUSE filesystem.
- Everything is stored on disk in that single large file, with the following structure: [superblock|inode-bitmap|data-bitmap|inode-table|data-region].
- The inode table is stored as a flat array, with indices denoting inode number.
- Use a list of extents to track data blocks for specific inodes.
- Use fixed size bitmaps to store free/allocated metadata for both inodes and data blocks.
- Extremely minimal access control: only mode bits and UID/GID.
