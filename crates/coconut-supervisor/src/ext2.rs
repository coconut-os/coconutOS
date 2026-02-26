//! Read-only ext2 filesystem parser operating on a static byte slice.
//!
//! Supports rev 0 ext2 with 1024-byte blocks and direct block pointers only
//! (files up to 12 KiB). Used for reading the embedded ramdisk image.

const EXT2_MAGIC: u16 = 0xEF53;
const ROOT_INODE: u32 = 2;

/// Parsed ext2 filesystem metadata, initialized from the superblock.
struct Ext2Fs {
    data: *const u8,
    len: usize,
    block_size: u32,
    inodes_per_group: u32,
    inode_size: u32,
    inode_table_block: u32,
}

static mut FS: Ext2Fs = Ext2Fs {
    data: core::ptr::null(),
    len: 0,
    block_size: 0,
    inodes_per_group: 0,
    inode_size: 0,
    inode_table_block: 0,
};

/// Parsed inode data.
pub struct Inode {
    pub mode: u16,
    pub size: u32,
    pub blocks: [u32; 12],
}

fn read_u16_le(data: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([data[offset], data[offset + 1]])
}

fn read_u32_le(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}

/// Initialize the ext2 parser from a static byte slice.
/// Panics if the superblock magic is invalid.
pub fn init(data: &'static [u8]) {
    // Superblock is at byte offset 1024
    let magic = read_u16_le(data, 1024 + 56);
    assert!(magic == EXT2_MAGIC, "ext2: bad superblock magic");

    let log_block_size = read_u32_le(data, 1024 + 24);
    let block_size = 1024u32 << log_block_size;
    let inodes_per_group = read_u32_le(data, 1024 + 40);

    // Rev 0: inode size is always 128
    let rev_level = read_u32_le(data, 1024 + 76);
    let inode_size = if rev_level == 0 { 128 } else {
        read_u16_le(data, 1024 + 88) as u32
    };

    // Block Group Descriptor Table starts at block 2 (for 1024-byte blocks)
    let bgdt_offset = 2 * block_size as usize;
    let inode_table_block = read_u32_le(data, bgdt_offset + 8);

    unsafe {
        let fs = &mut *(&raw mut FS);
        fs.data = data.as_ptr();
        fs.len = data.len();
        fs.block_size = block_size;
        fs.inodes_per_group = inodes_per_group;
        fs.inode_size = inode_size;
        fs.inode_table_block = inode_table_block;
    }
}

/// Get the raw data slice from the static FS.
fn fs_data() -> &'static [u8] {
    unsafe {
        let fs = &*(&raw const FS);
        core::slice::from_raw_parts(fs.data, fs.len)
    }
}

/// Read an inode by number (1-based).
pub fn read_inode(ino: u32) -> Option<Inode> {
    if ino == 0 {
        return None;
    }

    let fs = unsafe { &*(&raw const FS) };
    let index = (ino - 1) as usize;
    let offset = (fs.inode_table_block as usize * fs.block_size as usize)
        + index * fs.inode_size as usize;

    let data = fs_data();
    if offset + fs.inode_size as usize > data.len() {
        return None;
    }

    let mode = read_u16_le(data, offset);
    let size = read_u32_le(data, offset + 4);

    let mut blocks = [0u32; 12];
    for i in 0..12 {
        blocks[i] = read_u32_le(data, offset + 40 + i * 4);
    }

    Some(Inode { mode, size, blocks })
}

/// Look up a path (e.g., "/hello.txt") and return its inode number.
/// Only supports paths in the root directory (one level deep).
pub fn lookup(path: &[u8]) -> Option<u32> {
    // Strip leading '/'
    let name = if !path.is_empty() && path[0] == b'/' {
        &path[1..]
    } else {
        path
    };

    if name.is_empty() {
        return Some(ROOT_INODE);
    }

    // Read root directory inode
    let root = read_inode(ROOT_INODE)?;

    // Scan directory entries in root's data blocks
    let data = fs_data();
    let fs = unsafe { &*(&raw const FS) };
    let block_size = fs.block_size as usize;

    for i in 0..12 {
        let block_nr = root.blocks[i];
        if block_nr == 0 {
            break;
        }

        let block_offset = block_nr as usize * block_size;
        let mut pos = 0;

        while pos + 8 <= block_size {
            let entry_offset = block_offset + pos;
            let inode_nr = read_u32_le(data, entry_offset);
            let rec_len = read_u16_le(data, entry_offset + 4) as usize;
            let name_len = read_u16_le(data, entry_offset + 6) as usize;

            if rec_len == 0 {
                break;
            }

            if inode_nr != 0 && name_len == name.len() {
                let entry_name = &data[entry_offset + 8..entry_offset + 8 + name_len];
                if entry_name == name {
                    return Some(inode_nr);
                }
            }

            pos += rec_len;
        }
    }

    None
}

/// Read file data from an inode into a buffer, starting at the given offset.
/// Returns the number of bytes read.
pub fn read_data(inode: &Inode, offset: u32, buf: &mut [u8]) -> usize {
    if offset >= inode.size {
        return 0;
    }

    let fs = unsafe { &*(&raw const FS) };
    let data = fs_data();
    let block_size = fs.block_size as usize;
    let remaining = (inode.size - offset) as usize;
    let to_read = buf.len().min(remaining);

    let mut bytes_read = 0;
    let mut file_offset = offset as usize;

    while bytes_read < to_read {
        let block_index = file_offset / block_size;
        if block_index >= 12 {
            break; // Only direct blocks
        }

        let block_nr = inode.blocks[block_index];
        if block_nr == 0 {
            break;
        }

        let offset_in_block = file_offset % block_size;
        let available = block_size - offset_in_block;
        let chunk = available.min(to_read - bytes_read);

        let src_offset = block_nr as usize * block_size + offset_in_block;
        buf[bytes_read..bytes_read + chunk]
            .copy_from_slice(&data[src_offset..src_offset + chunk]);

        bytes_read += chunk;
        file_offset += chunk;
    }

    bytes_read
}

/// Count files in the root directory (excluding "." and "..").
pub fn file_count() -> usize {
    let root = match read_inode(ROOT_INODE) {
        Some(r) => r,
        None => return 0,
    };

    let data = fs_data();
    let fs = unsafe { &*(&raw const FS) };
    let block_size = fs.block_size as usize;
    let mut count = 0;

    for i in 0..12 {
        let block_nr = root.blocks[i];
        if block_nr == 0 {
            break;
        }

        let block_offset = block_nr as usize * block_size;
        let mut pos = 0;

        while pos + 8 <= block_size {
            let entry_offset = block_offset + pos;
            let inode_nr = read_u32_le(data, entry_offset);
            let rec_len = read_u16_le(data, entry_offset + 4) as usize;
            let name_len = read_u16_le(data, entry_offset + 6) as usize;

            if rec_len == 0 {
                break;
            }

            if inode_nr != 0 {
                // Skip "." and ".."
                let entry_name = &data[entry_offset + 8..entry_offset + 8 + name_len];
                if entry_name != b"." && entry_name != b".." {
                    count += 1;
                }
            }

            pos += rec_len;
        }
    }

    count
}
