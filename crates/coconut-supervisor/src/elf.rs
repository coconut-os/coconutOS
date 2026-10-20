//! ELF64 parsing on `&[u8]` — extracts the entry point and PT_LOAD segments.
//!
//! Only parses shard images embedded at build time (linked by targets/shard.ld),
//! so a malformed image is a kernel build bug: parsing asserts instead of
//! returning errors.

/// Shard images have at most three PT_LOAD segments: text, rodata, data+bss.
pub const MAX_SEGMENTS: usize = 3;

const EHDR_SIZE: usize = 64;
const PHDR_SIZE: usize = 56;

const ELFCLASS64: u8 = 2;
const ELFDATA2LSB: u8 = 1;
const ET_EXEC: u16 = 2;
const EM_X86_64: u16 = 0x3E;
const PT_LOAD: u32 = 1;
const PF_X: u32 = 1 << 0;
const PF_W: u32 = 1 << 1;

#[derive(Clone, Copy)]
pub struct Segment {
    pub vaddr: u64,
    /// Size in memory; bytes past `file_size` are zero (.bss).
    pub mem_size: u64,
    pub file_offset: usize,
    pub file_size: usize,
    pub writable: bool,
    pub executable: bool,
}

pub struct Image {
    pub entry: u64,
    pub segments: [Segment; MAX_SEGMENTS],
    pub segment_count: usize,
}

fn read_u16(data: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([data[off], data[off + 1]])
}

fn read_u32(data: &[u8], off: usize) -> u32 {
    let mut bytes = [0u8; 4];
    bytes.copy_from_slice(&data[off..off + 4]);
    u32::from_le_bytes(bytes)
}

fn read_u64(data: &[u8], off: usize) -> u64 {
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&data[off..off + 8]);
    u64::from_le_bytes(bytes)
}

/// Parse an x86-64 executable. Empty PT_LOAD segments are skipped.
pub fn parse(data: &[u8]) -> Image {
    assert!(data.len() >= EHDR_SIZE, "ELF: image smaller than header");
    assert!(data[0..4] == [0x7F, b'E', b'L', b'F'], "ELF: bad magic");
    assert!(data[4] == ELFCLASS64, "ELF: not ELF64");
    assert!(data[5] == ELFDATA2LSB, "ELF: not little-endian");
    assert!(read_u16(data, 16) == ET_EXEC, "ELF: not an executable");
    assert!(read_u16(data, 18) == EM_X86_64, "ELF: not x86-64");
    assert!(
        read_u16(data, 54) as usize == PHDR_SIZE,
        "ELF: bad phdr size"
    );

    let entry = read_u64(data, 24);
    let ph_offset = read_u64(data, 32) as usize;
    let ph_count = read_u16(data, 56) as usize;
    let ph_end = ph_offset.checked_add(ph_count * PHDR_SIZE);
    assert!(
        ph_end.is_some_and(|end| end <= data.len()),
        "ELF: phdrs out of bounds"
    );

    let empty = Segment {
        vaddr: 0,
        mem_size: 0,
        file_offset: 0,
        file_size: 0,
        writable: false,
        executable: false,
    };
    let mut image = Image {
        entry,
        segments: [empty; MAX_SEGMENTS],
        segment_count: 0,
    };

    for i in 0..ph_count {
        let ph = ph_offset + i * PHDR_SIZE;
        let mem_size = read_u64(data, ph + 40);
        if read_u32(data, ph) != PT_LOAD || mem_size == 0 {
            continue;
        }
        let flags = read_u32(data, ph + 4);
        let file_offset = read_u64(data, ph + 8) as usize;
        let file_size = read_u64(data, ph + 32) as usize;

        assert!(file_size as u64 <= mem_size, "ELF: filesz > memsz");
        let file_end = file_offset.checked_add(file_size);
        assert!(
            file_end.is_some_and(|end| end <= data.len()),
            "ELF: segment out of bounds"
        );
        assert!(
            image.segment_count < MAX_SEGMENTS,
            "ELF: too many PT_LOAD segments"
        );

        image.segments[image.segment_count] = Segment {
            vaddr: read_u64(data, ph + 16),
            mem_size,
            file_offset,
            file_size,
            writable: flags & PF_W != 0,
            executable: flags & PF_X != 0,
        };
        image.segment_count += 1;
    }

    image
}
