//! Build script that generates a minimal ext2 filesystem image.
//!
//! Produces a 128 KiB (128 × 1024-byte blocks) rev 0 ext2 image containing:
//!   - Root directory with "hello.txt" and "model.bin"
//!   - hello.txt contents: "Hello from coconutFS!\n"
//!   - model.bin: deterministic llama2.c-format model with LCG weights
//!
//! No external tools (mke2fs, etc.) are required.

use std::path::Path;

const BLOCK_SIZE: usize = 1024;
const TOTAL_BLOCKS: usize = 128;
const IMAGE_SIZE: usize = TOTAL_BLOCKS * BLOCK_SIZE;
const TOTAL_INODES: usize = 32;
const INODE_SIZE: usize = 128;

// Block assignments — fixed metadata
const _BLOCK_BOOT: usize = 0;
const BLOCK_SUPERBLOCK: usize = 1;
const BLOCK_GROUP_DESC: usize = 2;
const BLOCK_BLOCK_BITMAP: usize = 3;
const BLOCK_INODE_BITMAP: usize = 4;
const BLOCK_INODE_TABLE: usize = 5; // blocks 5-8 (32 inodes × 128 bytes = 4096 = 4 blocks)
const BLOCK_ROOT_DIR: usize = 9;
const BLOCK_HELLO_TXT: usize = 10;
// Blocks 11+ used by model.bin (direct data, indirect pointer block, indirect data)

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

/// Simple LCG PRNG — deterministic, reproducible weights.
struct Lcg {
    state: u64,
}

impl Lcg {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next(&mut self) -> u64 {
        self.state = self.state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.state
    }

    /// Generate a float in [-0.5, 0.5) for numerically stable activations.
    fn next_f32(&mut self) -> f32 {
        let bits = self.next();
        // Use upper 32 bits, map to [-0.5, 0.5)
        let u = (bits >> 32) as u32;
        (u as f32 / u32::MAX as f32) - 0.5
    }
}

/// Generate model.bin in llama2.c checkpoint format.
fn generate_model() -> Vec<u8> {
    let dim: i32 = 32;
    let hidden_dim: i32 = 64;
    let n_layers: i32 = 2;
    let n_heads: i32 = 4;
    let n_kv_heads: i32 = 4;
    let vocab_size: i32 = 32; // positive = shared embedding/unembedding weights
    let seq_len: i32 = 32;
    let head_size = dim / n_heads; // 8
    let kv_dim = (n_kv_heads * head_size) as usize; // 32

    let mut data = Vec::new();

    // Header: 7 × i32
    for &val in &[dim, hidden_dim, n_layers, n_heads, n_kv_heads, vocab_size, seq_len] {
        data.extend_from_slice(&val.to_le_bytes());
    }

    let mut rng = Lcg::new(42);

    // Helper: write n floats from the PRNG
    let write_floats = |data: &mut Vec<u8>, rng: &mut Lcg, count: usize| {
        for _ in 0..count {
            data.extend_from_slice(&rng.next_f32().to_le_bytes());
        }
    };

    let d = dim as usize;
    let hd = hidden_dim as usize;
    let nl = n_layers as usize;
    let vs = vocab_size as usize;
    let sl = seq_len as usize;
    let hs = head_size as usize;

    // token_embedding_table: vocab_size × dim
    write_floats(&mut data, &mut rng, vs * d);
    // rms_att_weight: n_layers × dim
    write_floats(&mut data, &mut rng, nl * d);
    // wq: n_layers × dim × dim
    write_floats(&mut data, &mut rng, nl * d * d);
    // wk: n_layers × dim × kv_dim
    write_floats(&mut data, &mut rng, nl * d * kv_dim);
    // wv: n_layers × dim × kv_dim
    write_floats(&mut data, &mut rng, nl * d * kv_dim);
    // wo: n_layers × dim × dim
    write_floats(&mut data, &mut rng, nl * d * d);
    // rms_ffn_weight: n_layers × dim
    write_floats(&mut data, &mut rng, nl * d);
    // w1: n_layers × dim × hidden_dim
    write_floats(&mut data, &mut rng, nl * d * hd);
    // w2: n_layers × hidden_dim × dim
    write_floats(&mut data, &mut rng, nl * hd * d);
    // w3: n_layers × dim × hidden_dim
    write_floats(&mut data, &mut rng, nl * d * hd);
    // rms_final_weight: dim
    write_floats(&mut data, &mut rng, d);
    // freq_cis_real: seq_len × head_size / 2
    write_floats(&mut data, &mut rng, sl * hs / 2);
    // freq_cis_imag: seq_len × head_size / 2
    write_floats(&mut data, &mut rng, sl * hs / 2);
    // wcls: shared with token_embedding_table (positive vocab_size), no extra data

    data
}

fn main() {
    let out_dir = std::env::var("OUT_DIR").unwrap();
    let out_path = Path::new(&out_dir).join("rootfs.ext2");

    let model_data = generate_model();
    let model_size = model_data.len();
    let model_data_blocks = (model_size + BLOCK_SIZE - 1) / BLOCK_SIZE;

    // Model block layout:
    //   Direct blocks: blocks 11..min(11+12, 11+model_data_blocks) = i_block[0..11]
    //   If model_data_blocks > 12, need a single indirect block
    let model_direct_count = model_data_blocks.min(12);
    let model_indirect_count = if model_data_blocks > 12 {
        model_data_blocks - 12
    } else {
        0
    };
    let needs_indirect = model_indirect_count > 0;

    // Block numbering for model.bin
    let model_direct_start = 11; // blocks 11..22 (up to 12 direct)
    let model_indirect_ptr_block = if needs_indirect {
        model_direct_start + model_direct_count
    } else {
        0
    };
    let model_indirect_data_start = if needs_indirect {
        model_indirect_ptr_block + 1
    } else {
        0
    };

    let total_model_blocks = model_data_blocks + if needs_indirect { 1 } else { 0 };
    let first_free_block = 11 + total_model_blocks;

    assert!(
        first_free_block <= TOTAL_BLOCKS,
        "model too large for image: need {} blocks, have {}",
        first_free_block,
        TOTAL_BLOCKS
    );

    let mut img = vec![0u8; IMAGE_SIZE];

    // =========================================================================
    // Superblock (block 1, offset 1024)
    // =========================================================================
    let sb = BLOCK_SUPERBLOCK * BLOCK_SIZE;

    write_u32_le(&mut img, sb + 0, TOTAL_INODES as u32); // s_inodes_count
    write_u32_le(&mut img, sb + 4, TOTAL_BLOCKS as u32); // s_blocks_count
    write_u32_le(&mut img, sb + 8, 0); // s_r_blocks_count
    write_u32_le(&mut img, sb + 12, (TOTAL_BLOCKS - first_free_block) as u32); // s_free_blocks_count
    write_u32_le(&mut img, sb + 16, (TOTAL_INODES - 12) as u32); // s_free_inodes_count (1-12 used)
    write_u32_le(&mut img, sb + 20, 1); // s_first_data_block
    write_u32_le(&mut img, sb + 24, 0); // s_log_block_size (0 = 1024)
    write_u32_le(&mut img, sb + 28, 0); // s_log_frag_size
    write_u32_le(&mut img, sb + 32, TOTAL_BLOCKS as u32); // s_blocks_per_group
    write_u32_le(&mut img, sb + 36, TOTAL_BLOCKS as u32); // s_frags_per_group
    write_u32_le(&mut img, sb + 40, TOTAL_INODES as u32); // s_inodes_per_group
    write_u16_le(&mut img, sb + 48, 0); // s_mnt_count
    write_u16_le(&mut img, sb + 50, u16::MAX); // s_max_mnt_count
    write_u16_le(&mut img, sb + 56, 0xEF53); // s_magic
    write_u16_le(&mut img, sb + 58, 1); // s_state = EXT2_VALID_FS
    write_u16_le(&mut img, sb + 60, 1); // s_errors = EXT2_ERRORS_CONTINUE
    write_u32_le(&mut img, sb + 76, 0); // s_rev_level = 0

    // =========================================================================
    // Block Group Descriptor Table (block 2)
    // =========================================================================
    let bgd = BLOCK_GROUP_DESC * BLOCK_SIZE;

    write_u32_le(&mut img, bgd + 0, BLOCK_BLOCK_BITMAP as u32);
    write_u32_le(&mut img, bgd + 4, BLOCK_INODE_BITMAP as u32);
    write_u32_le(&mut img, bgd + 8, BLOCK_INODE_TABLE as u32);
    write_u16_le(&mut img, bgd + 12, (TOTAL_BLOCKS - first_free_block) as u16);
    write_u16_le(&mut img, bgd + 14, (TOTAL_INODES - 12) as u16);
    write_u16_le(&mut img, bgd + 16, 1); // bg_used_dirs_count

    // =========================================================================
    // Block Bitmap (block 3) — blocks 0..(first_free_block-1) allocated
    // =========================================================================
    let bb = BLOCK_BLOCK_BITMAP * BLOCK_SIZE;
    for bit in 0..first_free_block {
        let byte_idx = bit / 8;
        let bit_idx = bit % 8;
        img[bb + byte_idx] |= 1 << bit_idx;
    }

    // =========================================================================
    // Inode Bitmap (block 4) — inodes 1-12 allocated
    // =========================================================================
    let ib = BLOCK_INODE_BITMAP * BLOCK_SIZE;
    // Bits 0-11 set (inode 1-12)
    write_u16_le(&mut img, ib, 0x0FFF);

    // =========================================================================
    // Inode Table (blocks 5-8)
    // =========================================================================
    let it = BLOCK_INODE_TABLE * BLOCK_SIZE;

    // Inode 2 (root directory) — offset = (2-1) * 128 = 128
    let root_ino = it + 1 * INODE_SIZE;
    write_u16_le(&mut img, root_ino + 0, 0x4000 | 0o755); // S_IFDIR | 0755
    write_u32_le(&mut img, root_ino + 4, BLOCK_SIZE as u32); // i_size
    write_u16_le(&mut img, root_ino + 26, 2); // i_links_count
    write_u32_le(&mut img, root_ino + 28, 2); // i_blocks (512-byte sectors)
    write_u32_le(&mut img, root_ino + 40, BLOCK_ROOT_DIR as u32); // i_block[0]

    // Inode 11 (hello.txt) — offset = (11-1) * 128 = 1280
    let hello_ino = it + 10 * INODE_SIZE;
    let hello_data = b"Hello from coconutFS!\n";
    let hello_size = hello_data.len() as u32;
    write_u16_le(&mut img, hello_ino + 0, 0x8000 | 0o644); // S_IFREG | 0644
    write_u32_le(&mut img, hello_ino + 4, hello_size);
    write_u16_le(&mut img, hello_ino + 26, 1);
    write_u32_le(&mut img, hello_ino + 28, 2); // i_blocks
    write_u32_le(&mut img, hello_ino + 40, BLOCK_HELLO_TXT as u32); // i_block[0]

    // Inode 12 (model.bin) — offset = (12-1) * 128 = 1408
    let model_ino = it + 11 * INODE_SIZE;
    write_u16_le(&mut img, model_ino + 0, 0x8000 | 0o644); // S_IFREG | 0644
    write_u32_le(&mut img, model_ino + 4, model_size as u32); // i_size
    write_u16_le(&mut img, model_ino + 26, 1); // i_links_count
    // i_blocks = total 512-byte sectors used (data + indirect block)
    let model_sectors = total_model_blocks * (BLOCK_SIZE / 512);
    write_u32_le(&mut img, model_ino + 28, model_sectors as u32);

    // Direct block pointers: i_block[0..model_direct_count]
    for i in 0..model_direct_count {
        write_u32_le(
            &mut img,
            model_ino + 40 + i * 4,
            (model_direct_start + i) as u32,
        );
    }
    // Single indirect pointer: i_block[12]
    if needs_indirect {
        write_u32_le(
            &mut img,
            model_ino + 40 + 12 * 4,
            model_indirect_ptr_block as u32,
        );
    }

    // =========================================================================
    // Root Directory Data (block 9)
    // =========================================================================
    let rd = BLOCK_ROOT_DIR * BLOCK_SIZE;

    // Entry 1: "." → inode 2
    write_u32_le(&mut img, rd + 0, 2);
    write_u16_le(&mut img, rd + 4, 12); // rec_len
    write_u16_le(&mut img, rd + 6, 1); // name_len
    img[rd + 8] = b'.';

    // Entry 2: ".." → inode 2
    write_u32_le(&mut img, rd + 12, 2);
    write_u16_le(&mut img, rd + 16, 12);
    write_u16_le(&mut img, rd + 18, 2);
    img[rd + 20] = b'.';
    img[rd + 21] = b'.';

    // Entry 3: "hello.txt" → inode 11
    write_u32_le(&mut img, rd + 24, 11);
    write_u16_le(&mut img, rd + 28, 20); // rec_len = 8 + name(9) + padding(3) = 20
    write_u16_le(&mut img, rd + 30, 9); // name_len
    img[rd + 32..rd + 41].copy_from_slice(b"hello.txt");

    // Entry 4: "model.bin" → inode 12 (last entry gets rest of block)
    write_u32_le(&mut img, rd + 44, 12);
    write_u16_le(&mut img, rd + 48, (BLOCK_SIZE - 44) as u16); // rec_len = rest
    write_u16_le(&mut img, rd + 50, 9); // name_len
    img[rd + 52..rd + 61].copy_from_slice(b"model.bin");

    // =========================================================================
    // hello.txt Data (block 10)
    // =========================================================================
    let hd = BLOCK_HELLO_TXT * BLOCK_SIZE;
    img[hd..hd + hello_data.len()].copy_from_slice(hello_data);

    // =========================================================================
    // model.bin Data (blocks 11+)
    // =========================================================================

    // Write direct data blocks (blocks model_direct_start..)
    for i in 0..model_direct_count {
        let block_nr = model_direct_start + i;
        let src_start = i * BLOCK_SIZE;
        let src_end = (src_start + BLOCK_SIZE).min(model_data.len());
        let dst = block_nr * BLOCK_SIZE;
        img[dst..dst + (src_end - src_start)].copy_from_slice(&model_data[src_start..src_end]);
    }

    if needs_indirect {
        // Write indirect pointer block
        let ind_block = model_indirect_ptr_block * BLOCK_SIZE;
        for i in 0..model_indirect_count {
            let data_block_nr = model_indirect_data_start + i;
            write_u32_le(&mut img, ind_block + i * 4, data_block_nr as u32);
        }

        // Write indirect data blocks
        for i in 0..model_indirect_count {
            let block_nr = model_indirect_data_start + i;
            let src_start = (12 + i) * BLOCK_SIZE; // 12 direct blocks already written
            let src_end = (src_start + BLOCK_SIZE).min(model_data.len());
            if src_start >= model_data.len() {
                break;
            }
            let dst = block_nr * BLOCK_SIZE;
            img[dst..dst + (src_end - src_start)]
                .copy_from_slice(&model_data[src_start..src_end]);
        }
    }

    // Write the image
    std::fs::write(&out_path, &img).expect("failed to write rootfs.ext2");
    eprintln!(
        "rootfs.ext2: {} KiB, model.bin={} bytes ({} blocks, {} indirect)",
        TOTAL_BLOCKS,
        model_size,
        model_data_blocks,
        model_indirect_count
    );

    // Copy GPU shard flat binary into OUT_DIR for include_bytes!
    if let Ok(shard_path) = std::env::var("COCONUT_SHARD_GPU_BIN") {
        let dst = Path::new(&out_dir).join("shard-gpu.bin");
        std::fs::copy(&shard_path, &dst).expect("failed to copy shard-gpu.bin");
        println!("cargo::rerun-if-changed={}", shard_path);
    }

    // Copy C shard flat binary into OUT_DIR for include_bytes!
    if let Ok(shard_path) = std::env::var("COCONUT_SHARD_HELLO_C_BIN") {
        let dst = Path::new(&out_dir).join("shard-hello-c.bin");
        std::fs::copy(&shard_path, &dst).expect("failed to copy shard-hello-c.bin");
        println!("cargo::rerun-if-changed={}", shard_path);
    }

    // Copy llama-inference shard flat binary into OUT_DIR for include_bytes!
    if let Ok(shard_path) = std::env::var("COCONUT_SHARD_LLAMA_BIN") {
        let dst = Path::new(&out_dir).join("shard-llama-inference.bin");
        std::fs::copy(&shard_path, &dst).expect("failed to copy shard-llama-inference.bin");
        println!("cargo::rerun-if-changed={}", shard_path);
    }

    println!("cargo::rerun-if-changed=build.rs");
}
