//! Preemptive round-robin scheduler with priority levels.
//!
//! Each shard has its own kernel stack. `context_switch` saves/restores
//! callee-saved registers and swaps RSP. The PIT timer ISR calls
//! `timer_preempt` to yield the current shard; `run_loop` picks the
//! next Ready shard using priority-based round-robin.

use core::arch::naked_asm;

use crate::gdt;
use crate::pic;
use crate::pit;
use crate::shard::{self, Priority, ShardState, CURRENT_SHARD, MAX_SHARDS, SHARDS};
use crate::syscall;
use crate::tss;
use crate::vmm;

/// Saved RSP for the supervisor context (run_loop runs on __stack_top).
static mut SUPERVISOR_RSP: u64 = 0;

/// Last scheduled shard ID for round-robin fairness within priority levels.
static mut LAST_SCHEDULED: usize = 0;

/// Whether the CPU supports IBPB (Indirect Branch Prediction Barrier).
/// Set once during init by detect_cpu_mitigations().
static mut HAS_IBPB: bool = false;

/// Low-level context switch: save callee-saved regs + RSP, load new RSP + regs.
///
/// Arguments:
///   rdi = pointer to save location for current RSP
///   rsi = pointer to load location for new RSP
#[unsafe(naked)]
unsafe extern "C" fn context_switch(save_rsp: *mut u64, load_rsp: *const u64) {
    naked_asm!(
        "push rbx",
        "push rbp",
        "push r12",
        "push r13",
        "push r14",
        "push r15",
        "mov [rdi], rsp",   // save current RSP
        "mov rsp, [rsi]",   // load new RSP
        "pop r15",
        "pop r14",
        "pop r13",
        "pop r12",
        "pop rbp",
        "pop rbx",
        "ret",
    );
}

/// Set up a shard's kernel stack so that `context_switch` "returns" into
/// `shard_first_entry`. The stack is laid out so the 6 pops + ret in
/// context_switch land at the trampoline.
pub fn setup_initial_kernel_stack(id: usize) {
    let shard = unsafe { &mut (*(&raw mut SHARDS))[id] };
    let top = shard.kernel_stack_top;

    // Stack grows downward. We push 7 values (6 regs + return address):
    //   [top - 8]  = shard_first_entry (return address for `ret`)
    //   [top - 16] = 0 (rbx)
    //   [top - 24] = 0 (rbp)
    //   [top - 32] = 0 (r12)
    //   [top - 40] = 0 (r13)
    //   [top - 48] = 0 (r14)
    //   [top - 56] = 0 (r15)
    let stack = top as *mut u64;
    unsafe {
        *stack.offset(-1) = shard_first_entry as *const () as u64; // return address
        *stack.offset(-2) = 0; // rbx
        *stack.offset(-3) = 0; // rbp
        *stack.offset(-4) = 0; // r12
        *stack.offset(-5) = 0; // r13
        *stack.offset(-6) = 0; // r14
        *stack.offset(-7) = 0; // r15
    }
    shard.saved_kernel_rsp = top - 7 * 8; // point to the bottom of our synthetic frame
}

/// Trampoline for first-run shards. Called when context_switch `ret`s into
/// a shard that has never run before.
///
/// At this point:
///   - CR3 is already set to the shard's PML4 by run_loop
///   - KERNEL_RSP and TSS.RSP0 are set by run_loop
///   - CURRENT_SHARD is set
///
/// We just need to set user data segments and sysretq into user mode.
extern "C" fn shard_first_entry() -> ! {
    let id = unsafe { *(&raw const CURRENT_SHARD) };
    let shard = unsafe { &(*(&raw const SHARDS))[id] };

    // Reset the kernel stack pointer for syscall entry — since this is the
    // shard's first entry, the kernel stack should be at its top.
    unsafe {
        *(&raw mut syscall::KERNEL_RSP) = shard.kernel_stack_top;
    }

    let user_ds = gdt::USER_DS as u64;
    unsafe {
        core::arch::asm!(
            "mov ds, {ds:x}",
            "mov es, {ds:x}",
            "mov rsp, {user_rsp}",
            "mov rcx, {user_rip}",
            "mov r11, {user_rflags}",
            "sysretq",
            ds = in(reg) user_ds,
            user_rsp = in(reg) shard::SHARD_INITIAL_RSP,
            user_rip = in(reg) shard::SHARD_CODE_VADDR,
            user_rflags = in(reg) 0x202u64, // IF=1, reserved bit 1
            options(noreturn),
        );
    }
}

/// Yield from the current shard back to the supervisor run_loop.
/// Called when a shard blocks on a channel recv or is preempted by the timer.
pub fn schedule_yield() {
    let id = unsafe { *(&raw const CURRENT_SHARD) };
    unsafe {
        context_switch(
            &raw mut (*(&raw mut SHARDS))[id].saved_kernel_rsp,
            &raw const SUPERVISOR_RSP,
        );
    }
    // Resumed — CR3 was set by run_loop before switching back to us
}

/// Yield from an exited shard (CURRENT_SHARD is already usize::MAX).
/// We don't save the shard's RSP since it will never resume.
pub fn schedule_yield_exit() -> ! {
    // We need a throwaway location to "save" into (context_switch requires it)
    static mut DISCARD_RSP: u64 = 0;
    unsafe {
        context_switch(&raw mut DISCARD_RSP, &raw const SUPERVISOR_RSP);
    }
    unreachable!("exited shard resumed");
}

/// Called from the timer ISR (user-mode path) to preempt the current shard.
///
/// Increments tick counter, sends EOI (must be before context_switch so the
/// PIC can deliver the next timer interrupt to the new shard), sets the
/// current shard from Running to Ready, then yields to the supervisor.
///
/// When the shard is resumed later, this function returns back to the ISR
/// stub, which restores caller-saved regs and iretq's back to user mode.
pub extern "C" fn timer_preempt() {
    pit::increment_ticks();
    pic::send_eoi(0);

    let id = unsafe { *(&raw const CURRENT_SHARD) };
    if id < MAX_SHARDS {
        let shard = unsafe { &mut (*(&raw mut SHARDS))[id] };
        if shard.state == ShardState::Running {
            shard.state = ShardState::Ready;
        }
    }

    schedule_yield();
    // Returns here when shard is resumed — back to ISR stub
}

/// Handle SYS_YIELD: voluntarily yield the current time slice.
pub fn handle_sys_yield() {
    let id = unsafe { *(&raw const CURRENT_SHARD) };
    if id < MAX_SHARDS {
        let shard = unsafe { &mut (*(&raw mut SHARDS))[id] };
        if shard.state == ShardState::Running {
            shard.state = ShardState::Ready;
        }
    }
    schedule_yield();
}

/// Priority-based round-robin: scan priority levels high→low,
/// within each level start from (LAST_SCHEDULED+1) % MAX_SHARDS.
fn pick_next_ready() -> Option<usize> {
    let shards = unsafe { &*(&raw const SHARDS) };
    let last = unsafe { *(&raw const LAST_SCHEDULED) };

    // Priority levels from highest (Critical=0) to lowest (Low=3)
    let priorities = [
        Priority::Critical,
        Priority::High,
        Priority::Normal,
        Priority::Low,
    ];

    for &prio in &priorities {
        for offset in 1..=MAX_SHARDS {
            let i = (last + offset) % MAX_SHARDS;
            if shards[i].state == ShardState::Ready && shards[i].priority == prio {
                unsafe {
                    *(&raw mut LAST_SCHEDULED) = i;
                }
                return Some(i);
            }
        }
    }
    None
}

/// Check if all shards are either Exited, Destroyed, or Free.
fn all_done() -> bool {
    let shards = unsafe { &*(&raw const SHARDS) };
    for i in 0..MAX_SHARDS {
        match shards[i].state {
            ShardState::Free | ShardState::Exited | ShardState::Destroyed => {}
            _ => return false,
        }
    }
    true
}

// ---------------------------------------------------------------------------
// Side-channel mitigations
// ---------------------------------------------------------------------------

/// Probe CPUID for branch predictor mitigations. Called once during init.
pub fn detect_cpu_mitigations() {
    let edx: u32;
    // Sound: CPUID is always available on x86-64, no side effects.
    // rbx is LLVM-reserved — save/restore manually around cpuid.
    unsafe {
        core::arch::asm!(
            "push rbx",
            "mov eax, 7",
            "xor ecx, ecx",
            "cpuid",
            "pop rbx",
            out("eax") _, out("ecx") _, out("edx") edx,
        );
    }
    // EDX bit 26: IBPB (Indirect Branch Prediction Barrier) support
    if edx & (1 << 26) != 0 {
        unsafe { *(&raw mut HAS_IBPB) = true; }
        crate::serial_println!("Side-channel: IBPB supported, will flush branch predictor");
    }
}

/// Clear microarchitectural state that could leak data between shards.
///
/// Prevents FPU/SSE register observation, debug register persistence,
/// and branch predictor cross-shard inference.
fn clear_sensitive_cpu_state() {
    // Sound: these instructions have no preconditions beyond ring-0 privilege.
    // We're in the supervisor run_loop, single-core, interrupts disabled.
    unsafe {
        core::arch::asm!(
            // Reset x87 FPU to initial state
            "fninit",
            // Zero all XMM registers (SSE state)
            "xorps xmm0, xmm0",
            "xorps xmm1, xmm1",
            "xorps xmm2, xmm2",
            "xorps xmm3, xmm3",
            "xorps xmm4, xmm4",
            "xorps xmm5, xmm5",
            "xorps xmm6, xmm6",
            "xorps xmm7, xmm7",
            "xorps xmm8, xmm8",
            "xorps xmm9, xmm9",
            "xorps xmm10, xmm10",
            "xorps xmm11, xmm11",
            "xorps xmm12, xmm12",
            "xorps xmm13, xmm13",
            "xorps xmm14, xmm14",
            "xorps xmm15, xmm15",
            // Reset MXCSR to default (0x1F80: all exceptions masked, round-to-nearest)
            "sub rsp, 4",
            "mov dword ptr [rsp], 0x1F80",
            "ldmxcsr [rsp]",
            "add rsp, 4",
            // Clear debug breakpoint addresses and disable all breakpoints
            "xor eax, eax",
            "mov dr0, rax",
            "mov dr1, rax",
            "mov dr2, rax",
            "mov dr3, rax",
            // DR7: bit 10 must be 1 (reserved), all breakpoints disabled
            "mov eax, 0x400",
            "mov dr7, rax",
            out("rax") _,
            out("xmm0") _, out("xmm1") _, out("xmm2") _, out("xmm3") _,
            out("xmm4") _, out("xmm5") _, out("xmm6") _, out("xmm7") _,
            out("xmm8") _, out("xmm9") _, out("xmm10") _, out("xmm11") _,
            out("xmm12") _, out("xmm13") _, out("xmm14") _, out("xmm15") _,
        );
    }

    // IBPB: flush indirect branch predictor to prevent cross-shard speculation
    if unsafe { *(&raw const HAS_IBPB) } {
        unsafe {
            core::arch::asm!(
                "mov ecx, 0x49",
                "xor edx, edx",
                "mov eax, 1",
                "wrmsr",
                out("eax") _, out("ecx") _, out("edx") _,
                options(nomem, nostack),
            );
        }
    }
}

/// Read the CPU timestamp counter (RDTSC). Used for per-syscall cycle counting.
#[inline(always)]
pub fn read_tsc() -> u64 {
    let lo: u32;
    let hi: u32;
    // Sound: rdtsc is always available on x86-64, no memory side-effects
    unsafe {
        core::arch::asm!("rdtsc", out("eax") lo, out("edx") hi, options(nomem, nostack));
    }
    (hi as u64) << 32 | lo as u64
}

/// Main scheduler loop. Runs on the supervisor's stack (__stack_top).
/// Picks Ready shards and switches to them. Returns when all shards
/// are Exited/Destroyed.
pub fn run_loop() -> ! {
    crate::serial_println!("Scheduler: starting run loop");

    loop {
        match pick_next_ready() {
            Some(id) => {
                crate::serial_println!("Scheduler: switching to shard {}", id);

                let shard = unsafe { &mut (*(&raw mut SHARDS))[id] };
                shard.state = ShardState::Running;
                shard.context_switches += 1;
                // Record tick of first scheduling for wall-time calculation
                if shard.first_scheduled_tick == 0 {
                    shard.first_scheduled_tick = unsafe { *(&raw const pit::TICKS) };
                }
                unsafe {
                    *(&raw mut CURRENT_SHARD) = id;
                }

                // Set up kernel stack and page tables for this shard
                tss::set_rsp0(shard.kernel_stack_top);
                unsafe {
                    *(&raw mut syscall::KERNEL_RSP) = shard.kernel_stack_top;
                }
                vmm::write_cr3(shard.pml4_phys);

                // Clear FPU/SSE/debug state to prevent side-channel leakage between shards
                clear_sensitive_cpu_state();

                // Switch to the shard's kernel context
                unsafe {
                    context_switch(
                        &raw mut SUPERVISOR_RSP,
                        &raw const shard.saved_kernel_rsp,
                    );
                }

                // Shard yielded/blocked/exited — back here
                // Restore supervisor page tables
                vmm::write_cr3(crate::highhalf::supervisor_pml4());

                // Restore kernel data segments
                let kernel_ds = gdt::KERNEL_DS as u64;
                unsafe {
                    core::arch::asm!(
                        "mov ds, {0:x}",
                        "mov es, {0:x}",
                        in(reg) kernel_ds,
                        options(nostack, preserves_flags),
                    );
                }
            }
            None => {
                if all_done() {
                    break;
                }
                panic!("deadlock: all shards blocked, none ready");
            }
        }
    }

    // Snapshot stats before destroying shards (destroy zeroes memory)
    print_profiling_summary();

    // Destroy all exited shards
    for id in 0..MAX_SHARDS {
        let state = unsafe { (*(&raw const SHARDS))[id].state };
        if state == ShardState::Exited {
            shard::destroy(id);
        }
    }

    crate::serial_println!();
    crate::serial_println!("coconutOS supervisor v0.3.4: all shards completed.");
    crate::serial_println!("Halting.");

    crate::halt();
}

/// Print per-shard profiling stats collected during the run.
fn print_profiling_summary() {
    let shards = unsafe { &*(&raw const SHARDS) };

    crate::serial_println!();
    crate::serial_println!("--- Shard Profiling Summary ---");
    crate::serial_println!(
        "ID  Syscalls  Cycles/Syscall  Switches  Wall (ms)  Name"
    );

    for id in 0..MAX_SHARDS {
        let s = &shards[id];
        if s.state == ShardState::Free {
            continue;
        }

        let avg_cycles = if s.syscall_count > 0 {
            s.syscall_cycles / s.syscall_count
        } else {
            0
        };

        let wall_ms = if s.first_scheduled_tick > 0 && s.last_exited_tick >= s.first_scheduled_tick
        {
            // PIT is ~1 kHz, so ticks ≈ milliseconds
            s.last_exited_tick - s.first_scheduled_tick
        } else {
            0
        };

        // Extract name from null-padded byte array
        let name_len = s.name.iter().position(|&b| b == 0).unwrap_or(20);
        let name = core::str::from_utf8(&s.name[..name_len]).unwrap_or("?");

        crate::serial_println!(
            "{:2}  {:>8}  {:>14}  {:>8}  {:>9}  {}",
            id, s.syscall_count, avg_cycles, s.context_switches, wall_ms, name
        );
    }
}
