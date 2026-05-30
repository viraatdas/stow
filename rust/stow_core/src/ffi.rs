//! The C ABI surface linked into the `stow` CLI (and, later, the agent/extension).
//!
//! The engine functions each return a newly-allocated JSON C string the caller
//! frees with `stow_string_free`. On success the JSON is the result object; on
//! failure it is `{"error":"...","code":N}`. Every entry point is wrapped in
//! `catch_unwind` so a Rust panic can never unwind across the FFI boundary.

use std::ffi::{c_char, CStr, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr;

use serde::Serialize;

use crate::engine;
use crate::error::{StowError, StowResult};

/// Allocate a C string from a Rust string (caller frees via `stow_string_free`).
fn cstring(s: String) -> *mut c_char {
    CString::new(s)
        .unwrap_or_else(|_| CString::new("\u{0}").unwrap())
        .into_raw()
}

/// Borrow a `&str` from a C pointer, or error.
fn cstr<'a>(p: *const c_char, name: &'static str) -> StowResult<&'a str> {
    if p.is_null() {
        return Err(StowError::InvalidArg(format!("{name} is null")));
    }
    unsafe { CStr::from_ptr(p) }
        .to_str()
        .map_err(|_| StowError::InvalidArg(format!("{name} is not valid UTF-8")))
}

#[derive(Serialize)]
struct ErrJson<'a> {
    error: &'a str,
    code: i32,
}

/// Run `f`, serializing its `Ok` value to JSON, or an error object on failure /
/// panic. Always returns a heap JSON C string.
fn json_call<T, F>(f: F) -> *mut c_char
where
    T: Serialize,
    F: FnOnce() -> StowResult<T>,
{
    let result = catch_unwind(AssertUnwindSafe(f));
    match result {
        Ok(Ok(value)) => match serde_json::to_string(&value) {
            Ok(s) => cstring(s),
            Err(e) => cstring(
                serde_json::to_string(&ErrJson { error: &e.to_string(), code: 1 })
                    .unwrap_or_else(|_| "{\"error\":\"serialize\",\"code\":1}".into()),
            ),
        },
        Ok(Err(e)) => cstring(
            serde_json::to_string(&ErrJson { error: &e.to_string(), code: e.status().code() })
                .unwrap_or_else(|_| "{\"error\":\"unknown\",\"code\":1}".into()),
        ),
        Err(_) => cstring("{\"error\":\"panic in stow_core\",\"code\":2}".into()),
    }
}

// ---- version / string lifecycle ---------------------------------------------

/// Library version as a newly-allocated C string. Free with `stow_string_free`.
#[no_mangle]
pub extern "C" fn stow_core_version() -> *mut c_char {
    match CString::new(env!("CARGO_PKG_VERSION")) {
        Ok(c) => c.into_raw(),
        Err(_) => ptr::null_mut(),
    }
}

/// Free a string returned by this library.
///
/// # Safety
/// `s` must be a pointer returned by a `stow_*` function and not freed before.
#[no_mangle]
pub unsafe extern "C" fn stow_string_free(s: *mut c_char) {
    if !s.is_null() {
        drop(CString::from_raw(s));
    }
}

// ---- engine -----------------------------------------------------------------

/// `stow init` — detect AWS creds, auto-create the bucket, persist config.
/// `region` may be null to auto-detect. Returns InitResult JSON.
///
/// # Safety
/// `region` must be null or a valid C string.
#[no_mangle]
pub unsafe extern "C" fn stow_engine_init(region: *const c_char) -> *mut c_char {
    json_call(|| {
        let region_arg = if region.is_null() {
            None
        } else {
            Some(cstr(region, "region")?.to_string())
        };
        engine::init(region_arg)
    })
}

/// `stow add/offload <path>` — upload to S3 and replace with a placeholder.
///
/// # Safety
/// `path` must be a valid C string.
#[no_mangle]
pub unsafe extern "C" fn stow_engine_offload(path: *const c_char) -> *mut c_char {
    json_call(|| {
        let p = cstr(path, "path")?;
        engine::offload(p)
    })
}

/// `stow restore <path>` — download from S3 and rewrite byte-identically.
///
/// # Safety
/// `path` must be a valid C string.
#[no_mangle]
pub unsafe extern "C" fn stow_engine_restore(path: *const c_char) -> *mut c_char {
    json_call(|| {
        let p = cstr(path, "path")?;
        engine::restore(p)
    })
}

/// `stow status` — list offloaded files and space saved. Returns StatusResult JSON.
#[no_mangle]
pub extern "C" fn stow_engine_status() -> *mut c_char {
    json_call(engine::status)
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
    fn offload_without_init_errors_cleanly() {
        // No config -> JSON error, never a panic/crash.
        let path = CString::new("/nonexistent/file").unwrap();
        let r = unsafe { stow_engine_offload(path.as_ptr()) };
        let s = unsafe { CStr::from_ptr(r) }.to_str().unwrap().to_owned();
        assert!(s.contains("\"error\""));
        unsafe { stow_string_free(r) };
    }
}
