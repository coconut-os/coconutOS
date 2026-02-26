//! Build script that generates a minimal ext2 filesystem image.
//!
//! Produces a 64 KiB (64 × 1024-byte blocks) rev 0 ext2 image containing:
//!   - Root directory with "hello.txt"
//!   - hello.txt contents: "Hello from coconutFS!\n"
//!
//! No external tools (mke2fs, etc.) are required.

use std::path::Path;

const BLOCK_SIZE: usize = 1024;
const TOTAL_BLOCKS: usize = 64;
const IMAGE_SIZE: usize = TOTAL_BLOCKS * BLOCK_SIZE;
const TOTAL_INODES: usize = 32;
const INODE_SIZE: usize = 128;

// Block assignments
const _BLOCK_BOOT: usize = 0;
const BLOCK_SUPERBLOCK: usize = 1;
const BLOCK_GROUP_DESC: usize = 2;
const BLOCK_BLOCK_BITMAP: usize = 3;
const BLOCK_INODE_BITMAP: usize = 4;
const BLOCK_INODE_TABLE: usize = 5; // blocks 5-8 (32 inodes × 128 bytes = 4096 = 4 blocks)
const BLOCK_ROOT_DIR: usize = 9;
const BLOCK_HELLO_TXT: usize = 10;

const FIRST_FREE_BLOCK: usize = 11;

fn write_u16_le(img: &mut [u8], offset: usize, val: u16) {
    img[offset] = val as u8;
    img[offset + 1] = (val >> 8) as u8;
}

fn write_u32_le(img: &mut [u8], offset: usize, val: u32) {
    img[offset] = val as u8;
    img[offset + 1] = (val >> 8) as u8;
    img[offset + 2] = (val >> 16) as u8;
    img[offset + 3] = (val >> 24) as u8;
}

fn main() {
    let out_dir = std::env::var("OUT_DIR").unwrap();
    let out_path = Path::new(&out_dir).join("rootfs.ext2");

    let mut img = vec![0u8; IMAGE_SIZE];

    // =========================================================================
    // Superblock (block 1, offset 1024)
    // =========================================================================
    let sb = BLOCK_SUPERBLOCK * BLOCK_SIZE;

    write_u32_le(&mut img, sb + 0, TOTAL_INODES as u32); // s_inodes_count
    write_u32_le(&mut img, sb + 4, TOTAL_BLOCKS as u32); // s_blocks_count
    write_u32_le(&mut img, sb + 8, 0); // s_r_blocks_count (reserved blocks)
    write_u32_le(&mut img, sb + 12, (TOTAL_BLOCKS - FIRST_FREE_BLOCK) as u32); // s_free_blocks_count
    write_u32_le(&mut img, sb + 16, (TOTAL_INODES - 11) as u32); // s_free_inodes_count (inodes 1-11 used)
    write_u32_le(&mut img, sb + 20, 1); // s_first_data_block (must be 1 for 1024-byte blocks)
    write_u32_le(&mut img, sb + 24, 0); // s_log_block_size (0 = 1024 bytes)
    write_u32_le(&mut img, sb + 28, 0); // s_log_frag_size
    write_u32_le(&mut img, sb + 32, TOTAL_BLOCKS as u32); // s_blocks_per_group
    write_u32_le(&mut img, sb + 36, TOTAL_BLOCKS as u32); // s_frags_per_group
    write_u32_le(&mut img, sb + 40, TOTAL_INODES as u32); // s_inodes_per_group
    // s_mtime, s_wtime = 0
    write_u16_le(&mut img, sb + 48, 0); // s_mnt_count
    write_u16_le(&mut img, sb + 50, u16::MAX); // s_max_mnt_count
    write_u16_le(&mut img, sb + 56, 0xEF53); // s_magic
    write_u16_le(&mut img, sb + 58, 1); // s_state = EXT2_VALID_FS
    write_u16_le(&mut img, sb + 60, 1); // s_errors = EXT2_ERRORS_CONTINUE
    // s_minor_rev_level = 0
    // s_lastcheck, s_checkinterval = 0
    // s_creator_os = 0 (Linux)
    write_u32_le(&mut img, sb + 76, 0); // s_rev_level = 0 (rev 0)
    // s_def_resuid, s_def_resgid = 0
    // Rev 0: inode size is fixed at 128, first non-reserved inode = 11

    // =========================================================================
    // Block Group Descriptor Table (block 2)
    // =========================================================================
    let bgd = BLOCK_GROUP_DESC * BLOCK_SIZE;

    write_u32_le(&mut img, bgd + 0, BLOCK_BLOCK_BITMAP as u32); // bg_block_bitmap
    write_u32_le(&mut img, bgd + 4, BLOCK_INODE_BITMAP as u32); // bg_inode_bitmap
    write_u32_le(&mut img, bgd + 8, BLOCK_INODE_TABLE as u32); // bg_inode_table
    write_u16_le(&mut img, bgd + 12, (TOTAL_BLOCKS - FIRST_FREE_BLOCK) as u16); // bg_free_blocks_count
    write_u16_le(&mut img, bgd + 14, (TOTAL_INODES - 11) as u16); // bg_free_inodes_count
    write_u16_le(&mut img, bgd + 16, 1); // bg_used_dirs_count

    // =========================================================================
    // Block Bitmap (block 3) — blocks 0-10 allocated (bit=1)
    // =========================================================================
    let bb = BLOCK_BLOCK_BITMAP * BLOCK_SIZE;
    // Blocks 0-10 allocated: bits 0-10 set = 0x07FF (lower 11 bits)
    write_u16_le(&mut img, bb, 0x07FF);

    // =========================================================================
    // Inode Bitmap (block 4) — inodes 1-11 allocated (bit=1)
    // =========================================================================
    let ib = BLOCK_INODE_BITMAP * BLOCK_SIZE;
    // Inodes 1-11 allocated: bits 0-10 set = 0x07FF
    // (inode numbers are 1-based, but bitmap bit 0 = inode 1)
    write_u16_le(&mut img, ib, 0x07FF);

    // =========================================================================
    // Inode Table (blocks 5-8)
    // =========================================================================
    let it = BLOCK_INODE_TABLE * BLOCK_SIZE;

    // Inode 2 (root directory) — offset = (2-1) * 128 = 128
    let root_ino = it + 1 * INODE_SIZE;
    write_u16_le(&mut img, root_ino + 0, 0x4000 | 0o755); // i_mode = S_IFDIR | 0755
    // i_uid = 0
    // i_size = one block (directory always uses full blocks)
    write_u32_le(&mut img, root_ino + 4, BLOCK_SIZE as u32);
    // i_atime, i_ctime, i_mtime, i_dtime = 0
    // i_gid = 0
    write_u16_le(&mut img, root_ino + 26, 2); // i_links_count = 2 (. and parent)
    write_u32_le(&mut img, root_ino + 28, 2); // i_blocks (in 512-byte sectors: 1024/512 = 2)
    // i_block[0] = block 9 (root dir data)
    write_u32_le(&mut img, root_ino + 40, BLOCK_ROOT_DIR as u32);

    // Inode 11 (hello.txt) — offset = (11-1) * 128 = 1280
    let hello_ino = it + 10 * INODE_SIZE;
    let hello_data = b"Hello from coconutFS!\n";
    let hello_size = hello_data.len() as u32;
    write_u16_le(&mut img, hello_ino + 0, 0x8000 | 0o644); // i_mode = S_IFREG | 0644
    write_u32_le(&mut img, hello_ino + 4, hello_size); // i_size
    write_u16_le(&mut img, hello_ino + 26, 1); // i_links_count = 1
    write_u32_le(&mut img, hello_ino + 28, 2); // i_blocks (in 512-byte sectors)
    write_u32_le(&mut img, hello_ino + 40, BLOCK_HELLO_TXT as u32); // i_block[0]

    // =========================================================================
    // Root Directory Data (block 9)
    // =========================================================================
    let rd = BLOCK_ROOT_DIR * BLOCK_SIZE;

    // Entry 1: "." → inode 2
    write_u32_le(&mut img, rd + 0, 2); // inode = 2
    write_u16_le(&mut img, rd + 4, 12); // rec_len = 12
    write_u16_le(&mut img, rd + 6, 1); // name_len = 1 (rev 0: 16-bit name_len)
    img[rd + 8] = b'.';

    // Entry 2: ".." → inode 2 (root's parent is itself)
    write_u32_le(&mut img, rd + 12, 2); // inode = 2
    write_u16_le(&mut img, rd + 16, 12); // rec_len = 12
    write_u16_le(&mut img, rd + 18, 2); // name_len = 2
    img[rd + 20] = b'.';
    img[rd + 21] = b'.';

    // Entry 3: "hello.txt" → inode 11
    // rec_len = rest of block = 1024 - 24 = 1000
    write_u32_le(&mut img, rd + 24, 11); // inode = 11
    write_u16_le(&mut img, rd + 28, (BLOCK_SIZE - 24) as u16); // rec_len = 1000
    write_u16_le(&mut img, rd + 30, 9); // name_len = 9
    img[rd + 32..rd + 41].copy_from_slice(b"hello.txt");

    // =========================================================================
    // hello.txt Data (block 10)
    // =========================================================================
    let hd = BLOCK_HELLO_TXT * BLOCK_SIZE;
    img[hd..hd + hello_data.len()].copy_from_slice(hello_data);

    // Write the image
    std::fs::write(&out_path, &img).expect("failed to write rootfs.ext2");

    println!("cargo::rerun-if-changed=build.rs");
}
