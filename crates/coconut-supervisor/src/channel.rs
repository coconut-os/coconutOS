//! IPC channels for synchronous message passing between shards.
//!
//! Each channel connects exactly two shard endpoints. Messages are
//! single-buffered per direction (max 256 bytes). A recv on an empty
//! channel blocks the calling shard until the other endpoint sends.

use crate::scheduler;
use crate::shard::{ShardState, SHARDS};

pub const MAX_MSG_SIZE: usize = 256;
pub const MAX_CHANNELS: usize = 4;

struct Channel {
    active: bool,
    /// Shard IDs for endpoint 0 and endpoint 1.
    shard: [usize; 2],
    /// Single message buffer.
    buffer: [u8; MAX_MSG_SIZE],
    /// Length of pending message (0 = empty).
    msg_len: usize,
    /// Which endpoint the pending message is for (0 or 1).
    msg_for: usize,
}

static mut CHANNELS: [Channel; MAX_CHANNELS] = [const {
    Channel {
        active: false,
        shard: [usize::MAX; 2],
        buffer: [0u8; MAX_MSG_SIZE],
        msg_len: 0,
        msg_for: 0,
    }
}; MAX_CHANNELS];

/// Initialize a channel between two shards.
pub fn init(channel_id: usize, shard_a: usize, shard_b: usize) {
    assert!(channel_id < MAX_CHANNELS, "channel_id out of range");
    let ch = unsafe { &mut (*(&raw mut CHANNELS))[channel_id] };
    ch.active = true;
    ch.shard[0] = shard_a;
    ch.shard[1] = shard_b;
    ch.msg_len = 0;
}

/// Determine which endpoint index (0 or 1) a shard is on a channel.
/// Returns None if the shard is not an endpoint.
fn endpoint_of(ch: &Channel, shard_id: usize) -> Option<usize> {
    if ch.shard[0] == shard_id {
        Some(0)
    } else if ch.shard[1] == shard_id {
        Some(1)
    } else {
        None
    }
}

/// Send a message on a channel.
///
/// `buf` and `len` are kernel-accessible pointers (user buffer already validated
/// and readable via shard's CR3 during syscall).
///
/// Returns 0 on success.
pub fn send(channel_id: usize, sender_shard: usize, buf: *const u8, len: usize) -> u64 {
    if channel_id >= MAX_CHANNELS || len > MAX_MSG_SIZE || len == 0 {
        return u64::MAX;
    }

    let ch = unsafe { &mut (*(&raw mut CHANNELS))[channel_id] };
    if !ch.active {
        return u64::MAX;
    }

    let my_ep = match endpoint_of(ch, sender_shard) {
        Some(ep) => ep,
        None => return u64::MAX,
    };
    let other_ep = 1 - my_ep;
    let other_shard = ch.shard[other_ep];

    // Copy message into channel buffer
    unsafe {
        core::ptr::copy_nonoverlapping(buf, ch.buffer.as_mut_ptr(), len);
    }
    ch.msg_len = len;
    ch.msg_for = other_ep;

    // If the other shard is blocked waiting on this channel, wake it
    let other = unsafe { &mut (*(&raw mut SHARDS))[other_shard] };
    if other.state == ShardState::Blocked && other.blocked_on_channel == channel_id {
        other.state = ShardState::Ready;
        other.blocked_on_channel = usize::MAX;
    }

    0
}

/// Receive a message on a channel. May block if no message is available.
///
/// `buf` is a user-space virtual address (writable, validated by syscall handler).
/// `max_len` is the maximum bytes to receive.
///
/// Returns number of bytes received.
pub fn recv(channel_id: usize, receiver_shard: usize, buf: *mut u8, max_len: usize) -> u64 {
    if channel_id >= MAX_CHANNELS || max_len == 0 {
        return u64::MAX;
    }

    let ch = unsafe { &mut (*(&raw mut CHANNELS))[channel_id] };
    if !ch.active {
        return u64::MAX;
    }

    let my_ep = match endpoint_of(ch, receiver_shard) {
        Some(ep) => ep,
        None => return u64::MAX,
    };

    // Check if there's a message waiting for us
    if ch.msg_len > 0 && ch.msg_for == my_ep {
        // Message available — copy it out
        let copy_len = ch.msg_len.min(max_len);
        unsafe {
            core::ptr::copy_nonoverlapping(ch.buffer.as_ptr(), buf, copy_len);
        }
        ch.msg_len = 0;
        return copy_len as u64;
    }

    // No message — block
    crate::serial_println!(
        "Shard {}: blocked on channel {} recv",
        receiver_shard,
        channel_id
    );

    let shard = unsafe { &mut (*(&raw mut SHARDS))[receiver_shard] };
    shard.state = ShardState::Blocked;
    shard.blocked_on_channel = channel_id;

    // Yield to supervisor
    scheduler::schedule_yield();

    // Resumed — message should now be in the buffer
    // Need to re-borrow after yield (the reference was invalidated by context switch)
    let ch = unsafe { &mut (*(&raw mut CHANNELS))[channel_id] };
    if ch.msg_len > 0 && ch.msg_for == my_ep {
        let copy_len = ch.msg_len.min(max_len);
        unsafe {
            core::ptr::copy_nonoverlapping(ch.buffer.as_ptr(), buf, copy_len);
        }
        ch.msg_len = 0;
        copy_len as u64
    } else {
        // Shouldn't happen — we were woken because a message arrived
        u64::MAX
    }
}
