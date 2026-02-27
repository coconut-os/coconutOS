//! coconut-rt — runtime library for coconutOS shard binaries.
//!
//! Provides entry point, panic handler, syscall wrappers, serial I/O,
//! and GPU primitives. Depends only on `coconut-shared`.

#![no_std]

pub mod gpu;
pub mod io;
pub mod sys;

use core::arch::global_asm;

// Entry stub: set RSP to stack top, call main(), exit cleanly.
// Placed in .text.entry so the linker script puts it first at VA 0x1000.
global_asm!(
    ".section .text.entry, \"ax\"",
    ".global _start",
    "_start:",
    "mov rsp, 0x800000",
    "call main",
    // main() returned — exit with code 0
    "xor edi, edi",
    "mov rax, 0",
    "syscall",
    // Unreachable
    "1: hlt",
    "jmp 1b",
);

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    // Best-effort: print panic message via serial
    use core::fmt::Write;
    let _ = write!(io::SerialWriter, "PANIC: {}\n", info);
    sys::exit(u64::MAX);
}
