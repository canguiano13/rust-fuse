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
- No support for xattrs.

### to-dos (by Friday 2/27/26!)
1. Get configuration set up for various filesystem knobs, implement filesystem initialization function.
2. Write up helper functions for allocating space on the "block storage" file.
3. Add in serialization and implement calculation for offset/"address" calculation.
4. Implement file allocation flow: checks in inode and data bitmaps, block allocation.
5. Implement file read and writes, including metadata changes.
6. Implement directory management.
7. Implement link counters and then file deletes.
8. Implement basic access control with modes and UID/GID.
9. Implement file renames.
10. Go through rest of fuser Filesystem implementation, fill out other functions.
11. Work through and fix edge cases, i.e. out-of-space errors.

## journalFS
An extension of minimalFS with journaling for a form of crash-recovery/consistency.

## mulcoreFS
A partial rewrite of minimalFS/journalFS that aims to use SIM Commutativity principles to guide multicore scalable design.
