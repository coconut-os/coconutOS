//! Task State Segment for ring 3 → ring 0 transitions.
//!
//! The TSS provides the kernel stack pointer (RSP0) that the CPU loads
//! automatically when transitioning from ring 3 to ring 0 via syscall
//! or interrupt.

/// 64-bit Task State Segment (104 bytes).
#[repr(C, packed)]
pub struct TaskStateSegment {
    _reserved0: u32,
    /// Stack pointers for privilege levels 0-2.
    pub rsp: [u64; 3],
    _reserved1: u64,
    /// Interrupt Stack Table pointers (IST1-IST7).
    pub ist: [u64; 7],
    _reserved2: u64,
    _reserved3: u16,
    /// I/O map base address (offset from TSS base).
    pub iomap_base: u16,
}

/// Static TSS instance. 104 bytes, must remain at a fixed address.
#[repr(C, align(16))]
pub struct AlignedTss {
    pub tss: TaskStateSegment,
}

pub static mut TSS: AlignedTss = AlignedTss {
    tss: TaskStateSegment {
        _reserved0: 0,
        rsp: [0; 3],
        _reserved1: 0,
        ist: [0; 7],
        _reserved2: 0,
        _reserved3: 0,
        iomap_base: 104, // size of TSS = no I/O bitmap
    },
};

/// Initialize the TSS with the kernel stack pointer.
pub fn init(kernel_stack_top: u64) {
    unsafe {
        (*(&raw mut TSS)).tss.rsp[0] = kernel_stack_top;
    }
}

/// Update RSP0 (e.g., before entering a shard).
pub fn set_rsp0(rsp0: u64) {
    unsafe {
        (*(&raw mut TSS)).tss.rsp[0] = rsp0;
    }
}

/// Get the physical/virtual address of the TSS (for GDT descriptor).
pub fn tss_addr() -> u64 {
    (&raw const TSS) as u64
}

/// Size of the TSS in bytes.
pub fn tss_size() -> u16 {
    core::mem::size_of::<TaskStateSegment>() as u16
}
