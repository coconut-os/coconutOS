//! Filesystem syscall layer.
//!
//! Provides SYS_FS_OPEN, SYS_FS_READ, SYS_FS_STAT, SYS_FS_CLOSE syscall handlers
//! backed by the embedded ext2 ramdisk image.

use crate::ext2;
use crate::shard;

const MAX_OPEN_FILES: usize = 16;
const MAX_PATH_LEN: usize = 256;

static ROOTFS_IMAGE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/rootfs.ext2"));

struct OpenFile {
    active: bool,
    shard_id: usize,
    inode_nr: u32,
    size: u32,
    offset: u32,
}

static mut OPEN_FILES: [OpenFile; MAX_OPEN_FILES] = [const {
    OpenFile {
        active: false,
        shard_id: 0,
        inode_nr: 0,
        size: 0,
        offset: 0,
    }
}; MAX_OPEN_FILES];

/// Initialize the filesystem: parse the embedded ext2 image.
pub fn init() {
    ext2::init(ROOTFS_IMAGE);
    let files = ext2::file_count();
    let size_kb = ROOTFS_IMAGE.len() / 1024;
    crate::serial_println!(
        "Filesystem: ext2 ramdisk, {} KiB, {} file{}",
        size_kb,
        files,
        if files == 1 { "" } else { "s" }
    );
}

/// Validate that a user buffer [ptr, ptr+len) is readable (code or stack page).
fn validate_user_read_buf(ptr: u64, len: u64) -> bool {
    if len == 0 || len > 4096 {
        return false;
    }
    let end = ptr.wrapping_add(len);
    if end < ptr {
        return false;
    }
    // Code region
    if ptr >= 0x1000 && end <= 0x2000 {
        return true;
    }
    // Stack region
    if ptr >= 0x7FF000 && end <= 0x800000 {
        return true;
    }
    false
}

/// Validate that a user buffer for writing lies in the stack region.
fn validate_user_write_buf(ptr: u64, len: u64) -> bool {
    if len == 0 || len > 4096 {
        return false;
    }
    let end = ptr.wrapping_add(len);
    if end < ptr {
        return false;
    }
    ptr >= 0x7FF000 && end <= 0x800000
}

/// SYS_FS_OPEN: open a file by path. Returns fd or u64::MAX on error.
pub fn handle_fs_open(path_ptr: u64, path_len: u64) -> u64 {
    if path_len == 0 || path_len > MAX_PATH_LEN as u64 {
        return u64::MAX;
    }
    if !validate_user_read_buf(path_ptr, path_len) {
        return u64::MAX;
    }

    // Copy path from user space to kernel buffer
    let path = unsafe { core::slice::from_raw_parts(path_ptr as *const u8, path_len as usize) };

    // Look up the file in ext2
    let inode_nr = match ext2::lookup(path) {
        Some(nr) => nr,
        None => {
            crate::serial_println!("FS: open failed, file not found");
            return u64::MAX;
        }
    };

    let inode = match ext2::read_inode(inode_nr) {
        Some(i) => i,
        None => return u64::MAX,
    };

    // Allocate a file descriptor
    let caller = shard::current_shard();
    let open_files = unsafe { &mut *(&raw mut OPEN_FILES) };

    for fd in 0..MAX_OPEN_FILES {
        if !open_files[fd].active {
            open_files[fd] = OpenFile {
                active: true,
                shard_id: caller,
                inode_nr,
                size: inode.size,
                offset: 0,
            };

            // Build path string for logging
            let mut path_buf = [0u8; 64];
            let log_len = path_len.min(63) as usize;
            path_buf[..log_len].copy_from_slice(&path[..log_len]);

            crate::serial_print!("FS: open \"");
            for &b in &path_buf[..log_len] {
                crate::serial::write_byte(b);
            }
            crate::serial_println!("\" -> fd {} ({} bytes)", fd, inode.size);

            return fd as u64;
        }
    }

    u64::MAX // No free fd
}

/// SYS_FS_READ: read from an open file. Returns bytes read or u64::MAX on error.
pub fn handle_fs_read(fd: u64, buf_ptr: u64, max_len: u64) -> u64 {
    let fd = fd as usize;
    if fd >= MAX_OPEN_FILES {
        return u64::MAX;
    }

    let caller = shard::current_shard();
    let open_files = unsafe { &mut *(&raw mut OPEN_FILES) };
    let file = &mut open_files[fd];

    if !file.active || file.shard_id != caller {
        return u64::MAX;
    }

    if max_len == 0 || !validate_user_write_buf(buf_ptr, max_len) {
        return u64::MAX;
    }

    let inode = match ext2::read_inode(file.inode_nr) {
        Some(i) => i,
        None => return u64::MAX,
    };

    let buf = unsafe { core::slice::from_raw_parts_mut(buf_ptr as *mut u8, max_len as usize) };
    let bytes_read = ext2::read_data(&inode, file.offset, buf);
    file.offset += bytes_read as u32;

    crate::serial_println!("FS: read fd {}, {} bytes", fd, bytes_read);

    bytes_read as u64
}

/// SYS_FS_STAT: return file size for an open fd, or u64::MAX on error.
pub fn handle_fs_stat(fd: u64) -> u64 {
    let fd = fd as usize;
    if fd >= MAX_OPEN_FILES {
        return u64::MAX;
    }

    let caller = shard::current_shard();
    let open_files = unsafe { &*(&raw const OPEN_FILES) };
    let file = &open_files[fd];

    if !file.active || file.shard_id != caller {
        return u64::MAX;
    }

    file.size as u64
}

/// SYS_FS_CLOSE: close an open file descriptor. Returns 0 or u64::MAX on error.
pub fn handle_fs_close(fd: u64) -> u64 {
    let fd = fd as usize;
    if fd >= MAX_OPEN_FILES {
        return u64::MAX;
    }

    let caller = shard::current_shard();
    let open_files = unsafe { &mut *(&raw mut OPEN_FILES) };
    let file = &mut open_files[fd];

    if !file.active || file.shard_id != caller {
        return u64::MAX;
    }

    crate::serial_println!("FS: close fd {}", fd);

    file.active = false;
    file.shard_id = 0;
    file.inode_nr = 0;
    file.size = 0;
    file.offset = 0;

    0
}
