//constants

//4kb blocks
const BLOCK_SIZE: u32 = 4096;
//4mb total filesystem size
const NUM_BLOCKS: u32 = 1024;
//total number of possible files
const NUM_INODES: u32 = 256;

////superblock
//information about the filesystem as a whole
struct Superblock{
    block_size: u32,
    num_inodes: u32,
    num_free_inodes: u32,
    num_blocks: u32,
    num_free_blocks: u32
}
//define functions for the superblock
impl Superblock{
    fn new() -> Superblock{
        println!("created a superblock..");
        Superblock{
            block_size: BLOCK_SIZE,
            num_blocks: NUM_BLOCKS,
            num_inodes: NUM_INODES,
            num_free_inodes: NUM_INODES,
            num_free_blocks: NUM_BLOCKS
        }
    }
    //some other functions for this impl
    //create() -> create new file
    //mkdir() -> create a directory
    //lookup() -> find inode if it exists
    //unlink() -> remove an inode
    //rmdir() -> remove a directory
    //create_link()?
    //
}


////inode bitmap
//information about which spaces in the inode table are currently free
struct InodeBitmap{
    //array of bits
    bits: [bool; NUM_INODES as usize]
}
impl InodeBitmap{
    //return a new bitmap

    fn new() -> InodeBitmap{
        println!("Created an inode bitmap..");

        //initially all spaces are free
        let bits = [false; NUM_INODES as usize];
        InodeBitmap{
            bits
        }
    }
    //some other functions
    //markFree() -> set a location in the inode bitmap as free
    //markUsed() -> set a location in the inode bitmap as not free
    //isFree() -> check if a certain location in the bitmap is free
    //findFree() -> find next free position
}

////data bitmap
//information about which blocks in the data region are currently free
struct DataBitmap{
    //array of bits
    bits: [bool; NUM_BLOCKS as usize]
}
//might even just be able to create 1 generic "bitmap" struct and initialize two
//as needed
impl DataBitmap{
    fn new() -> DataBitmap{
        println!("Created a new data bitmap..");
        let bits = [false; NUM_BLOCKS as usize];
        DataBitmap{
            bits
        }
    }

    //some other functions for this impl
    //markFree() -> set a location in the data bitmap as free
    //markUsed() -> set a location in the data bitmap as not free
    //isFree() -> check if a certain location in the bitmap is free
    //findFree() -> find next free position
}

////inode struct
struct Inode{
    size: u32, //size of the file in bytes
    uid: u32
    //could also have other stuff in here like
    //type (i.e. directory or file), gid?, time created, access time, modify time,
    //pointer to blocks of memory in data region will also live in here
}
impl Inode{
    fn new(size:u32) -> Inode{
        println!("Created a single inode..");
        Inode{
            size,
            //could use a functin for this maybe? for now just setting to root
            uid: 0
        }
    }
    //other functions that could go in this impl
    //getuid()
    //setuid()
    //allocate() -> allocate space in the data region
    //deallocate() -> free used space in the data region
}

////inode table
////table holding the inodes
struct InodeTable{
    //struct holding either None or inode structs
    inodes: Vec<Option<Inode>>
}
impl InodeTable{
    fn new() -> InodeTable{
        println!("creating inode table..");

        let mut inodes = Vec::new();
        for i in 0..NUM_INODES{
            inodes.push(None);
        }

        InodeTable{
            inodes
        }
}

    //other functions for this impl
    //getInode() -> search for a certain inode
    //addInode() -> add an inode to the table
    //clearInode() -> remove an inode from the table
    //updateInode()??
    //getAttr() -> get some attribute from an inode
}

////data region
//is the data region a struct??
struct DataRegion{
    //vector of bytes?
    data_blocks: Vec<[u8; BLOCK_SIZE as usize]>
}
impl DataRegion{
    fn new() -> DataRegion{
        //empty data region
        let mut data_blocks = Vec::new();
        for i in 0..NUM_BLOCKS{
            data_blocks.push([0u8; BLOCK_SIZE as usize]);
        }
        DataRegion{
            data_blocks
        }
    }
    //other functiosn..
    //read() -> read a block of data
    //write() -> write to a block of data
    //clear() -> clear or mark as empty
}

//filesystem as a whole
struct FuseFS{
    superblock: Superblock,
    inode_bitmap: InodeBitmap,
    data_bitmap: DataBitmap,
    inode_table: InodeTable,
    data_region: DataRegion,
}
impl FuseFS{
    fn new() -> FuseFS{
        println!("Creating filesystem..");
        let superblock = Superblock::new();
        let inode_bitmap = InodeBitmap::new();
        let data_bitmap = DataBitmap::new();
        let inode_table = InodeTable::new();
        let data_region = DataRegion::new();

        FuseFS{
            superblock,
            inode_bitmap,
            data_bitmap,
            inode_table,
            data_region
        }
    }
}


fn main() {
    let _fs = FuseFS::new();
}
