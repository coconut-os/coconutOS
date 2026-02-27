//! Capability system: kernel-side capability tokens for access control.
//!
//! Each shard has a fixed-size capability table (`caps` array in `ShardDescriptor`).
//! Capabilities are unforgeable — user-mode shards reference them by handle (index),
//! and the kernel validates access on every privileged operation.

use crate::shard::{CapEntry, MAX_CAPS_PER_SHARD, MAX_SHARDS, SHARDS};
use coconut_shared::{RIGHT_CHANNEL_GRANT, RIGHT_CHANNEL_RECV, RIGHT_CHANNEL_SEND};

/// Kernel-internal: insert a capability into a shard's table.
/// Returns the handle (slot index) on success, or None if the table is full.
pub fn grant_to_shard(shard_id: usize, cap_type: u8, resource_id: u32, rights: u16) -> Option<usize> {
    assert!(shard_id < MAX_SHARDS);
    let shard = unsafe { &mut (*(&raw mut SHARDS))[shard_id] };

    // Find a free slot
    for i in 0..MAX_CAPS_PER_SHARD {
        if !shard.caps[i].valid {
            shard.caps[i] = CapEntry {
                valid: true,
                cap_type,
                resource_id,
                rights,
            };

            let rights_str = format_channel_rights(cap_type, rights);
            crate::serial_println!(
                "Capability: shard {} granted CAP_{}({}) [{}]",
                shard_id,
                cap_type_name(cap_type),
                resource_id,
                rights_str
            );

            return Some(i);
        }
    }
    None
}

/// Check whether a shard holds a capability matching the given type, resource, and rights.
pub fn check(shard_id: usize, cap_type: u8, resource_id: u32, required_rights: u16) -> bool {
    if shard_id >= MAX_SHARDS {
        return false;
    }
    let shard = unsafe { &(*(&raw const SHARDS))[shard_id] };
    for i in 0..MAX_CAPS_PER_SHARD {
        let cap = &shard.caps[i];
        if cap.valid
            && cap.cap_type == cap_type
            && cap.resource_id == resource_id
            && (cap.rights & required_rights) == required_rights
        {
            return true;
        }
    }
    false
}

/// Revoke a capability by handle (clear the slot).
/// Returns 0 on success, u64::MAX on error.
pub fn revoke(shard_id: usize, handle: usize) -> u64 {
    if shard_id >= MAX_SHARDS || handle >= MAX_CAPS_PER_SHARD {
        return u64::MAX;
    }
    let shard = unsafe { &mut (*(&raw mut SHARDS))[shard_id] };
    if !shard.caps[handle].valid {
        return u64::MAX;
    }
    shard.caps[handle] = CapEntry {
        valid: false,
        cap_type: 0,
        resource_id: 0,
        rights: 0,
    };
    0
}

/// Restrict rights on a capability (monotonic AND — can only remove rights, never add).
/// Returns 0 on success, u64::MAX on error.
pub fn restrict(shard_id: usize, handle: usize, new_rights: u16) -> u64 {
    if shard_id >= MAX_SHARDS || handle >= MAX_CAPS_PER_SHARD {
        return u64::MAX;
    }
    let shard = unsafe { &mut (*(&raw mut SHARDS))[shard_id] };
    if !shard.caps[handle].valid {
        return u64::MAX;
    }
    shard.caps[handle].rights &= new_rights;
    0
}

/// Copy a capability from one shard to another, with optionally restricted rights.
/// The source must hold RIGHT_CHANNEL_GRANT for channel capabilities.
/// Returns the target handle on success, u64::MAX on error.
pub fn grant_copy(src_shard: usize, src_handle: usize, target_shard: usize, new_rights: u16) -> u64 {
    if src_shard >= MAX_SHARDS
        || target_shard >= MAX_SHARDS
        || src_handle >= MAX_CAPS_PER_SHARD
    {
        return u64::MAX;
    }

    let src_cap = unsafe {
        let shard = &(*(&raw const SHARDS))[src_shard];
        if !shard.caps[src_handle].valid {
            return u64::MAX;
        }
        shard.caps[src_handle]
    };

    // For channel capabilities, the source must hold the GRANT right
    if src_cap.cap_type == coconut_shared::CAP_CHANNEL
        && (src_cap.rights & RIGHT_CHANNEL_GRANT) == 0
    {
        return u64::MAX;
    }

    // New rights are the intersection of source rights and requested rights
    let effective_rights = src_cap.rights & new_rights;

    match grant_to_shard(target_shard, src_cap.cap_type, src_cap.resource_id, effective_rights) {
        Some(handle) => handle as u64,
        None => u64::MAX,
    }
}

/// Inspect a capability by handle.
/// Returns packed value: (cap_type << 48) | (resource_id << 16) | rights
/// Returns u64::MAX if invalid.
pub fn inspect(shard_id: usize, handle: usize) -> u64 {
    if shard_id >= MAX_SHARDS || handle >= MAX_CAPS_PER_SHARD {
        return u64::MAX;
    }
    let shard = unsafe { &(*(&raw const SHARDS))[shard_id] };
    let cap = &shard.caps[handle];
    if !cap.valid {
        return u64::MAX;
    }
    ((cap.cap_type as u64) << 48) | ((cap.resource_id as u64) << 16) | (cap.rights as u64)
}

/// Clear all capabilities for a shard (called on destroy).
pub fn clear_shard(shard_id: usize) {
    if shard_id >= MAX_SHARDS {
        return;
    }
    let shard = unsafe { &mut (*(&raw mut SHARDS))[shard_id] };
    for i in 0..MAX_CAPS_PER_SHARD {
        shard.caps[i] = CapEntry {
            valid: false,
            cap_type: 0,
            resource_id: 0,
            rights: 0,
        };
    }
}

fn cap_type_name(cap_type: u8) -> &'static str {
    match cap_type {
        1 => "CHANNEL",
        2 => "SHARD",
        3 => "MEMORY",
        4 => "GPU_DMA",
        _ => "UNKNOWN",
    }
}

fn format_channel_rights(cap_type: u8, rights: u16) -> &'static str {
    if cap_type != coconut_shared::CAP_CHANNEL {
        return "ALL";
    }
    let send = (rights & RIGHT_CHANNEL_SEND) != 0;
    let recv = (rights & RIGHT_CHANNEL_RECV) != 0;
    let grant = (rights & RIGHT_CHANNEL_GRANT) != 0;
    match (send, recv, grant) {
        (true, false, false) => "SEND",
        (false, true, false) => "RECV",
        (true, true, false) => "SEND|RECV",
        (true, false, true) => "SEND|GRANT",
        (false, true, true) => "RECV|GRANT",
        (true, true, true) => "SEND|RECV|GRANT",
        (false, false, true) => "GRANT",
        (false, false, false) => "NONE",
    }
}
