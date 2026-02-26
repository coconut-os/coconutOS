//! Minimal ELF64 parser — extracts PT_LOAD segments and the entry point.

/// ELF64 file header.
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct Elf64Header {
    pub e_ident: [u8; 16],
    pub e_type: u16,
    pub e_machine: u16,
    pub e_version: u32,
    pub e_entry: u64,
    pub e_phoff: u64,
    pub e_shoff: u64,
    pub e_flags: u32,
    pub e_ehsize: u16,
    pub e_phentsize: u16,
    pub e_phnum: u16,
    pub e_shentsize: u16,
    pub e_shnum: u16,
    pub e_shstrndx: u16,
}

/// ELF64 program header.
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct Elf64Phdr {
    pub p_type: u32,
    pub p_flags: u32,
    pub p_offset: u64,
    pub p_vaddr: u64,
    pub p_paddr: u64,
    pub p_filesz: u64,
    pub p_memsz: u64,
    pub p_align: u64,
}

/// PT_LOAD segment type.
pub const PT_LOAD: u32 = 1;

/// ELF magic bytes.
const ELF_MAGIC: [u8; 4] = [0x7f, b'E', b'L', b'F'];

/// A loadable segment extracted from the ELF.
pub struct LoadSegment {
    /// Offset into the ELF file data where this segment starts.
    pub file_offset: usize,
    /// Number of bytes to copy from the file.
    pub file_size: usize,
    /// Physical/virtual address to load the segment at.
    pub load_addr: u64,
    /// Total size in memory (may be larger than file_size; remainder is zeroed).
    pub mem_size: usize,
}

/// Parsed ELF information.
pub struct ElfInfo {
    /// Virtual address of the entry point.
    pub entry_point: u64,
    /// PT_LOAD segments.
    pub segments: [Option<LoadSegment>; 8],
    pub segment_count: usize,
}

/// Parse an ELF64 file from a byte buffer.
/// Returns the entry point and load segments, or an error message.
pub fn parse_elf64(data: &[u8]) -> Result<ElfInfo, &'static str> {
    if data.len() < core::mem::size_of::<Elf64Header>() {
        return Err("ELF data too small for header");
    }

    let header = unsafe { &*(data.as_ptr() as *const Elf64Header) };

    // Validate magic
    if header.e_ident[0..4] != ELF_MAGIC {
        return Err("invalid ELF magic");
    }

    // Must be ELF64 (class 2)
    if header.e_ident[4] != 2 {
        return Err("not ELF64");
    }

    // Must be little-endian
    if header.e_ident[5] != 1 {
        return Err("not little-endian");
    }

    // Must be x86_64 (machine 0x3E)
    if header.e_machine != 0x3E {
        return Err("not x86_64");
    }

    let entry_point = header.e_entry;
    let ph_offset = header.e_phoff as usize;
    let ph_entsize = header.e_phentsize as usize;
    let ph_num = header.e_phnum as usize;

    let mut segments: [Option<LoadSegment>; 8] = [const { None }; 8];
    let mut segment_count = 0;

    for i in 0..ph_num {
        let offset = ph_offset + i * ph_entsize;
        if offset + core::mem::size_of::<Elf64Phdr>() > data.len() {
            return Err("program header out of bounds");
        }

        let phdr = unsafe { &*(data.as_ptr().add(offset) as *const Elf64Phdr) };

        if phdr.p_type == PT_LOAD {
            if segment_count >= 8 {
                return Err("too many PT_LOAD segments");
            }
            segments[segment_count] = Some(LoadSegment {
                file_offset: phdr.p_offset as usize,
                file_size: phdr.p_filesz as usize,
                load_addr: phdr.p_paddr,
                mem_size: phdr.p_memsz as usize,
            });
            segment_count += 1;
        }
    }

    Ok(ElfInfo {
        entry_point,
        segments,
        segment_count,
    })
}
