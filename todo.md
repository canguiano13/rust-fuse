- Add directory entries data structure the system, with proper serialization.
- Implement functions for inode updates, bitmap updates, and file read/writes.
---- run clippy and cargo fmt
- Fix all existing, broken Filesystem implementations.
- Add the Filesystem functions I assigned myself.
- Add basic UID/GID ACL stuff.

extra shit:
- Actually serialize metadata to vec of u8s and use rkyv (or another more efficient serialization format
like bincode if need be).
- Break up and test next_free_block_extent function.
