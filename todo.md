- Add directory entries data structure the system, with proper serialization.
- Implement functions for inode updates, bitmap updates, and file read/writes.
---- fix allocate block function
---- fix issues with indexing into inode table and inode allocation due to inode numbers starting at 1
---- set backing file size to be static

---- serialize with postcard instead of serde_json
---- run clippy and cargo fmt
- Fix all existing, broken Filesystem implementations.
- Add the Filesystem functions I assigned myself.
- Add basic UID/GID ACL stuff.

extra shit:
- Actually serialize metadata to vec of u8s and use rkyv (or another more efficient serialization format
like bincode if need be).
- Break up and test next_free_block_extent function.
