//! PCI bus enumeration via legacy I/O ports (0xCF8/0xCFC).
//!
//! Scans all buses/devices/functions and caches discovered devices.
//! Uses Configuration Mechanism #1 (256 bytes per function).

use core::arch::asm;

const CONFIG_ADDRESS: u16 = 0xCF8;
const CONFIG_DATA: u16 = 0xCFC;

const MAX_DEVICES: usize = 64;

/// PCI class code for display controllers.
const CLASS_DISPLAY: u8 = 0x03;

// ---------------------------------------------------------------------------
// Port I/O
// ---------------------------------------------------------------------------

#[inline(always)]
unsafe fn outl(port: u16, value: u32) {
    unsafe {
        asm!("out dx, eax", in("dx") port, in("eax") value, options(nomem, nostack, preserves_flags));
    }
}

#[inline(always)]
unsafe fn inl(port: u16) -> u32 {
    let value: u32;
    unsafe {
        asm!("in eax, dx", out("eax") value, in("dx") port, options(nomem, nostack, preserves_flags));
    }
    value
}

// ---------------------------------------------------------------------------
// Config space access
// ---------------------------------------------------------------------------

/// Read a 32-bit value from PCI config space.
/// Offset must be 4-byte aligned.
pub fn config_read32(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    let addr: u32 = 0x8000_0000
        | ((bus as u32) << 16)
        | ((device as u32) << 11)
        | ((function as u32) << 8)
        | ((offset as u32) & 0xFC);

    // Sound: I/O port access is always valid on x86; CONFIG_ADDRESS/DATA are
    // the standard PCI configuration mechanism.
    unsafe {
        outl(CONFIG_ADDRESS, addr);
        inl(CONFIG_DATA)
    }
}

// ---------------------------------------------------------------------------
// Device storage
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
pub struct PciDevice {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
    pub vendor_id: u16,
    pub device_id: u16,
    pub class: u8,
    pub subclass: u8,
}

static mut DEVICES: [PciDevice; MAX_DEVICES] = [PciDevice {
    bus: 0,
    device: 0,
    function: 0,
    vendor_id: 0,
    device_id: 0,
    class: 0,
    subclass: 0,
}; MAX_DEVICES];
static mut DEVICE_COUNT: usize = 0;
static mut GPU_COUNT: usize = 0;

// ---------------------------------------------------------------------------
// Enumeration
// ---------------------------------------------------------------------------

/// Scan all PCI buses and cache discovered devices.
pub fn init() {
    unsafe {
        *(&raw mut DEVICE_COUNT) = 0;
        *(&raw mut GPU_COUNT) = 0;
    }

    for bus in 0u16..256 {
        // Skip bus if no device at slot 0
        let bus = bus as u8;
        if config_read32(bus, 0, 0, 0) == 0xFFFF_FFFF {
            continue;
        }

        for device in 0u8..32 {
            let header0 = config_read32(bus, device, 0, 0);
            if header0 as u16 == 0xFFFF {
                continue;
            }

            probe_function(bus, device, 0);

            // Check if multi-function device (bit 7 of header type)
            let header_type = (config_read32(bus, device, 0, 0x0C) >> 16) as u8;
            if header_type & 0x80 != 0 {
                for function in 1u8..8 {
                    let id = config_read32(bus, device, function, 0);
                    if id as u16 != 0xFFFF {
                        probe_function(bus, device, function);
                    }
                }
            }
        }
    }

    let count = unsafe { *(&raw const DEVICE_COUNT) };
    let gpus = unsafe { *(&raw const GPU_COUNT) };
    crate::serial_println!("PCI: {} device(s), {} GPU(s)", count, gpus);
}

fn probe_function(bus: u8, device: u8, function: u8) {
    let id = config_read32(bus, device, function, 0);
    let vendor_id = id as u16;
    let device_id = (id >> 16) as u16;

    let class_reg = config_read32(bus, device, function, 0x08);
    let class = (class_reg >> 24) as u8;
    let subclass = (class_reg >> 16) as u8;

    let class_name = class_name(class, subclass);
    crate::serial_println!(
        "PCI: {:02x}:{:02x}.{} {:04x}:{:04x} ({:02x}:{:02x}) {}",
        bus, device, function, vendor_id, device_id, class, subclass, class_name
    );

    unsafe {
        let count = *(&raw const DEVICE_COUNT);
        if count < MAX_DEVICES {
            (*(&raw mut DEVICES))[count] = PciDevice {
                bus,
                device,
                function,
                vendor_id,
                device_id,
                class,
                subclass,
            };
            *(&raw mut DEVICE_COUNT) = count + 1;

            if class == CLASS_DISPLAY {
                *(&raw mut GPU_COUNT) += 1;
            }
        }
    }
}

/// Human-readable name for common PCI class/subclass combinations.
fn class_name(class: u8, subclass: u8) -> &'static str {
    match (class, subclass) {
        (0x00, 0x00) => "Non-VGA unclassified",
        (0x01, _) => "Mass storage controller",
        (0x02, _) => "Network controller",
        (0x03, 0x00) => "VGA compatible controller",
        (0x03, 0x80) => "Display controller",
        (0x03, _) => "Display controller",
        (0x04, _) => "Multimedia controller",
        (0x06, 0x00) => "Host bridge",
        (0x06, 0x01) => "ISA bridge",
        (0x06, 0x04) => "PCI bridge",
        (0x06, _) => "Bridge device",
        (0x08, _) => "System peripheral",
        (0x0C, 0x03) => "USB controller",
        (0x0C, _) => "Serial bus controller",
        _ => "Unknown",
    }
}

/// Number of discovered GPUs (class 0x03).
pub fn gpu_count() -> usize {
    unsafe { *(&raw const GPU_COUNT) }
}
