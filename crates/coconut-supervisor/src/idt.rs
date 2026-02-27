//! Interrupt Descriptor Table with fault handlers and timer ISR.
//!
//! Sets up a 256-entry IDT. Specific handlers for divide-by-zero (#0),
//! double fault (#8), GPF (#13), page fault (#14), and timer (vector 32).
//! All others use a default handler that prints the vector number and halts.

use core::arch::{asm, naked_asm};

/// A single 16-byte IDT entry (interrupt gate descriptor) for 64-bit mode.
#[repr(C, packed)]
#[derive(Clone, Copy)]
struct IdtEntry {
    offset_low: u16,
    selector: u16,
    ist: u8,
    type_attr: u8,
    offset_mid: u16,
    offset_high: u32,
    _reserved: u32,
}

impl IdtEntry {
    const fn missing() -> Self {
        Self {
            offset_low: 0,
            selector: 0,
            ist: 0,
            type_attr: 0,
            offset_mid: 0,
            offset_high: 0,
            _reserved: 0,
        }
    }

    /// Create an interrupt gate entry pointing to the given handler address.
    /// selector = kernel CS = 0x08, DPL = 0, present, type = interrupt gate (0xE).
    fn new(handler: usize) -> Self {
        let addr = handler as u64;
        Self {
            offset_low: addr as u16,
            selector: 0x08,
            ist: 0,
            type_attr: 0x8E, // Present | DPL=0 | Interrupt Gate
            offset_mid: (addr >> 16) as u16,
            offset_high: (addr >> 32) as u32,
            _reserved: 0,
        }
    }
}

/// The IDTR pointer structure loaded by `lidt`.
#[repr(C, packed)]
struct IdtPointer {
    limit: u16,
    base: u64,
}

/// 256-entry IDT (4 KiB).
static mut IDT: [IdtEntry; 256] = [IdtEntry::missing(); 256];

/// Load the IDT and install fault handlers.
pub fn init() {
    unsafe {
        // Install specific fault handlers
        IDT[0] = IdtEntry::new(isr_stub_0 as *const () as usize);
        IDT[8] = IdtEntry::new(isr_stub_8 as *const () as usize);
        IDT[13] = IdtEntry::new(isr_gpf as *const () as usize);
        IDT[14] = IdtEntry::new(isr_stub_14 as *const () as usize);

        // Timer ISR at vector 32 (PIT IRQ 0, remapped by PIC)
        IDT[32] = IdtEntry::new(isr_timer as *const () as usize);

        // Install default handler for all other vectors
        for i in 0..256 {
            if i != 0 && i != 8 && i != 13 && i != 14 && i != 32 {
                IDT[i] = IdtEntry::new(isr_stub_default as *const () as usize);
            }
        }

        let idt_ptr = IdtPointer {
            limit: (size_of::<[IdtEntry; 256]>() - 1) as u16,
            base: (&raw const IDT) as u64,
        };

        asm!("lidt [{}]", in(reg) &idt_ptr, options(readonly, nostack, preserves_flags));
    }
}

// ---------------------------------------------------------------------------
// ISR stubs — naked functions that save state, call Rust handler, and halt.
// These exceptions are fatal in 0.1 so we never iretq.
// ---------------------------------------------------------------------------

/// #0 Divide Error (no error code)
#[unsafe(naked)]
unsafe extern "C" fn isr_stub_0() {
    naked_asm!(
        "push 0",    // fake error code for uniform frame
        "push 0",    // vector number
        "jmp {handler}",
        handler = sym fault_common,
    );
}

/// #8 Double Fault (error code pushed by CPU)
#[unsafe(naked)]
unsafe extern "C" fn isr_stub_8() {
    naked_asm!(
        "push 8",    // vector number (error code already on stack from CPU)
        "jmp {handler}",
        handler = sym fault_common,
    );
}

/// #13 General Protection Fault — user-mode aware.
///
/// Kernel mode: fatal, dispatches to fault_common.
/// User mode: kills the faulting shard via handle_sys_exit.
///
/// Stack at entry (error code pushed by CPU):
///   [RSP+0]  = error code
///   [RSP+8]  = RIP
///   [RSP+16] = CS
#[unsafe(naked)]
unsafe extern "C" fn isr_gpf() {
    naked_asm!(
        // Check CS RPL — user mode has bits 0:1 set
        "test qword ptr [rsp + 16], 3",
        "jnz 2f",

        // Kernel mode: fatal
        "push 13",
        "jmp {fault_common}",

        // User mode: kill the shard
        "2:",
        "mov rdi, [rsp + 8]",   // faulting RIP
        "call {gpf_kill}",
        // gpf_kill_shard never returns
        "3:",
        "hlt",
        "jmp 3b",

        fault_common = sym fault_common,
        gpf_kill = sym gpf_kill_shard,
    );
}

/// #14 Page Fault (error code pushed by CPU)
#[unsafe(naked)]
unsafe extern "C" fn isr_stub_14() {
    naked_asm!(
        "push 14",
        "jmp {handler}",
        handler = sym fault_common,
    );
}

/// Default handler for all other vectors (no error code)
#[unsafe(naked)]
unsafe extern "C" fn isr_stub_default() {
    naked_asm!(
        "push 0",    // fake error code
        "push 255",  // placeholder vector
        "jmp {handler}",
        handler = sym fault_common,
    );
}

/// Common fault handler. Stack layout at entry:
///   [RSP+0]  = vector number (pushed by stub)
///   [RSP+8]  = error code (pushed by CPU or stub)
///   [RSP+16] = RIP
///   [RSP+24] = CS
///   [RSP+32] = RFLAGS
///   [RSP+40] = RSP
///   [RSP+48] = SS
#[unsafe(naked)]
unsafe extern "C" fn fault_common() {
    naked_asm!(
        // Load vector and error code, pass as arguments
        "pop rdi",          // vector number
        "pop rsi",          // error code
        "mov rdx, [rsp]",   // RIP from interrupt frame
        "call {handler}",
        "2:",
        "hlt",
        "jmp 2b",
        handler = sym fault_handler_rust,
    );
}

/// Rust-level fault handler — prints details and halts.
extern "C" fn fault_handler_rust(vector: u64, error_code: u64, rip: u64) {
    let name = match vector {
        0 => "Divide Error",
        8 => "Double Fault",
        13 => "General Protection Fault",
        14 => "Page Fault",
        _ => "Unknown Interrupt",
    };

    crate::serial_println!();
    crate::serial_println!("EXCEPTION: #{} {}", vector, name);
    crate::serial_println!("  Error code: {:#x}", error_code);
    crate::serial_println!("  RIP:        {:#x}", rip);

    if vector == 14 {
        let cr2: u64;
        unsafe { asm!("mov {}, cr2", out(reg) cr2, options(nomem, nostack, preserves_flags)) };
        crate::serial_println!("  CR2:        {:#x}", cr2);
    }
}

/// Kill a shard that caused a user-mode GPF (e.g. rdtsc with CR4.TSD set).
extern "C" fn gpf_kill_shard(rip: u64) {
    let id = crate::shard::current_shard();
    crate::serial_println!("GPF: shard {} faulted at RIP {:#x}, killing", id, rip);
    crate::shard::handle_sys_exit(u64::MAX);
}

// ---------------------------------------------------------------------------
// Timer ISR (vector 32) — PIT IRQ 0, preemptive scheduling
// ---------------------------------------------------------------------------

/// Timer ISR (vector 32, PIT IRQ 0).
///
/// Two paths based on interrupted context:
/// - Kernel mode (CS RPL=0): increment ticks, EOI, iretq
/// - User mode (CS RPL=3): save regs, preempt via scheduler, restore, iretq
///
/// Interrupt frame on stack at entry (pushed by CPU):
///   [RSP+0]  = RIP
///   [RSP+8]  = CS
///   [RSP+16] = RFLAGS
///   [RSP+24] = RSP
///   [RSP+32] = SS
#[unsafe(naked)]
unsafe extern "C" fn isr_timer() {
    naked_asm!(
        // Check CS RPL bits in the interrupt frame
        "test qword ptr [rsp + 8], 3",
        "jnz 2f",

        // --- Kernel-mode path: increment ticks, EOI, return ---
        "push rax",
        "mov rax, offset {ticks}",
        "inc qword ptr [rax]",
        "mov al, 0x20",
        "out 0x20, al",
        "pop rax",
        "iretq",

        // --- User-mode path: preempt the running shard ---
        "2:",
        // Save all caller-saved registers (Rust ABI will clobber these)
        "push rax",
        "push rcx",
        "push rdx",
        "push rsi",
        "push rdi",
        "push r8",
        "push r9",
        "push r10",
        "push r11",

        // Save FPU/SSE state — the shard uses float math (SSE2) and
        // clear_sensitive_cpu_state() zeros XMM on context switch.
        // FXSAVE area is 512 bytes, must be 16-byte aligned.
        "sub rsp, 512",
        "fxsave [rsp]",

        // Call scheduler::timer_preempt()
        // This increments ticks, sends EOI, sets shard Ready, context-switches.
        // When the shard is resumed later, execution returns here.
        "call {timer_preempt}",

        // Shard resumed — restore FPU/SSE state (same shard, no cross-shard leak)
        "fxrstor [rsp]",
        "add rsp, 512",

        // Restore caller-saved registers
        "pop r11",
        "pop r10",
        "pop r9",
        "pop r8",
        "pop rdi",
        "pop rsi",
        "pop rdx",
        "pop rcx",

        // Restore user data segments (iretq only restores CS and SS, not DS/ES)
        "mov eax, 0x1B",    // USER_DS = 0x18 | 3
        "mov ds, ax",
        "mov es, ax",
        "pop rax",           // restore original rax

        "iretq",

        ticks = sym crate::pit::TICKS,
        timer_preempt = sym crate::scheduler::timer_preempt,
    );
}
