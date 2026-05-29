//! The C ABI surface linked into `StowApp` and `StowFileProvider.appex`.
//!
//! Contract:
//! * Every fallible entry point is wrapped in `catch_unwind` — a Rust panic
//!   becomes `StowStatus::Panic` (or a null return), never an unwind across FFI.
//! * Functions returning an `int32_t` write a human-readable message into
//!   `err_out` on failure; the caller frees it with `stow_string_free`.
//! * All heap strings returned to Swift are owned by the caller and must be
//!   freed with `stow_string_free`.
//! * Opaque handles (`StowCore`, `StowCancelToken`) are created by `*_new` and
//!   destroyed by the matching `*_free`; double-free / use-after-free is UB.

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::config::Config;
use crate::error::{StowError, StowResult, StowStatus};

/// Opaque handle to the Stow core. Holds parsed config now; in M1 it also owns
/// the Tokio runtime, the shared S3 client, and the SQLite connection pool.
pub struct StowCore {
    #[allow(dead_code)]
    config: Config,
}

/// Cooperative cancellation token. M0 uses a simple atomic flag; M1 swaps the
/// internals for a `tokio_util::sync::CancellationToken` while keeping this ABI.
pub struct StowCancelToken {
    cancelled: Arc<AtomicBool>,
}

/// Progress callback: invoked from download/upload tasks with bytes-done and
/// total. `ctx` is the opaque pointer the caller passed alongside it. Must be
/// safe to call from a background thread.
pub type StowProgressCb =
    Option<extern "C" fn(ctx: *mut c_void, done: u64, total: u64)>;

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Write `msg` into `*err_out` as a freshly allocated C string (if `err_out` is
/// non-null). Silently does nothing on allocation failure.
fn write_err(err_out: *mut *mut c_char, msg: &str) {
    if err_out.is_null() {
        return;
    }
    let c = CString::new(msg).unwrap_or_else(|_| CString::new("error").unwrap());
    unsafe { *err_out = c.into_raw() };
}

/// Run `f`, mapping success/error/panic to a `StowStatus` code and populating
/// `err_out`. Clears `*err_out` to null first.
fn guard_status<F>(err_out: *mut *mut c_char, f: F) -> c_int
where
    F: FnOnce() -> StowResult<()>,
{
    if !err_out.is_null() {
        unsafe { *err_out = ptr::null_mut() };
    }
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok(())) => StowStatus::Ok.code(),
        Ok(Err(e)) => {
            write_err(err_out, &e.to_string());
            e.status().code()
        }
        Err(_) => {
            write_err(err_out, "panic in stow_core");
            StowStatus::Panic.code()
        }
    }
}

/// Run `f` returning a heap pointer; on error/panic return null and set `err_out`.
fn guard_ptr<T, F>(err_out: *mut *mut c_char, f: F) -> *mut T
where
    F: FnOnce() -> StowResult<*mut T>,
{
    if !err_out.is_null() {
        unsafe { *err_out = ptr::null_mut() };
    }
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok(p)) => p,
        Ok(Err(e)) => {
            write_err(err_out, &e.to_string());
            ptr::null_mut()
        }
        Err(_) => {
            write_err(err_out, "panic in stow_core");
            ptr::null_mut()
        }
    }
}

/// Borrow a `&str` from a C string pointer, validating null + UTF-8.
fn cstr<'a>(p: *const c_char, name: &'static str) -> StowResult<&'a str> {
    if p.is_null() {
        return Err(StowError::InvalidArg(format!("{name} is null")));
    }
    unsafe { CStr::from_ptr(p) }
        .to_str()
        .map_err(|_| StowError::InvalidArg(format!("{name} is not valid UTF-8")))
}

// ---------------------------------------------------------------------------
// Version / string lifecycle
// ---------------------------------------------------------------------------

/// Return the core library version as a newly allocated C string. Free with
/// `stow_string_free`. Used as the trivial smoke-test call from Swift.
#[no_mangle]
pub extern "C" fn stow_core_version() -> *mut c_char {
    match CString::new(env!("CARGO_PKG_VERSION")) {
        Ok(c) => c.into_raw(),
        Err(_) => ptr::null_mut(),
    }
}

/// Free a string previously returned by this library.
///
/// # Safety
/// `s` must be a pointer returned by a `stow_*` function and not freed before.
#[no_mangle]
pub unsafe extern "C" fn stow_string_free(s: *mut c_char) {
    if !s.is_null() {
        drop(CString::from_raw(s));
    }
}

// ---------------------------------------------------------------------------
// Core lifecycle
// ---------------------------------------------------------------------------

/// Create a core handle from a JSON config string. Returns null on failure with
/// a message in `err_out`. Free with `stow_core_free`.
///
/// # Safety
/// `config_json` must be a valid NUL-terminated C string; `err_out` may be null.
#[no_mangle]
pub unsafe extern "C" fn stow_core_new(
    config_json: *const c_char,
    err_out: *mut *mut c_char,
) -> *mut StowCore {
    guard_ptr(err_out, || {
        let json = cstr(config_json, "config_json")?;
        let config = Config::from_json(json)?;
        Ok(Box::into_raw(Box::new(StowCore { config })))
    })
}

/// Destroy a core handle.
///
/// # Safety
/// `core` must have come from `stow_core_new` and not been freed before.
#[no_mangle]
pub unsafe extern "C" fn stow_core_free(core: *mut StowCore) {
    if !core.is_null() {
        drop(Box::from_raw(core));
    }
}

// ---------------------------------------------------------------------------
// Cancellation
// ---------------------------------------------------------------------------

/// Create a cancel token (starts un-cancelled). Free with `stow_cancel_token_free`.
#[no_mangle]
pub extern "C" fn stow_cancel_token_new() -> *mut StowCancelToken {
    Box::into_raw(Box::new(StowCancelToken {
        cancelled: Arc::new(AtomicBool::new(false)),
    }))
}

/// Signal cancellation. Safe to call from any thread (e.g. a `Progress`
/// cancellation handler). No-op if `token` is null.
///
/// # Safety
/// `token` must be a live token from `stow_cancel_token_new`.
#[no_mangle]
pub unsafe extern "C" fn stow_cancel(token: *mut StowCancelToken) {
    if let Some(t) = token.as_ref() {
        t.cancelled.store(true, Ordering::SeqCst);
    }
}

/// Free a cancel token.
///
/// # Safety
/// `token` must have come from `stow_cancel_token_new` and not been freed before.
#[no_mangle]
pub unsafe extern "C" fn stow_cancel_token_free(token: *mut StowCancelToken) {
    if !token.is_null() {
        drop(Box::from_raw(token));
    }
}

// ---------------------------------------------------------------------------
// Data plane (stubbed in M0 — real S3/index/crypto lands in M1/M3)
// ---------------------------------------------------------------------------

/// Download an S3 object to `out_path`, reporting progress and honoring `cancel`.
/// The rehydration hot path. **Stub in M0.**
///
/// # Safety
/// All non-null pointer args must be valid; see module contract.
#[no_mangle]
pub unsafe extern "C" fn stow_fetch_object(
    core: *mut StowCore,
    s3_key: *const c_char,
    out_path: *const c_char,
    _progress: StowProgressCb,
    _ctx: *mut c_void,
    _cancel: *const StowCancelToken,
    err_out: *mut *mut c_char,
) -> c_int {
    guard_status(err_out, || {
        if core.is_null() {
            return Err(StowError::InvalidArg("core is null".into()));
        }
        let _key = cstr(s3_key, "s3_key")?;
        let _out = cstr(out_path, "out_path")?;
        Err(StowError::Unimplemented("stow_fetch_object (lands in M1)"))
    })
}

/// Upload `local_path` to S3, returning the object key and content hash via the
/// out-params (caller frees with `stow_string_free`). **Stub in M0.**
///
/// # Safety
/// All non-null pointer args must be valid; see module contract.
#[no_mangle]
pub unsafe extern "C" fn stow_upload_object(
    core: *mut StowCore,
    local_path: *const c_char,
    _out_s3_key: *mut *mut c_char,
    _out_hash: *mut *mut c_char,
    err_out: *mut *mut c_char,
) -> c_int {
    guard_status(err_out, || {
        if core.is_null() {
            return Err(StowError::InvalidArg("core is null".into()));
        }
        let _path = cstr(local_path, "local_path")?;
        Err(StowError::Unimplemented("stow_upload_object (lands in M1)"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_roundtrips() {
        let p = stow_core_version();
        assert!(!p.is_null());
        let s = unsafe { CStr::from_ptr(p) }.to_str().unwrap().to_owned();
        assert_eq!(s, env!("CARGO_PKG_VERSION"));
        unsafe { stow_string_free(p) };
    }

    #[test]
    fn core_new_rejects_bad_config() {
        let mut err: *mut c_char = ptr::null_mut();
        let bad = CString::new("{}").unwrap();
        let core = unsafe { stow_core_new(bad.as_ptr(), &mut err) };
        assert!(core.is_null());
        assert!(!err.is_null());
        unsafe { stow_string_free(err) };
    }

    #[test]
    fn core_new_accepts_valid_config() {
        let mut err: *mut c_char = ptr::null_mut();
        let json = CString::new(r#"{"bucket":"b","region":"us-west-1"}"#).unwrap();
        let core = unsafe { stow_core_new(json.as_ptr(), &mut err) };
        assert!(!core.is_null());
        assert!(err.is_null());
        unsafe { stow_core_free(core) };
    }

    #[test]
    fn fetch_is_unimplemented_not_crash() {
        let mut err: *mut c_char = ptr::null_mut();
        let json = CString::new(r#"{"bucket":"b","region":"us-west-1"}"#).unwrap();
        let core = unsafe { stow_core_new(json.as_ptr(), &mut err) };
        let key = CString::new("obj/abc").unwrap();
        let out = CString::new("/tmp/out").unwrap();
        let token = stow_cancel_token_new();
        let rc = unsafe {
            stow_fetch_object(core, key.as_ptr(), out.as_ptr(), None, ptr::null_mut(), token, &mut err)
        };
        assert_eq!(rc, StowStatus::Unimplemented.code());
        assert!(!err.is_null());
        unsafe {
            stow_string_free(err);
            stow_cancel_token_free(token);
            stow_core_free(core);
        }
    }
}
