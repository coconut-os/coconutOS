//! Thin syscall wrappers — one inline asm stub per coconutOS syscall.
//!
//! Each wrapper loads arguments into System V registers and issues `syscall`.
//! The kernel's syscall entry stub preserves callee-saved registers (rbx, rbp,
//! r12-r15) but clobbers caller-saved ones (rdi, rsi, rdx, r8-r10, rcx, r11).

use core::arch::asm;

use coconut_shared::{
    SYS_CHANNEL_RECV, SYS_CHANNEL_SEND, SYS_GPU_DMA, SYS_GPU_PLEDGE, SYS_GPU_UNVEIL,
    SYS_SERIAL_WRITE, SYS_YIELD,
};

/// Terminate the current shard with the given exit code.
pub fn exit(code: u64) -> ! {
    unsafe {
        asm!(
            "mov rax, {nr}",
            "syscall",
            nr = const coconut_shared::SYS_EXIT,
            in("rdi") code,
            options(noreturn),
        );
    }
}

/// Write bytes to the serial console.
pub fn serial_write(buf: &[u8]) {
    let ret: u64;
    unsafe {
        asm!(
            "syscall",
            in("rax") SYS_SERIAL_WRITE,
            in("rdi") buf.as_ptr(),
            in("rsi") buf.len(),
            lateout("rax") ret,
            out("rcx") _, out("r11") _,
            out("rdx") _, out("r8") _, out("r9") _, out("r10") _,
            options(nostack),
        );
    }
    let _ = ret;
}

/// Monotonically restrict allowed syscall categories.
pub fn gpu_pledge(mask: u64) -> u64 {
    let ret: u64;
    unsafe {
        asm!(
            "syscall",
            in("rax") SYS_GPU_PLEDGE,
            in("rdi") mask,
            lateout("rax") ret,
            out("rcx") _, out("r11") _,
            lateout("rdi") _, lateout("rsi") _,
            out("rdx") _, out("r8") _, out("r9") _, out("r10") _,
            options(nostack),
        );
    }
    ret
}

/// Lock the VRAM range this shard may access via DMA (one-shot).
pub fn gpu_unveil(offset: u64, size: u64) -> u64 {
    let ret: u64;
    unsafe {
        asm!(
            "syscall",
            in("rax") SYS_GPU_UNVEIL,
            in("rdi") offset,
            in("rsi") size,
            lateout("rax") ret,
            out("rcx") _, out("r11") _,
            lateout("rdi") _, lateout("rsi") _,
            out("rdx") _, out("r8") _, out("r9") _, out("r10") _,
            options(nostack),
        );
    }
    ret
}

/// Copy data between VRAM partitions via kernel-mediated DMA.
///
/// `packed_dst_len` = `(dst_offset << 32) | len`.
pub fn gpu_dma(target_partition: u64, src_offset: u64, packed_dst_len: u64) -> u64 {
    let ret: u64;
    unsafe {
        asm!(
            "syscall",
            in("rax") SYS_GPU_DMA,
            in("rdi") target_partition,
            in("rsi") src_offset,
            in("rdx") packed_dst_len,
            lateout("rax") ret,
            out("rcx") _, out("r11") _,
            lateout("rdi") _, lateout("rsi") _, lateout("rdx") _,
            out("r8") _, out("r9") _, out("r10") _,
            options(nostack),
        );
    }
    ret
}

/// Send a message on a channel.
pub fn channel_send(channel: usize, buf: &[u8]) -> u64 {
    let ret: u64;
    unsafe {
        asm!(
            "syscall",
            in("rax") SYS_CHANNEL_SEND,
            in("rdi") channel,
            in("rsi") buf.as_ptr(),
            in("rdx") buf.len(),
            lateout("rax") ret,
            out("rcx") _, out("r11") _,
            lateout("rdi") _, lateout("rsi") _, lateout("rdx") _,
            out("r8") _, out("r9") _, out("r10") _,
            options(nostack),
        );
    }
    ret
}

/// Receive a message from a channel (may block until data available).
pub fn channel_recv(channel: usize, buf: &mut [u8]) -> u64 {
    let ret: u64;
    unsafe {
        asm!(
            "syscall",
            in("rax") SYS_CHANNEL_RECV,
            in("rdi") channel,
            in("rsi") buf.as_mut_ptr(),
            in("rdx") buf.len(),
            lateout("rax") ret,
            out("rcx") _, out("r11") _,
            lateout("rdi") _, lateout("rsi") _, lateout("rdx") _,
            out("r8") _, out("r9") _, out("r10") _,
            options(nostack),
        );
    }
    ret
}

/// Yield the current time slice voluntarily.
pub fn yield_now() {
    let ret: u64;
    unsafe {
        asm!(
            "syscall",
            in("rax") SYS_YIELD,
            lateout("rax") ret,
            out("rcx") _, out("r11") _,
            out("rdi") _, out("rsi") _,
            out("rdx") _, out("r8") _, out("r9") _, out("r10") _,
            options(nostack),
        );
    }
    let _ = ret;
}
