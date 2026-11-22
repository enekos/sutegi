//! The C runtime's ABI (`csrc/sutegi_pgext.h`). This is the crate's only
//! `unsafe` surface: the rest of the crate goes through [`Call`], which keeps
//! the pointer and lifetime invariants in one place.
//!
//! Every `sx_*` helper that returns `false` has parked an error on the call;
//! the caller must stop and propagate [`Error::raised`].
#![allow(unsafe_code, reason = "the FFI boundary with the C runtime")]

use std::cell::Cell;
use std::marker::PhantomData;
use std::os::raw::c_char;

use crate::SqlState;
use crate::error::{Error, Result};

thread_local! {
    static CURRENT: Cell<*mut RawCall> = const { Cell::new(std::ptr::null_mut()) };
}

/// Runs `f` with the call running on this thread, for the helpers
/// (`notice`, `spi::…`) that don't take one. The `Call` gets a fresh
/// lifetime that can't outlive `f`, so nothing borrowed from it escapes.
pub(crate) fn with_current<R>(f: impl for<'a> FnOnce(Call<'a>) -> Result<R>) -> Result<R> {
    let raw = CURRENT.with(Cell::get);
    if raw.is_null() {
        return Err(Error::new(
            SqlState::FEATURE_NOT_SUPPORTED,
            "sutegi-pgext: no PostgreSQL function call is running on this thread",
        ));
    }
    // SAFETY: CURRENT is non-null only while `invoke` runs on this thread,
    // and `f` returns before `invoke` does.
    f(unsafe { Call::from_raw(raw) })
}

/// Whether PostgreSQL's `max_stack_depth` is exceeded.
pub(crate) fn stack_too_deep() -> bool {
    // SAFETY: a plain read of the stack pointer against PostgreSQL's limit;
    // it never raises.
    unsafe { sx_stack_too_deep() }
}

/// Restores the previous running call when dropped, so a nested call (SPI
/// running SQL that calls back into this extension) and unwinding leave
/// CURRENT right.
struct CurrentGuard(*mut RawCall);

impl Drop for CurrentGuard {
    fn drop(&mut self) {
        CURRENT.with(|c| c.set(self.0));
    }
}

/// The body of every `#[pg_function]` wrapper.
///
/// # Safety
///
/// `raw` must be the live call `sx_invoke` passed to the wrapper, and this
/// must run on the backend thread, inside that invocation.
pub unsafe fn invoke(
    raw: *mut RawCall,
    name: &str,
    nargs: i32,
    body: impl for<'a> FnOnce(Call<'a>) -> Result<()>,
) {
    let _restore = CurrentGuard(CURRENT.with(|c| c.replace(raw)));
    // SAFETY: the caller guarantees `raw` is live for this invocation; the
    // `Call` is confined to `run`, which returns before we do.
    let call = unsafe { Call::from_raw(raw) };
    crate::run(call, name, nargs, body);
}

/// The C runtime's per-call state. Only ever handled behind a pointer.
#[repr(C)]
pub struct RawCall {
    _opaque: [u8; 0],
    _not_send: PhantomData<*mut u8>,
}

/// One SQL value crossing the boundary (`sx_value` in the C header).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct RawValue {
    pub(crate) kind: i32,
    pub(crate) isnull: i32,
    pub(crate) i: i64,
    pub(crate) f: f64,
    pub(crate) ptr: *const c_char,
    pub(crate) len: usize,
}

const _: () = assert!(std::mem::size_of::<RawValue>() == 40);

impl RawValue {
    pub(crate) const fn null(kind: Kind) -> RawValue {
        RawValue {
            kind: kind as i32,
            isnull: 1,
            i: 0,
            f: 0.0,
            ptr: std::ptr::null(),
            len: 0,
        }
    }

    pub(crate) const fn int(kind: Kind, i: i64) -> RawValue {
        RawValue {
            isnull: 0,
            i,
            ..RawValue::null(kind)
        }
    }

    pub(crate) const fn float(kind: Kind, f: f64) -> RawValue {
        RawValue {
            isnull: 0,
            f,
            ..RawValue::null(kind)
        }
    }

    pub(crate) fn bytes(kind: Kind, b: &[u8]) -> RawValue {
        RawValue {
            isnull: 0,
            ptr: b.as_ptr().cast(),
            len: b.len(),
            ..RawValue::null(kind)
        }
    }
}

#[repr(C)]
pub(crate) struct RawSpiResult {
    pub(crate) nrows: u64,
    pub(crate) ncols: i32,
    pub(crate) processed: u64,
    pub(crate) names: *const *const c_char,
    pub(crate) values: *const RawValue,
    pub(crate) cxt: *mut std::ffi::c_void,
}

const _: () = assert!(std::mem::size_of::<RawSpiResult>() == 48);

/// The SQL types a value can have on the boundary (`sx_kind`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
pub enum Kind {
    Void = 0,
    Bool = 1,
    Int2 = 2,
    Int4 = 3,
    Int8 = 4,
    Float4 = 5,
    Float8 = 6,
    Text = 7,
    Bytea = 8,
}

#[cfg(not(test))]
unsafe extern "C" {
    fn sx_nargs(call: *const RawCall) -> i32;
    fn sx_arg(call: *mut RawCall, index: i32, kind: i32, out: *mut RawValue) -> bool;
    fn sx_ret(call: *mut RawCall, value: *const RawValue) -> bool;
    fn sx_srf_begin(call: *mut RawCall, kind: i32) -> bool;
    fn sx_srf_push(call: *mut RawCall, value: *const RawValue) -> bool;
    fn sx_set_error(
        call: *mut RawCall,
        sqlstate: *const c_char,
        message: *const c_char,
        message_len: usize,
        detail: *const c_char,
        detail_len: usize,
        hint: *const c_char,
        hint_len: usize,
    );
    fn sx_log(call: *mut RawCall, level: i32, message: *const c_char, len: usize) -> bool;
    fn sx_check_interrupts(call: *mut RawCall) -> bool;
    fn sx_spi_exec(
        call: *mut RawCall,
        sql: *const c_char,
        sql_len: usize,
        nparams: i32,
        params: *const RawValue,
        limit: i64,
        out: *mut *const RawSpiResult,
    ) -> bool;
    fn sx_spi_free(res: *const RawSpiResult);
    fn sx_stack_too_deep() -> bool;
}

#[cfg(test)]
use crate::fake::{
    sx_arg, sx_check_interrupts, sx_log, sx_nargs, sx_ret, sx_set_error, sx_spi_exec, sx_spi_free,
    sx_srf_begin, sx_srf_push, sx_stack_too_deep,
};

/// A live PostgreSQL function call.
///
/// `'a` is the call's lifetime: borrowed arguments (`&'a str`, `&'a [u8]`)
/// point into PostgreSQL memory that stays valid until the function returns.
/// `!Send`: a call may only be used on the backend thread that received it.
#[derive(Clone, Copy)]
pub struct Call<'a> {
    raw: *mut RawCall,
    _call: PhantomData<&'a mut RawCall>,
}

fn ok(succeeded: bool) -> Result<()> {
    if succeeded {
        Ok(())
    } else {
        Err(Error::raised())
    }
}

impl<'a> Call<'a> {
    /// # Safety
    ///
    /// `raw` must be the call `sx_invoke` handed to a generated wrapper, and
    /// the returned `Call` must not outlive that wrapper's invocation.
    pub(crate) unsafe fn from_raw(raw: *mut RawCall) -> Call<'a> {
        Call {
            raw,
            _call: PhantomData,
        }
    }

    pub(crate) fn nargs(self) -> i32 {
        // SAFETY: `raw` is live for 'a (from_raw's contract).
        unsafe { sx_nargs(self.raw) }
    }

    /// Reads argument `index` as `kind`. Borrowed bytes live for `'a`: the C
    /// side detoasts into the function call's memory context.
    pub(crate) fn arg(self, index: i32, kind: Kind) -> Result<Arg<'a>> {
        let mut out = RawValue::null(kind);
        // SAFETY: `raw` is live for 'a and `out` is a valid sx_value to fill.
        ok(unsafe { sx_arg(self.raw, index, kind as i32, &mut out) })?;
        if out.isnull != 0 {
            return Ok(Arg::Null);
        }
        Ok(match kind {
            Kind::Text | Kind::Bytea => Arg::Bytes(if out.len == 0 {
                &[]
            } else {
                // SAFETY: sx_arg returned `len` readable bytes at `ptr` in the
                // call's memory context, which outlives 'a.
                unsafe { std::slice::from_raw_parts(out.ptr.cast::<u8>(), out.len) }
            }),
            Kind::Float4 | Kind::Float8 => Arg::Float(out.f),
            _ => Arg::Int(out.i),
        })
    }

    pub(crate) fn ret(self, value: &RawValue) -> Result<()> {
        // SAFETY: `raw` is live; `value` (and any bytes it points at) is
        // valid for the duration of the call, which copies it.
        ok(unsafe { sx_ret(self.raw, value) })
    }

    pub(crate) fn srf_begin(self, kind: Kind) -> Result<()> {
        // SAFETY: `raw` is live for 'a.
        ok(unsafe { sx_srf_begin(self.raw, kind as i32) })
    }

    pub(crate) fn srf_push(self, value: &RawValue) -> Result<()> {
        // SAFETY: as in `ret`: the C side copies the value into the set.
        ok(unsafe { sx_srf_push(self.raw, value) })
    }

    pub(crate) fn set_error(
        self,
        state: &[u8; 5],
        message: &str,
        detail: Option<&str>,
        hint: Option<&str>,
    ) {
        let message = without_nul(message);
        let detail = detail.map(without_nul);
        let hint = hint.map(without_nul);
        let detail = detail.as_deref();
        let hint = hint.as_deref();
        let (dp, dl) = detail.map_or((std::ptr::null(), 0), |d| (d.as_ptr().cast(), d.len()));
        let (hp, hl) = hint.map_or((std::ptr::null(), 0), |h| (h.as_ptr().cast(), h.len()));
        // SAFETY: every pointer/length pair is a live Rust slice (or null
        // with length 0) that the C side copies before returning.
        unsafe {
            sx_set_error(
                self.raw,
                state.as_ptr().cast(),
                message.as_ptr().cast(),
                message.len(),
                dp,
                dl,
                hp,
                hl,
            );
        }
    }

    pub(crate) fn log(self, level: i32, message: &str) -> Result<()> {
        let message = without_nul(message);
        // SAFETY: `message` is a live slice; the C side copies it.
        ok(unsafe { sx_log(self.raw, level, message.as_ptr().cast(), message.len()) })
    }

    pub(crate) fn check_interrupts(self) -> Result<()> {
        // SAFETY: `raw` is live for 'a.
        ok(unsafe { sx_check_interrupts(self.raw) })
    }

    /// Runs `sql` through SPI and copies the whole result into Rust.
    pub(crate) fn spi_exec(self, sql: &str, params: &[RawValue], limit: i64) -> Result<SpiResult> {
        let nparams = i32::try_from(params.len())
            .map_err(|_| Error::new(SqlState::PROGRAM_LIMIT_EXCEEDED, "too many SPI parameters"))?;
        let mut out: *const RawSpiResult = std::ptr::null();
        // SAFETY: `sql` and `params` (and the bytes they point at) are live
        // for the call, which copies them before running the query.
        ok(unsafe {
            sx_spi_exec(
                self.raw,
                sql.as_ptr().cast(),
                sql.len(),
                nparams,
                params.as_ptr(),
                limit,
                &mut out,
            )
        })?;
        // SAFETY: on success `out` points at a result in its own memory
        // context; read_spi copies everything out of it.
        let copied = unsafe { read_spi(out) };
        // SAFETY: `out` came from sx_spi_exec and nothing borrows it any
        // more: read_spi returned owned values.
        unsafe { sx_spi_free(out) };
        copied
    }
}

/// C copies messages as NUL-terminated strings: show an embedded NUL rather
/// than silently cutting the message at it.
fn without_nul(s: &str) -> std::borrow::Cow<'_, str> {
    if s.contains('\0') {
        std::borrow::Cow::Owned(s.replace('\0', "\\0"))
    } else {
        std::borrow::Cow::Borrowed(s)
    }
}

/// A raw argument before typed conversion.
pub(crate) enum Arg<'a> {
    Null,
    Int(i64),
    Float(f64),
    Bytes(&'a [u8]),
}

/// An SPI result copied into Rust-owned memory.
pub(crate) struct SpiResult {
    pub(crate) processed: u64,
    pub(crate) columns: Vec<String>,
    pub(crate) cells: Vec<Option<String>>,
}

/// # Safety
///
/// `res` must be null or point at a valid `sx_spi_result` whose `names`
/// holds `ncols` NUL-terminated strings and whose `values` holds
/// `nrows * ncols` text values.
unsafe fn read_spi(res: *const RawSpiResult) -> Result<SpiResult> {
    // SAFETY: the caller guarantees `res` is null or valid.
    let Some(res) = (unsafe { res.as_ref() }) else {
        return Err(Error::internal("SPI returned no result"));
    };
    let ncols =
        usize::try_from(res.ncols).map_err(|_| Error::internal("negative SPI column count"))?;
    let nrows = usize::try_from(res.nrows).map_err(|_| Error::internal("SPI result too large"))?;
    let names: &[*const c_char] = if ncols == 0 {
        &[]
    } else {
        // SAFETY: `names` holds `ncols` entries (caller contract).
        unsafe { std::slice::from_raw_parts(res.names, ncols) }
    };
    let mut columns = Vec::with_capacity(ncols);
    for &name in names {
        // SAFETY: each entry is a NUL-terminated string from the C side.
        let name = unsafe { std::ffi::CStr::from_ptr(name) };
        columns.push(name.to_string_lossy().into_owned());
    }
    let total = nrows
        .checked_mul(ncols)
        .ok_or_else(|| Error::internal("SPI result too large"))?;
    let values: &[RawValue] = if total == 0 {
        &[]
    } else {
        // SAFETY: `values` holds `nrows * ncols` entries (caller contract).
        unsafe { std::slice::from_raw_parts(res.values, total) }
    };
    let mut cells = Vec::with_capacity(total);
    for v in values {
        if v.isnull != 0 {
            cells.push(None);
            continue;
        }
        let bytes = if v.len == 0 {
            &[][..]
        } else {
            // SAFETY: a non-null text value has `len` readable bytes at `ptr`.
            unsafe { std::slice::from_raw_parts(v.ptr.cast::<u8>(), v.len) }
        };
        cells.push(Some(crate::value::utf8(bytes)?.to_owned()));
    }
    Ok(SpiResult {
        processed: res.processed,
        columns,
        cells,
    })
}

#[cfg(test)]
pub(crate) mod test_support {
    pub(crate) use super::{RawSpiResult, RawValue};
}
