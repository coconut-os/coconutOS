//! GPU HAL shard — Rust replacement for the assembly HAL shard.
//!
//! Validates GPU device access, initializes a VRAM allocator, dispatches a
//! 4×4 matmul via command ring, verifies results, and tests inter-partition DMA.
//! Must produce identical serial output to the original ASM version.

#![no_std]
#![no_main]

extern crate coconut_rt;

use coconut_rt::gpu::{CommandRing, GpuConfig, VramAllocator};
use coconut_rt::sys;

use core::ptr;

use coconut_shared::{PLEDGE_CHANNEL, PLEDGE_GPU_DMA, PLEDGE_SERIAL};

/// Fail handler — prints failure message and exits with code 1.
fn fail() -> ! {
    sys::serial_write(b"GPU compute: FAIL\n");
    sys::exit(1);
}

#[no_mangle]
pub extern "C" fn main() {
    // 1. Read + validate GPU config page
    let config = match GpuConfig::read() {
        Some(c) => c,
        None => fail(),
    };

    // 2. pledge(SERIAL | CHANNEL | GPU_DMA)
    if sys::gpu_pledge(PLEDGE_SERIAL | PLEDGE_CHANNEL | PLEDGE_GPU_DMA) != 0 {
        fail();
    }

    // 3. unveil(0, vram_size)
    if sys::gpu_unveil(0, config.vram_size) != 0 {
        fail();
    }

    let vram = config.vram_vaddr as *mut u8;
    let mmio = config.mmio_vaddr as *mut u8;

    // 4. VRAM write/readback test
    unsafe {
        ptr::write_volatile(vram as *mut u32, 0xDEAD_BEEF);
        if ptr::read_volatile(vram as *const u32) != 0xDEAD_BEEF {
            fail();
        }
    }

    // 5. MMIO VBE ID register check (at MMIO + 0x500)
    let vbe_id = unsafe { ptr::read_volatile(mmio.add(0x500) as *const u16) };
    if vbe_id == 0xFFFF || vbe_id == 0 {
        fail();
    }

    // 6. Init VRAM allocator
    let mut alloc = VramAllocator::init(vram, config.vram_size as u32);

    // 7. Alloc command ring + matrices A, B, C
    let ring_off = alloc.alloc(1, 4096).unwrap_or_else(|| fail());
    let a_off = alloc.alloc(2, 64).unwrap_or_else(|| fail());
    let b_off = alloc.alloc(2, 64).unwrap_or_else(|| fail());
    let c_off = alloc.alloc(2, 64).unwrap_or_else(|| fail());

    // Verify allocator state
    if alloc.alloc_count() != 4 {
        fail();
    }

    // 8. Initialize command ring
    let mut ring = CommandRing::init(unsafe { vram.add(ring_off as usize) });
    if !ring.verify() {
        fail();
    }

    // 9. Write A = [1, 2, ..., 16]
    let a_ptr = unsafe { vram.add(a_off as usize) as *mut u32 };
    for i in 0..16u32 {
        unsafe { ptr::write_volatile(a_ptr.add(i as usize), i + 1) };
    }

    // 10. Write B = 2×I₄ (identity scaled by 2)
    let b_ptr = unsafe { vram.add(b_off as usize) as *mut u32 };
    for i in 0..16u32 {
        unsafe { ptr::write_volatile(b_ptr.add(i as usize), 0) };
    }
    // Diagonal: B[0][0], B[1][1], B[2][2], B[3][3] = 2
    unsafe {
        ptr::write_volatile(b_ptr, 2);
        ptr::write_volatile(b_ptr.add(5), 2);  // (1*4+1)
        ptr::write_volatile(b_ptr.add(10), 2); // (2*4+2)
        ptr::write_volatile(b_ptr.add(15), 2); // (3*4+3)
    }

    // 11. Submit + process matmul via command ring
    ring.submit_matmul(a_off, b_off, c_off, 4);

    // Process: read command, compute C = A × B
    let (cmd_a, cmd_b, cmd_c, _dim) = ring.read_command();
    let cmd_a_ptr = unsafe { vram.add(cmd_a as usize) as *const u32 };
    let cmd_b_ptr = unsafe { vram.add(cmd_b as usize) as *const u32 };
    let cmd_c_ptr = unsafe { vram.add(cmd_c as usize) as *mut u32 };
    coconut_rt::gpu::matmul_4x4(cmd_a_ptr, cmd_b_ptr, cmd_c_ptr);
    ring.complete();

    // 12. Verify C[i] == 2 * A[i]
    let c_ptr = unsafe { vram.add(c_off as usize) as *const u32 };
    for i in 0..16u32 {
        unsafe {
            let a_val = ptr::read_volatile(a_ptr.add(i as usize));
            let c_val = ptr::read_volatile(c_ptr.add(i as usize));
            if c_val != a_val * 2 {
                fail();
            }
        }
    }

    // 13. Free all allocations (reverse order), verify zeroing
    if !alloc.free(c_off) { fail(); }
    if !alloc.free(b_off) { fail(); }
    if !alloc.free(a_off) { fail(); }
    if !alloc.free(ring_off) { fail(); }

    // Verify zeroing: first dword of each freed region must be 0
    unsafe {
        if ptr::read_volatile(vram.add(c_off as usize) as *const u32) != 0 { fail(); }
        if ptr::read_volatile(vram.add(b_off as usize) as *const u32) != 0 { fail(); }
        if ptr::read_volatile(vram.add(a_off as usize) as *const u32) != 0 { fail(); }
        if ptr::read_volatile(vram.add(ring_off as usize) as *const u32) != 0 { fail(); }
    }

    // 14. Zero allocator page
    alloc.zero_page();

    // 15. Print success
    sys::serial_write(b"GPU mem: freed+zeroed, compute ok\n");

    // 16. DMA test: branch on partition_id
    if config.partition_id == 0 {
        dma_sender(vram);
    } else {
        dma_receiver(vram);
    }
}

/// Partition 0: write test pattern, DMA to partition 1, signal via channel.
fn dma_sender(vram: *mut u8) {
    // Write [1, 2, ..., 16] at VRAM + 0x100000
    let data = unsafe { vram.add(0x10_0000) as *mut u32 };
    for i in 0..16u32 {
        unsafe { ptr::write_volatile(data.add(i as usize), i + 1) };
    }

    // SYS_GPU_DMA(target=1, src_offset=0x100000, packed=(0x100000<<32)|64)
    let packed = (0x10_0000u64 << 32) | 64;
    if sys::gpu_dma(1, 0x10_0000, packed) != 0 {
        fail();
    }

    // Signal via channel 0
    sys::channel_send(0, b"DON");

    sys::serial_write(b"GPU DMA: sent 64 bytes to partition 1\n");
}

/// Partition 1: wait for signal, verify DMA'd data.
fn dma_receiver(vram: *mut u8) {
    // Block on channel 0 until sender signals
    let mut buf = [0u8; 8];
    sys::channel_recv(0, &mut buf);

    // Verify [1, 2, ..., 16] at VRAM + 0x100000
    let data = unsafe { vram.add(0x10_0000) as *const u32 };
    for i in 0..16u32 {
        unsafe {
            if ptr::read_volatile(data.add(i as usize)) != i + 1 {
                fail();
            }
        }
    }

    sys::serial_write(b"GPU DMA: recv ok, verified\n");
}
