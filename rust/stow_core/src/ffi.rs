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

/// `stow scan` — dry run: list auto-offload candidates. Returns ScanResult JSON.
#[no_mangle]
pub extern "C" fn stow_engine_scan() -> *mut c_char {
    json_call(engine::scan)
}

/// `stow auto` — apply the policy: offload all candidates. Returns AutoResult JSON.
#[no_mangle]
pub extern "C" fn stow_engine_auto() -> *mut c_char {
    json_call(engine::auto_offload)
}

/// `stow clean` — reclaim regenerable tool/package caches idle >= min_idle_days.
/// When `apply` is false it's a dry run (lists what it would delete). Returns
/// CleanReport JSON.
#[no_mangle]
pub extern "C" fn stow_engine_clean_caches(min_idle_days: u64, apply: bool) -> *mut c_char {
    json_call(|| crate::cache::clean(min_idle_days, apply))
}

/// Return the full persisted config (bucket/region/policy) as JSON.
#[no_mangle]
pub extern "C" fn stow_engine_get_config() -> *mut c_char {
    json_call(engine::get_config)
}

/// `stow migrate` — convert legacy stub offloads to transparent symlinks.
#[no_mangle]
pub extern "C" fn stow_engine_migrate() -> *mut c_char {
    json_call(engine::migrate)
}

/// `stow share <path>` — publish a permanent public link (folders are zipped).
///
/// # Safety
/// `path` must be a valid C string.
#[no_mangle]
pub unsafe extern "C" fn stow_engine_share(path: *const c_char) -> *mut c_char {
    json_call(|| {
        let p = cstr(path, "path")?;
        engine::share(p)
    })
}

/// `stow unshare <token>` — revoke a share link (deletes the public object).
///
/// # Safety
/// `token` must be a valid C string.
#[no_mangle]
pub unsafe extern "C" fn stow_engine_unshare(token: *const c_char) -> *mut c_char {
    json_call(|| {
        let t = cstr(token, "token")?;
        engine::unshare(t)
    })
}

/// `stow shares` — list active share links. Returns {"shares":[...]} JSON.
#[no_mangle]
pub extern "C" fn stow_engine_list_shares() -> *mut c_char {
    json_call(engine::list_shares)
}

/// Replace the policy block from a JSON object. Returns the updated config JSON.
///
/// # Safety
/// `policy_json` must be a valid C string.
#[no_mangle]
pub unsafe extern "C" fn stow_engine_set_policy(policy_json: *const c_char) -> *mut c_char {
    json_call(|| {
        let j = cstr(policy_json, "policy_json")?;
        engine::set_policy(j)
    })
}

// ---- File Provider FFI (called by the extension) ----------------------------

/// Enumerate children of a container. Returns JSON array of items.
///
/// # Safety
/// `parent_id` must be a valid C string.
#[no_mangle]
pub unsafe extern "C" fn stow_fp_enumerate(parent_id: *const c_char) -> *mut c_char {
    json_call(|| {
        let p = cstr(parent_id, "parent_id")?;
        let store = crate::provider::Store::open()?;
        // The working set wants every item, not one container's children.
        if p == crate::provider::ALL_ITEMS {
            store.all_items()
        } else {
            store.children(p)
        }
    })
}

/// Look up a single item by id. Returns item JSON, or error if not found.
///
/// # Safety
/// `item_id` must be a valid C string.
#[no_mangle]
pub unsafe extern "C" fn stow_fp_item(item_id: *const c_char) -> *mut c_char {
    json_call(|| {
        let id = cstr(item_id, "item_id")?;
        crate::provider::Store::open()?
            .get(id)?
            .ok_or_else(|| crate::error::StowError::NotFound(id.to_string()))
    })
}

/// Create an item (file or folder). For files, uploads `temp_path` to S3.
///
/// # Safety
/// All non-null pointer args must be valid C strings.
#[no_mangle]
pub unsafe extern "C" fn stow_fp_create(
    parent_id: *const c_char,
    filename: *const c_char,
    is_folder: bool,
    temp_path: *const c_char,
) -> *mut c_char {
    json_call(|| {
        let parent = cstr(parent_id, "parent_id")?;
        let name = cstr(filename, "filename")?;
        let tmp = if temp_path.is_null() { "" } else { cstr(temp_path, "temp_path")? };
        crate::provider::create(parent, name, is_folder, tmp)
    })
}

/// Replace an item's contents from `temp_path` (uploads to S3).
///
/// # Safety
/// All non-null pointer args must be valid C strings.
#[no_mangle]
pub unsafe extern "C" fn stow_fp_modify(
    item_id: *const c_char,
    temp_path: *const c_char,
) -> *mut c_char {
    json_call(|| {
        let id = cstr(item_id, "item_id")?;
        let tmp = cstr(temp_path, "temp_path")?;
        crate::provider::update_contents(id, tmp)
    })
}

/// Download an item from S3 to `out_path` (rehydrate on open).
///
/// # Safety
/// All non-null pointer args must be valid C strings.
#[no_mangle]
pub unsafe extern "C" fn stow_fp_fetch(
    item_id: *const c_char,
    out_path: *const c_char,
) -> *mut c_char {
    json_call(|| {
        let id = cstr(item_id, "item_id")?;
        let out = cstr(out_path, "out_path")?;
        crate::provider::fetch(id, out)
    })
}

/// Delete an item (metadata only; S3 object is left for dedup safety).
///
/// # Safety
/// `item_id` must be a valid C string.
#[no_mangle]
pub unsafe extern "C" fn stow_fp_delete(item_id: *const c_char) -> *mut c_char {
    json_call(|| {
        let id = cstr(item_id, "item_id")?;
        crate::provider::Store::open()?.delete(id)?;
        Ok(true)
    })
}

/// Current sync anchor: a fingerprint of the provider DB. Returns
/// {"anchor":"..."} JSON. fileproviderd compares anchors to decide whether to
/// re-enumerate after `signalEnumerator`.
#[no_mangle]
pub extern "C" fn stow_fp_anchor() -> *mut c_char {
    json_call(|| {
        #[derive(Serialize)]
        struct A {
            anchor: String,
        }
        Ok(A { anchor: crate::provider::Store::open()?.anchor()? })
    })
}

/// Mark an item dataless (true) or materialized (false). The agent calls this
/// after a successful `evictItem` so the shared DB stays in sync with on-disk
/// state — `stow status` reads the `dataless` flag to list offloaded folder files.
///
/// # Safety
/// `item_id` must be a valid C string.
#[no_mangle]
pub unsafe extern "C" fn stow_fp_set_dataless(item_id: *const c_char, dataless: bool) -> *mut c_char {
    json_call(|| {
        let id = cstr(item_id, "item_id")?;
        crate::provider::Store::open()?.set_dataless(id, dataless)?;
        Ok(true)
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
    fn offload_without_init_errors_cleanly() {
        // No config -> JSON error, never a panic/crash.
        let path = CString::new("/nonexistent/file").unwrap();
        let r = unsafe { stow_engine_offload(path.as_ptr()) };
        let s = unsafe { CStr::from_ptr(r) }.to_str().unwrap().to_owned();
        assert!(s.contains("\"error\""));
        unsafe { stow_string_free(r) };
    }
}
