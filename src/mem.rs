//! Keeps the process's memory footprint down on Linux (glibc).
//!
//! glibc gives threads their own malloc arenas (up to 8 per core), and memory
//! freed in an arena mostly stays mapped. Downloads, covers and decoding each
//! run on short-lived threads, so after a spike the freed memory would linger
//! in a dozen arenas. Two arenas are plenty for this app; [`trim`] hands
//! what's free back to the OS after heavy work. Elsewhere these do nothing.

#[cfg(all(target_os = "linux", target_env = "gnu"))]
mod glibc {
    unsafe extern "C" {
        fn mallopt(param: i32, value: i32) -> i32;
        fn malloc_trim(pad: usize) -> i32;
    }

    /// `M_ARENA_MAX` from `<malloc.h>`.
    const M_ARENA_MAX: i32 = -8;

    pub fn tune() {
        // SAFETY: plain configuration call; called first thing in `main`.
        unsafe { mallopt(M_ARENA_MAX, 2) };
    }

    pub fn trim() {
        // SAFETY: releases free memory only; safe to call from any thread.
        unsafe { malloc_trim(0) };
    }
}

/// Configure the allocator. Call at the very start of `main`, before any
/// threads exist.
pub fn tune() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    glibc::tune();
}

/// Return freed memory to the OS (after a burst of allocations).
pub fn trim() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    glibc::trim();
}
