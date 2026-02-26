//! Higher-half kernel transition.
//!
//! The boot trampoline (_start in .text.boot) builds initial page tables
//! and switches CR3 before jumping to higher-half. This module provides
//! post-switch cleanup (removing identity mapping) and the PML4 accessor.

use core::arch::asm;

use crate::vmm;

/// The supervisor's PML4 physical address, set by the boot trampoline.
static mut SUPERVISOR_PML4: u64 = 0;

/// Set the supervisor PML4 address (called from boot trampoline).
pub fn set_supervisor_pml4(pml4_phys: u64) {
    unsafe {
        *(&raw mut SUPERVISOR_PML4) = pml4_phys;
    }
}

/// Get the supervisor's PML4 physical address.
pub fn supervisor_pml4() -> u64 {
    unsafe { *(&raw const SUPERVISOR_PML4) }
}

/// Remove the identity mapping for the low physical region.
/// Called after jumping to higher-half addresses.
pub fn remove_identity_mapping() {
    let pml4_phys = supervisor_pml4();

    // Clear PML4[0] which maps the identity-mapped low region
    let pml4 = vmm::phys_to_virt(pml4_phys) as *mut u64;
    unsafe {
        core::ptr::write_volatile(pml4, 0);
    }

    vmm::flush_tlb();

    let rip: u64;
    unsafe {
        asm!("lea {}, [rip]", out(reg) rip, options(nomem, nostack, preserves_flags));
    }
    crate::serial_println!("Higher-half: identity mapping removed, running at {:#x}", rip);
}
