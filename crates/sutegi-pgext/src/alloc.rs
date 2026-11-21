//! The global allocator: the system allocator, plus PostgreSQL's
//! out-of-memory hook.
//!
//! When an allocation fails, Rust aborts the process. A backend killed by
//! `SIGABRT` is a crash to PostgreSQL: the postmaster terminates every
//! session and replays WAL, so one query asking a Rust function for too much
//! memory would take the whole server down. The C runtime's `_PG_init`
//! registers a hook that raises `FATAL` instead: only this session ends,
//! through PostgreSQL's normal exit path. Before the hook is registered (unit
//! tests, the `sutegi` CLI) a failure aborts as usual.
//!
//! An extension crate can't define its own `#[global_allocator]` next to this
//! one.
#![allow(
    unsafe_code,
    reason = "a GlobalAlloc implementation is unsafe by definition"
)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::OnceLock;

static OOM_HOOK: OnceLock<extern "C" fn(usize)> = OnceLock::new();

/// Called once by the C runtime's `_PG_init` with the function that raises
/// `FATAL`. Later calls are ignored.
#[unsafe(no_mangle)]
pub extern "C" fn sutegi_pgext_on_load(oom: extern "C" fn(usize)) {
    let _first_wins = OOM_HOOK.set(oom);
}

fn out_of_memory(size: usize) {
    if let Some(hook) = OOM_HOOK.get() {
        hook(size);
    }
}

struct PgAwareAllocator;

// SAFETY: every method forwards to `System` with the caller's arguments
// unchanged, so `System`'s guarantees carry over; the hook only runs when
// `System` already failed, and it doesn't return.
unsafe impl GlobalAlloc for PgAwareAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded unchanged; the caller upholds `alloc`'s contract.
        let p = unsafe { System.alloc(layout) };
        if p.is_null() {
            out_of_memory(layout.size());
        }
        p
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded unchanged.
        let p = unsafe { System.alloc_zeroed(layout) };
        if p.is_null() {
            out_of_memory(layout.size());
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: forwarded unchanged; `ptr` came from this allocator, i.e.
        // from `System`.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: forwarded unchanged.
        let p = unsafe { System.realloc(ptr, layout, new_size) };
        if p.is_null() {
            out_of_memory(new_size);
        }
        p
    }
}

#[global_allocator]
static GLOBAL: PgAwareAllocator = PgAwareAllocator;
