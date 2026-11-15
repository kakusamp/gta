//! d3d9.dll proxy mode.
//!
//! When the DLL file is named `d3d9.dll` and placed next to `gta_sa.exe`, the
//! Windows loader gives it to the game instead of the system d3d9.dll (the
//! application directory is searched before System32 for non-KnownDLLs).
//! SAPE then forwards every documented d3d9.dll export to the real DLL, which
//! is loaded lazily on the first call (never under the loader lock):
//!
//! * `[proxy] chain = <file>`: another d3d9 wrapper (ReShade, DXVK, ...) in
//!   the game folder, so several d3d9 mods can be stacked;
//! * otherwise `<System directory>\d3d9.dll`. For a 32-bit process on 64-bit
//!   Windows the WOW64 file-system redirector maps this to SysWOW64.
//!
//! Exported (all documented in the Windows SDK `d3d9.h`):
//! `Direct3DCreate9`, `Direct3DCreate9Ex`, `D3DPERF_BeginEvent`,
//! `D3DPERF_EndEvent`, `D3DPERF_SetMarker`, `D3DPERF_SetRegion`,
//! `D3DPERF_QueryRepeatFrame`, `D3DPERF_SetOptions`, `D3DPERF_GetStatus`.
//! Undocumented internal exports (PSGP*, Debug*, shader validator, shims)
//! are deliberately not provided: their signatures are not published, and
//! GTA SA, SA-MP and D3DX do not need them.

#![allow(non_snake_case)]

use crate::win32;
use core::ffi::c_void;
use std::sync::OnceLock;

/// Set from DllMain: file name of the DLL to chain to ("" = system d3d9.dll).
static CHAIN: OnceLock<String> = OnceLock::new();
static REAL: OnceLock<usize> = OnceLock::new();
static OUR_MODULE: OnceLock<usize> = OnceLock::new();

pub fn configure(our_module: win32::HMODULE, dir: &std::path::Path, chain: &str) {
    let _ = OUR_MODULE.set(our_module as usize);
    let path = if chain.is_empty() {
        String::new()
    } else {
        dir.join(chain).to_string_lossy().into_owned()
    };
    let _ = CHAIN.set(path);
}

fn system_d3d9() -> String {
    let mut buf = [0u16; 520];
    // SAFETY: buffer length passed correctly.
    let n = unsafe { win32::GetSystemDirectoryW(buf.as_mut_ptr(), buf.len() as u32) } as usize;
    let dir = if n > 0 && n < buf.len() {
        String::from_utf16_lossy(&buf[..n])
    } else {
        r"C:\Windows\System32".to_string()
    };
    format!(r"{dir}\d3d9.dll")
}

/// Loads (once) the real d3d9.dll and returns its module handle (0 on failure).
fn real_module() -> usize {
    *REAL.get_or_init(|| {
        let chain = CHAIN.get().cloned().unwrap_or_default();
        let mut candidates = Vec::new();
        if !chain.is_empty() {
            candidates.push(chain);
        }
        candidates.push(system_d3d9());
        for path in candidates {
            let w = win32::wide(&path);
            // SAFETY: valid null-terminated path. Called from game code, not DllMain.
            let h = unsafe { win32::LoadLibraryW(w.as_ptr()) } as usize;
            if h == 0 {
                crate::log_error!("proxy: could not load {path}");
                continue;
            }
            if Some(&h) == OUR_MODULE.get() {
                crate::log_error!("proxy: {path} is SAPE itself; skipping (check [proxy] chain)");
                continue;
            }
            crate::log_info!("proxy: forwarding d3d9 exports to {path} ({h:#x})");
            return h;
        }
        crate::log_error!("proxy: no real d3d9.dll available; Direct3D will fail");
        0
    })
}

fn real_proc(name: &[u8]) -> usize {
    let m = real_module();
    if m == 0 {
        return 0;
    }
    // SAFETY: `name` is null-terminated (callers pass b"...\0").
    unsafe { win32::GetProcAddress(m as win32::HMODULE, name.as_ptr()) as usize }
}

macro_rules! cached_proc {
    ($name:literal) => {{
        static P: OnceLock<usize> = OnceLock::new();
        *P.get_or_init(|| real_proc(concat!($name, "\0").as_bytes()))
    }};
}

const E_FAIL: i32 = 0x8000_4005u32 as i32;

/// # Safety
/// Called by the game as `d3d9.dll!Direct3DCreate9`.
#[no_mangle]
pub unsafe extern "system" fn Direct3DCreate9(sdk_version: u32) -> *mut c_void {
    let p = cached_proc!("Direct3DCreate9");
    if p == 0 {
        return core::ptr::null_mut();
    }
    let f: unsafe extern "system" fn(u32) -> *mut c_void = core::mem::transmute(p);
    let d3d = f(sdk_version);
    crate::log_info!("proxy: Direct3DCreate9(sdk {sdk_version:#x}) -> {d3d:p}");
    let hooks = crate::runtime().is_some_and(|rt| rt.config.needs_frame_hooks());
    if hooks && !d3d.is_null() {
        crate::d3d9::hook_d3d9_object(d3d);
    }
    d3d
}

/// Forwarded unchanged; IDirect3D9Ex devices are not hooked (GTA SA does not use them).
///
/// # Safety
/// Same contract as the real export.
#[no_mangle]
pub unsafe extern "system" fn Direct3DCreate9Ex(sdk_version: u32, out: *mut *mut c_void) -> i32 {
    let p = cached_proc!("Direct3DCreate9Ex");
    if p == 0 {
        return E_FAIL;
    }
    let f: unsafe extern "system" fn(u32, *mut *mut c_void) -> i32 = core::mem::transmute(p);
    f(sdk_version, out)
}

/// # Safety
/// Same contract as the real export.
#[no_mangle]
pub unsafe extern "system" fn D3DPERF_BeginEvent(col: u32, name: *const u16) -> i32 {
    match cached_proc!("D3DPERF_BeginEvent") {
        0 => -1,
        p => core::mem::transmute::<usize, unsafe extern "system" fn(u32, *const u16) -> i32>(p)(
            col, name,
        ),
    }
}

/// # Safety
/// Same contract as the real export.
#[no_mangle]
pub unsafe extern "system" fn D3DPERF_EndEvent() -> i32 {
    match cached_proc!("D3DPERF_EndEvent") {
        0 => -1,
        p => core::mem::transmute::<usize, unsafe extern "system" fn() -> i32>(p)(),
    }
}

/// # Safety
/// Same contract as the real export.
#[no_mangle]
pub unsafe extern "system" fn D3DPERF_SetMarker(col: u32, name: *const u16) {
    if let p @ 1.. = cached_proc!("D3DPERF_SetMarker") {
        core::mem::transmute::<usize, unsafe extern "system" fn(u32, *const u16)>(p)(col, name)
    }
}

/// # Safety
/// Same contract as the real export.
#[no_mangle]
pub unsafe extern "system" fn D3DPERF_SetRegion(col: u32, name: *const u16) {
    if let p @ 1.. = cached_proc!("D3DPERF_SetRegion") {
        core::mem::transmute::<usize, unsafe extern "system" fn(u32, *const u16)>(p)(col, name)
    }
}

/// # Safety
/// Same contract as the real export.
#[no_mangle]
pub unsafe extern "system" fn D3DPERF_QueryRepeatFrame() -> i32 {
    match cached_proc!("D3DPERF_QueryRepeatFrame") {
        0 => 0,
        p => core::mem::transmute::<usize, unsafe extern "system" fn() -> i32>(p)(),
    }
}

/// # Safety
/// Same contract as the real export.
#[no_mangle]
pub unsafe extern "system" fn D3DPERF_SetOptions(options: u32) {
    if let p @ 1.. = cached_proc!("D3DPERF_SetOptions") {
        core::mem::transmute::<usize, unsafe extern "system" fn(u32)>(p)(options)
    }
}

/// # Safety
/// Same contract as the real export.
#[no_mangle]
pub unsafe extern "system" fn D3DPERF_GetStatus() -> u32 {
    match cached_proc!("D3DPERF_GetStatus") {
        0 => 0,
        p => core::mem::transmute::<usize, unsafe extern "system" fn() -> u32>(p)(),
    }
}
