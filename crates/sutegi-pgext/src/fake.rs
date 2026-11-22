//! A Rust stand-in for the C runtime, so the marshalling, error and SPI
//! paths run under `cargo test` without a PostgreSQL server. It mimics the
//! C side's contract: a failing helper parks an error and poisons the call.
#![allow(unsafe_code, reason = "stands in for the C side of the FFI boundary")]

use std::cell::Cell;
use std::ffi::CString;
use std::os::raw::c_char;

use crate::ffi::test_support::{RawSpiResult, RawValue};
use crate::ffi::{Call, Kind, RawCall};
use crate::{__private, Result};

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Val {
    Null,
    Int(i64),
    Float(f64),
    Text(String),
    Bytes(Vec<u8>),
}

#[derive(Debug, PartialEq)]
pub(crate) enum Outcome {
    Value(Val),
    Set(Vec<Val>),
    Error(String, String),
    Pending,
    NoValue,
}

type SpiFixture = (Vec<&'static str>, Vec<Vec<Option<&'static str>>>);

struct SpiStore {
    _names: Vec<CString>,
    _name_ptrs: Vec<*const c_char>,
    _cells: Vec<String>,
    _values: Vec<RawValue>,
    result: RawSpiResult,
}

pub(crate) struct FakeCall {
    args: Vec<Val>,
    result: Option<Val>,
    set: Option<Vec<Val>>,
    error: Option<(String, String)>,
    pending: bool,
    spi: Option<SpiFixture>,
    spi_error: bool,
    spi_store: Option<SpiStore>,
}

impl FakeCall {
    pub(crate) fn new(args: Vec<Val>) -> FakeCall {
        FakeCall {
            args,
            result: None,
            set: None,
            error: None,
            pending: false,
            spi: None,
            spi_error: false,
            spi_store: None,
        }
    }

    pub(crate) fn with_spi(
        mut self,
        cols: Vec<&'static str>,
        rows: Vec<Vec<Option<&'static str>>>,
    ) -> FakeCall {
        self.spi = Some((cols, rows));
        self
    }

    pub(crate) fn with_spi_error(mut self) -> FakeCall {
        self.spi_error = true;
        self
    }

    pub(crate) fn invoke(
        mut self,
        name: &str,
        nargs: i32,
        body: impl for<'a> FnOnce(Call<'a>) -> Result<()>,
    ) -> Outcome {
        let raw = (&raw mut self).cast::<RawCall>();
        // SAFETY: `raw` points at this live FakeCall, which every fake sx_*
        // function below casts back; it outlives the invocation.
        unsafe { __private::invoke(raw, name, nargs, body) };
        if self.pending {
            Outcome::Pending
        } else if let Some((s, m)) = self.error {
            Outcome::Error(s, m)
        } else if let Some(rows) = self.set {
            Outcome::Set(rows)
        } else if let Some(v) = self.result {
            Outcome::Value(v)
        } else {
            Outcome::NoValue
        }
    }

    fn poisoned(&self) -> bool {
        self.pending || self.error.is_some()
    }
}

/// # Safety
///
/// `call` must point at a live `FakeCall`.
unsafe fn fake<'a>(call: *const RawCall) -> &'a mut FakeCall {
    // SAFETY: the caller guarantees `call` is a live FakeCall.
    unsafe { &mut *call.cast::<FakeCall>().cast_mut() }
}

/// # Safety
///
/// `v` must be a valid value whose bytes (if any) are readable.
unsafe fn to_val(v: &RawValue) -> Val {
    if v.isnull != 0 {
        return Val::Null;
    }
    let bytes = || {
        if v.len == 0 {
            Vec::new()
        } else {
            // SAFETY: the caller guarantees `len` readable bytes at `ptr`.
            unsafe { std::slice::from_raw_parts(v.ptr.cast::<u8>(), v.len) }.to_vec()
        }
    };
    match v.kind {
        k if k == Kind::Text as i32 => Val::Text(String::from_utf8(bytes()).unwrap_or_default()),
        k if k == Kind::Bytea as i32 => Val::Bytes(bytes()),
        k if k == Kind::Float4 as i32 || k == Kind::Float8 as i32 => Val::Float(v.f),
        k if k == Kind::Void as i32 => Val::Null,
        _ => Val::Int(v.i),
    }
}

pub(crate) unsafe fn sx_nargs(call: *const RawCall) -> i32 {
    // SAFETY: fake ABI contract.
    let f = unsafe { fake(call) };
    i32::try_from(f.args.len()).unwrap_or(i32::MAX)
}

pub(crate) unsafe fn sx_arg(call: *mut RawCall, index: i32, kind: i32, out: *mut RawValue) -> bool {
    // SAFETY: fake ABI contract.
    let f = unsafe { fake(call) };
    if f.poisoned() {
        return false;
    }
    let Some(v) = usize::try_from(index).ok().and_then(|i| f.args.get(i)) else {
        f.pending = true;
        return false;
    };
    let mut raw = RawValue::null(Kind::Void);
    raw.kind = kind;
    match v {
        Val::Null => {}
        Val::Int(i) => {
            raw.isnull = 0;
            raw.i = *i;
        }
        Val::Float(x) => {
            raw.isnull = 0;
            raw.f = *x;
        }
        Val::Text(s) => {
            raw.isnull = 0;
            raw.ptr = s.as_ptr().cast();
            raw.len = s.len();
        }
        Val::Bytes(b) => {
            raw.isnull = 0;
            raw.ptr = b.as_ptr().cast();
            raw.len = b.len();
        }
    }
    // SAFETY: `out` is a valid sx_value the caller owns.
    unsafe { out.write(raw) };
    true
}

pub(crate) unsafe fn sx_ret(call: *mut RawCall, value: *const RawValue) -> bool {
    // SAFETY: fake ABI contract.
    let f = unsafe { fake(call) };
    if f.poisoned() {
        return false;
    }
    // SAFETY: `value` is a valid sx_value for this call.
    let value = unsafe { &*value };
    // SAFETY: its bytes, if any, are readable for this call.
    f.result = Some(unsafe { to_val(value) });
    true
}

pub(crate) unsafe fn sx_srf_begin(call: *mut RawCall, _kind: i32) -> bool {
    // SAFETY: fake ABI contract.
    let f = unsafe { fake(call) };
    if f.poisoned() {
        return false;
    }
    f.set = Some(Vec::new());
    true
}

pub(crate) unsafe fn sx_srf_push(call: *mut RawCall, value: *const RawValue) -> bool {
    // SAFETY: fake ABI contract.
    let f = unsafe { fake(call) };
    if f.poisoned() {
        return false;
    }
    // SAFETY: `value` is a valid sx_value for this call.
    let value = unsafe { &*value };
    // SAFETY: its bytes, if any, are readable for this call.
    let v = unsafe { to_val(value) };
    match f.set.as_mut() {
        Some(rows) => {
            rows.push(v);
            true
        }
        None => {
            f.pending = true;
            false
        }
    }
}

#[allow(clippy::too_many_arguments, reason = "mirrors the C signature")]
pub(crate) unsafe fn sx_set_error(
    call: *mut RawCall,
    sqlstate: *const c_char,
    message: *const c_char,
    message_len: usize,
    _detail: *const c_char,
    _detail_len: usize,
    _hint: *const c_char,
    _hint_len: usize,
) {
    // SAFETY: fake ABI contract.
    let f = unsafe { fake(call) };
    if f.poisoned() {
        return;
    }
    // SAFETY: `sqlstate` points at 5 bytes, `message` at `message_len`.
    let state = unsafe { std::slice::from_raw_parts(sqlstate.cast::<u8>(), 5) };
    // SAFETY: as above.
    let msg = unsafe { std::slice::from_raw_parts(message.cast::<u8>(), message_len) };
    f.error = Some((
        String::from_utf8_lossy(state).into_owned(),
        String::from_utf8_lossy(msg).into_owned(),
    ));
}

pub(crate) unsafe fn sx_log(
    call: *mut RawCall,
    _level: i32,
    _message: *const c_char,
    _len: usize,
) -> bool {
    // SAFETY: fake ABI contract.
    !unsafe { fake(call) }.poisoned()
}

pub(crate) unsafe fn sx_check_interrupts(call: *mut RawCall) -> bool {
    // SAFETY: fake ABI contract.
    !unsafe { fake(call) }.poisoned()
}

pub(crate) unsafe fn sx_spi_exec(
    call: *mut RawCall,
    _sql: *const c_char,
    _sql_len: usize,
    _nparams: i32,
    _params: *const RawValue,
    _limit: i64,
    out: *mut *const RawSpiResult,
) -> bool {
    // SAFETY: fake ABI contract.
    let f = unsafe { fake(call) };
    if f.poisoned() {
        return false;
    }
    if f.spi_error {
        f.pending = true;
        return false;
    }
    let (cols, rows) = f.spi.clone().unwrap_or_default();
    let names: Vec<CString> = cols
        .iter()
        .map(|c| CString::new(*c).unwrap_or_default())
        .collect();
    let name_ptrs: Vec<*const c_char> = names.iter().map(|c| c.as_ptr()).collect();
    let cells: Vec<String> = rows
        .iter()
        .flatten()
        .map(|c| c.unwrap_or("").to_owned())
        .collect();
    let values: Vec<RawValue> = rows
        .iter()
        .flatten()
        .zip(&cells)
        .map(|(c, s)| match c {
            Some(_) => RawValue::bytes(Kind::Text, s.as_bytes()),
            None => RawValue::null(Kind::Text),
        })
        .collect();
    let result = RawSpiResult {
        nrows: rows.len() as u64,
        ncols: i32::try_from(cols.len()).unwrap_or(0),
        processed: rows.len() as u64,
        names: name_ptrs.as_ptr(),
        values: values.as_ptr(),
        cxt: std::ptr::null_mut(),
    };
    let store = f.spi_store.insert(SpiStore {
        _names: names,
        _name_ptrs: name_ptrs,
        _cells: cells,
        _values: values,
        result,
    });
    // SAFETY: `out` is a valid out-pointer; the store lives in the FakeCall.
    unsafe { out.write(&raw const store.result) };
    true
}

thread_local! {
    static SPI_FREED: Cell<usize> = const { Cell::new(0) };
    static STACK_TOO_DEEP: Cell<bool> = const { Cell::new(false) };
}

/// How many SPI results have been freed on this thread.
pub(crate) fn spi_freed() -> usize {
    SPI_FREED.with(Cell::get)
}

pub(crate) fn set_stack_too_deep(deep: bool) {
    STACK_TOO_DEEP.with(|c| c.set(deep));
}

pub(crate) unsafe fn sx_spi_free(_res: *const RawSpiResult) {
    SPI_FREED.with(|c| c.set(c.get() + 1));
}

pub(crate) unsafe fn sx_stack_too_deep() -> bool {
    STACK_TOO_DEEP.with(Cell::get)
}
