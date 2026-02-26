#![no_std]
#![no_main]

mod elf;

extern crate alloc;

use core::ptr;
use uefi::boot::{self, AllocateType, MemoryType};
use uefi::fs::FileSystem;
use uefi::mem::memory_map::MemoryMap;
use uefi::prelude::*;
use uefi::println;

use coconut_shared::{BootInfo, MemoryRegionDescriptor, MemoryRegionType, BOOT_INFO_MAGIC};

/// Path to the supervisor ELF on the boot filesystem.
const SUPERVISOR_PATH: &str = "\\EFI\\coconut\\supervisor.elf";

/// Physical address where the supervisor will be loaded (2 MiB).
const SUPERVISOR_LOAD_ADDR: u64 = 0x200000;

/// Maximum number of memory map entries we'll store.
const MAX_MMAP_ENTRIES: usize = 256;

#[entry]
fn main() -> Status {
    uefi::helpers::init().unwrap();

    println!("coconut-boot: loading supervisor from {}", SUPERVISOR_PATH);

    // -----------------------------------------------------------------------
    // 1. Load the supervisor ELF from the boot filesystem
    // -----------------------------------------------------------------------
    let elf_data = load_supervisor_elf();
    let elf_info = elf::parse_elf64(&elf_data).expect("failed to parse supervisor ELF");

    // -----------------------------------------------------------------------
    // 2. Allocate pages for the supervisor and copy segments
    // -----------------------------------------------------------------------
    let (total_loaded, page_count) = load_elf_segments(&elf_data, &elf_info);

    println!(
        "coconut-boot: supervisor loaded at {:#x} ({} bytes)",
        SUPERVISOR_LOAD_ADDR, total_loaded
    );

    // -----------------------------------------------------------------------
    // 3. Allocate space for BootInfo + memory map (persists after ExitBS)
    // -----------------------------------------------------------------------
    let boot_info_pages = 2; // 8 KiB — BootInfo + up to 256 descriptors
    let boot_info_mem = boot::allocate_pages(
        AllocateType::AnyPages,
        MemoryType::LOADER_DATA,
        boot_info_pages,
    )
    .expect("failed to allocate BootInfo pages");

    let boot_info_ptr = boot_info_mem.as_ptr() as *mut BootInfo;
    let mmap_ptr = unsafe {
        boot_info_mem
            .as_ptr()
            .add(core::mem::size_of::<BootInfo>()) as *mut MemoryRegionDescriptor
    };

    // -----------------------------------------------------------------------
    // 4. Exit boot services — no more UEFI calls after this point
    // -----------------------------------------------------------------------
    println!("coconut-boot: exiting boot services, jumping to supervisor...");

    let memory_map = unsafe { boot::exit_boot_services(MemoryType::LOADER_DATA) };

    // -----------------------------------------------------------------------
    // 5. Build the BootInfo struct from the UEFI memory map
    // -----------------------------------------------------------------------
    let mut mmap_count: u32 = 0;
    for desc in memory_map.entries() {
        if mmap_count as usize >= MAX_MMAP_ENTRIES {
            break;
        }

        let region_type = translate_memory_type(desc.ty);
        let size = desc.page_count * 4096;

        unsafe {
            ptr::write(
                mmap_ptr.add(mmap_count as usize),
                MemoryRegionDescriptor {
                    phys_start: desc.phys_start,
                    size,
                    region_type,
                },
            );
        }
        mmap_count += 1;
    }

    // Mark the supervisor region explicitly
    if (mmap_count as usize) < MAX_MMAP_ENTRIES {
        unsafe {
            ptr::write(
                mmap_ptr.add(mmap_count as usize),
                MemoryRegionDescriptor {
                    phys_start: SUPERVISOR_LOAD_ADDR,
                    size: page_count as u64 * 4096,
                    region_type: MemoryRegionType::SupervisorCode,
                },
            );
        }
        mmap_count += 1;
    }

    unsafe {
        ptr::write(
            boot_info_ptr,
            BootInfo {
                magic: BOOT_INFO_MAGIC,
                version: 1,
                memory_map_count: mmap_count,
                memory_map_addr: mmap_ptr as u64,
                supervisor_phys_base: SUPERVISOR_LOAD_ADDR,
                supervisor_size: total_loaded as u64,
            },
        );
    }

    // -----------------------------------------------------------------------
    // 6. Jump to the supervisor entry point
    //
    // Must use inline asm because the UEFI target uses Microsoft x64 ABI
    // (first arg in RCX) but the supervisor expects System V ABI (RDI).
    // -----------------------------------------------------------------------
    unsafe {
        core::arch::asm!(
            "mov rdi, {boot_info}",
            "jmp {entry}",
            boot_info = in(reg) boot_info_ptr,
            entry = in(reg) elf_info.entry_point,
            options(noreturn),
        );
    }
}

/// Load the supervisor ELF file from the boot filesystem into a Vec.
fn load_supervisor_elf() -> alloc::vec::Vec<u8> {
    let image_handle = boot::image_handle();
    let mut fs = FileSystem::new(
        boot::get_image_file_system(image_handle).expect("failed to open filesystem"),
    );

    let path = uefi::CString16::try_from(SUPERVISOR_PATH).expect("invalid path");
    fs.read(&*path).expect("failed to read supervisor ELF")
}

/// Allocate pages at SUPERVISOR_LOAD_ADDR and copy PT_LOAD segments.
/// Returns (total bytes loaded, pages allocated).
fn load_elf_segments(elf_data: &[u8], elf_info: &elf::ElfInfo) -> (usize, usize) {
    // Calculate total memory needed
    let mut max_addr: u64 = 0;
    let mut min_addr: u64 = u64::MAX;

    for i in 0..elf_info.segment_count {
        if let Some(seg) = &elf_info.segments[i] {
            if seg.load_addr < min_addr {
                min_addr = seg.load_addr;
            }
            let seg_end = seg.load_addr + seg.mem_size as u64;
            if seg_end > max_addr {
                max_addr = seg_end;
            }
        }
    }

    let total_size = (max_addr - min_addr) as usize;
    let page_count = (total_size + 4095) / 4096;

    // Allocate at the supervisor's expected load address
    boot::allocate_pages(
        AllocateType::Address(SUPERVISOR_LOAD_ADDR),
        MemoryType::LOADER_DATA,
        page_count,
    )
    .expect("failed to allocate pages for supervisor");

    // Zero the entire region first
    unsafe {
        ptr::write_bytes(SUPERVISOR_LOAD_ADDR as *mut u8, 0, page_count * 4096);
    }

    // Copy each segment
    let mut total_loaded = 0usize;
    for i in 0..elf_info.segment_count {
        if let Some(seg) = &elf_info.segments[i] {
            let dest = seg.load_addr as *mut u8;
            let src = &elf_data[seg.file_offset..seg.file_offset + seg.file_size];
            unsafe {
                ptr::copy_nonoverlapping(src.as_ptr(), dest, seg.file_size);
            }
            total_loaded += seg.file_size;
        }
    }

    (total_loaded, page_count)
}

/// Translate UEFI memory types to our memory region types.
fn translate_memory_type(uefi_type: MemoryType) -> MemoryRegionType {
    match uefi_type {
        MemoryType::CONVENTIONAL => MemoryRegionType::Usable,
        MemoryType::BOOT_SERVICES_CODE | MemoryType::BOOT_SERVICES_DATA => {
            MemoryRegionType::BootloaderReclaimable
        }
        MemoryType::ACPI_RECLAIM => MemoryRegionType::AcpiReclaimable,
        MemoryType::ACPI_NON_VOLATILE => MemoryRegionType::AcpiNvs,
        MemoryType::MMIO | MemoryType::MMIO_PORT_SPACE => MemoryRegionType::Mmio,
        _ => MemoryRegionType::Reserved,
    }
}
