//! Virtual memory management — 4-level x86-64 page tables.
//!
//! Provides map/unmap for 4 KiB and 2 MiB pages, plus HHDM (higher-half
//! direct map) helpers for converting between physical and virtual addresses.

use core::arch::asm;

use crate::frame;

/// Higher-half direct map offset: all physical memory is mapped at this virtual base.
pub const HHDM_OFFSET: u64 = 0xFFFF_8000_0000_0000;

/// Whether the higher-half mapping is active (controls phys_to_virt behavior).
static mut HIGHER_HALF_ACTIVE: bool = false;

// Page table entry flags
pub const PTE_PRESENT: u64 = 1 << 0;
pub const PTE_WRITABLE: u64 = 1 << 1;
pub const PTE_USER: u64 = 1 << 2;
pub const PTE_WRITE_THROUGH: u64 = 1 << 3;
pub const PTE_CACHE_DISABLE: u64 = 1 << 4;
pub const PTE_PAGE_SIZE_2M: u64 = 1 << 7; // PS bit for 2 MiB pages at PD level
pub const PTE_NO_EXECUTE: u64 = 1 << 63;

const ENTRIES_PER_TABLE: usize = 512;
const PAGE_4K: u64 = 4096;
const PAGE_2M: u64 = 2 * 1024 * 1024;

/// A page table entry — transparent wrapper around u64.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct PageTableEntry(pub u64);

impl PageTableEntry {
    pub const fn empty() -> Self {
        Self(0)
    }

    pub fn is_present(&self) -> bool {
        self.0 & PTE_PRESENT != 0
    }

    pub fn phys_addr(&self) -> u64 {
        self.0 & 0x000F_FFFF_FFFF_F000
    }

    pub fn flags(&self) -> u64 {
        self.0 & !0x000F_FFFF_FFFF_F000
    }

    pub fn set(&mut self, phys: u64, flags: u64) {
        self.0 = (phys & 0x000F_FFFF_FFFF_F000) | flags;
    }

    pub fn clear(&mut self) {
        self.0 = 0;
    }
}

/// A 4 KiB-aligned page table with 512 entries.
#[repr(C, align(4096))]
pub struct PageTable {
    pub entries: [PageTableEntry; ENTRIES_PER_TABLE],
}

/// Convert a physical address to a virtual pointer.
/// Before higher-half is active, this is identity (phys == virt).
/// After higher-half, uses HHDM_OFFSET.
pub fn phys_to_virt(phys: u64) -> *mut u8 {
    unsafe {
        if *(&raw const HIGHER_HALF_ACTIVE) {
            (phys + HHDM_OFFSET) as *mut u8
        } else {
            phys as *mut u8
        }
    }
}

/// Convert a virtual address back to physical.
pub fn virt_to_phys(virt: u64) -> u64 {
    unsafe {
        if *(&raw const HIGHER_HALF_ACTIVE) {
            virt - HHDM_OFFSET
        } else {
            virt
        }
    }
}

/// Mark the higher-half as active (called after CR3 switch).
pub fn set_higher_half_active() {
    unsafe {
        *(&raw mut HIGHER_HALF_ACTIVE) = true;
    }
}

/// Get a mutable reference to a page table at a physical address.
fn table_at(phys: u64) -> &'static mut PageTable {
    unsafe { &mut *(phys_to_virt(phys) as *mut PageTable) }
}

/// Extract the PML4 index (bits 47:39) from a virtual address.
fn pml4_index(virt: u64) -> usize {
    ((virt >> 39) & 0x1FF) as usize
}

/// Extract the PDPT index (bits 38:30) from a virtual address.
fn pdpt_index(virt: u64) -> usize {
    ((virt >> 30) & 0x1FF) as usize
}

/// Extract the PD index (bits 29:21) from a virtual address.
fn pd_index(virt: u64) -> usize {
    ((virt >> 21) & 0x1FF) as usize
}

/// Extract the PT index (bits 20:12) from a virtual address.
fn pt_index(virt: u64) -> usize {
    ((virt >> 12) & 0x1FF) as usize
}

/// Ensure a next-level table exists at the given entry, allocating if needed.
/// `parent_flags` are applied to the intermediate PTE (typically PRESENT|WRITABLE|USER).
fn ensure_table(entry: &mut PageTableEntry, parent_flags: u64) -> u64 {
    if entry.is_present() {
        entry.phys_addr()
    } else {
        let new_phys = frame::alloc_frame_zeroed().expect("VMM: out of frames for page table");
        entry.set(new_phys, parent_flags | PTE_PRESENT);
        new_phys
    }
}

/// Map a 4 KiB page: virt → phys with the given flags.
/// Allocates intermediate page tables as needed.
pub fn map_4k(pml4_phys: u64, virt: u64, phys: u64, flags: u64) {
    // W^X: no page may be both writable and executable
    assert!(
        flags & PTE_WRITABLE == 0 || flags & PTE_NO_EXECUTE != 0,
        "W^X: page cannot be both writable and executable"
    );

    // Intermediate entries need PRESENT|WRITABLE|USER so they don't
    // restrict the leaf entry's permissions.
    let parent_flags = PTE_PRESENT | PTE_WRITABLE | PTE_USER;

    let pml4 = table_at(pml4_phys);
    let pdpt_phys = ensure_table(&mut pml4.entries[pml4_index(virt)], parent_flags);

    let pdpt = table_at(pdpt_phys);
    let pd_phys = ensure_table(&mut pdpt.entries[pdpt_index(virt)], parent_flags);

    let pd = table_at(pd_phys);
    let pt_phys = ensure_table(&mut pd.entries[pd_index(virt)], parent_flags);

    let pt = table_at(pt_phys);
    pt.entries[pt_index(virt)].set(phys, flags | PTE_PRESENT);
}

/// Unmap a 4 KiB page. Returns the physical address that was mapped, or None.
pub fn unmap_4k(pml4_phys: u64, virt: u64) -> Option<u64> {
    let pml4 = table_at(pml4_phys);
    let pml4e = &pml4.entries[pml4_index(virt)];
    if !pml4e.is_present() {
        return None;
    }

    let pdpt = table_at(pml4e.phys_addr());
    let pdpte = &pdpt.entries[pdpt_index(virt)];
    if !pdpte.is_present() {
        return None;
    }

    let pd = table_at(pdpte.phys_addr());
    let pde = &pd.entries[pd_index(virt)];
    if !pde.is_present() {
        return None;
    }

    let pt = table_at(pde.phys_addr());
    let pte = &mut pt.entries[pt_index(virt)];
    if !pte.is_present() {
        return None;
    }

    let old_phys = pte.phys_addr();
    pte.clear();
    invlpg(virt);
    Some(old_phys)
}

/// Map a 2 MiB page at PD level: virt → phys with the given flags.
pub fn map_2m(pml4_phys: u64, virt: u64, phys: u64, flags: u64) {
    let parent_flags = PTE_PRESENT | PTE_WRITABLE;

    let pml4 = table_at(pml4_phys);
    let pdpt_phys = ensure_table(&mut pml4.entries[pml4_index(virt)], parent_flags);

    let pdpt = table_at(pdpt_phys);
    let pd_phys = ensure_table(&mut pdpt.entries[pdpt_index(virt)], parent_flags);

    let pd = table_at(pd_phys);
    pd.entries[pd_index(virt)].set(phys, flags | PTE_PRESENT | PTE_PAGE_SIZE_2M);
}

/// Invalidate TLB entry for a virtual address.
pub fn invlpg(virt: u64) {
    unsafe {
        asm!("invlpg [{}]", in(reg) virt, options(nostack, preserves_flags));
    }
}

/// Flush entire TLB by reloading CR3.
pub fn flush_tlb() {
    unsafe {
        let cr3: u64;
        asm!("mov {}, cr3", out(reg) cr3, options(nomem, nostack, preserves_flags));
        asm!("mov cr3, {}", in(reg) cr3, options(nostack, preserves_flags));
    }
}

/// Read the current CR3 value.
pub fn read_cr3() -> u64 {
    let cr3: u64;
    unsafe {
        asm!("mov {}, cr3", out(reg) cr3, options(nomem, nostack, preserves_flags));
    }
    cr3
}

/// Write a new CR3 value (switches page tables).
pub fn write_cr3(cr3: u64) {
    unsafe {
        asm!("mov cr3, {}", in(reg) cr3, options(nostack, preserves_flags));
    }
}

// ---------------------------------------------------------------------------
// MMIO mapping — bump allocator for device register regions
// ---------------------------------------------------------------------------

/// Virtual base for MMIO mappings. Uses PDPT_kern[511] (0xFFFFFFFFC0000000),
/// which is empty — the boot trampoline only populates PDPT_kern[510] for
/// the kernel text/data 1 GiB window. This avoids conflicting with existing
/// 2 MiB page entries in PD_kern.
const MMIO_VIRT_BASE: u64 = 0xFFFF_FFFF_C000_0000;

/// Next available virtual address for MMIO mappings.
static mut MMIO_NEXT: u64 = MMIO_VIRT_BASE;

/// Map a physical MMIO region into kernel virtual address space.
///
/// Returns a virtual pointer to the start of the mapped region (preserving
/// sub-page offset if phys_base is not page-aligned). Pages are mapped with
/// PCD+PWT (uncacheable) and NX (no execute).
pub fn map_mmio(phys_base: u64, size: u64) -> *mut u8 {
    let offset_in_page = phys_base & (PAGE_4K - 1);
    let aligned_phys = phys_base & !(PAGE_4K - 1);
    let aligned_size = (size + offset_in_page + PAGE_4K - 1) & !(PAGE_4K - 1);

    let pml4_phys = crate::highhalf::supervisor_pml4();

    let flags = PTE_PRESENT | PTE_WRITABLE | PTE_NO_EXECUTE | PTE_CACHE_DISABLE | PTE_WRITE_THROUGH;

    unsafe {
        let virt_base = *(&raw const MMIO_NEXT);

        let mut offset = 0u64;
        while offset < aligned_size {
            map_4k(pml4_phys, virt_base + offset, aligned_phys + offset, flags);
            offset += PAGE_4K;
        }

        *(&raw mut MMIO_NEXT) = virt_base + aligned_size;

        (virt_base + offset_in_page) as *mut u8
    }
}
