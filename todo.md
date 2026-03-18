- X Add directory entries data structure the system, with proper serialization.
- X Implement functions for inode updates, bitmap updates, and file read/writes.

X ---- finish simple version of create function
X ---- fix issues with indexing into inode table and inode allocation due to inode numbers starting at 1
X ---- set backing file size to be static
X ---- fix problem with mutability of bitmaps (i think this is done?)
X ---- make sure that create actually works without an input/output error
X ---- finish implementations of write/read file and try to clean things up/simplify a bit
--- could not clean up :(
---- finish Filesystem implementation of read/write and test
---- run clippy and cargo fmt

---- serialize with postcard instead of serde_json

- Fix all existing, broken Filesystem implementations.
- Add the Filesystem functions I assigned myself.
- Add basic UID/GID ACL stuff.

- a big challenge: figuring out serialization of metadata and doing conversions in code
--- a compromise: two files, one for metadata serialization, the other that actually holds backing store
- another challenge: implementing the logic to handle reading/writing file extents

extra shit:
- Actually serialize metadata to vec of u8s and use rkyv (or another more efficient serialization format
like bincode if need be).
- Break up and test next_free_block_extent function.
