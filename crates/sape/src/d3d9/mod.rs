//! d3d9_hook: Direct3D 9 interception.
//!
//! Attach strategy (in order):
//! 1. **IAT hook** of `d3d9.dll!Direct3DCreate9` in the game executable, installed
//!    from DllMain (memory patch only, no calls into d3d9). When the game creates
//!    its IDirect3D9 we hook `IDirect3D9::CreateDevice`, and from the created
//!    device we patch the device vtable.
//! 2. **Late attach**: if the device already exists when SAPE loads (e.g. injected
//!    later), read it from RenderWare's device pointer (1.0 US only, validated)
//!    and patch its vtable directly.
//!
//! Vtable patching modifies the vtable shared by all objects of that class in
//! the process; there is normally exactly one device. If a second device class
//! (different vtable) shows up it is logged and left alone.

pub mod hooks;
pub mod types;

use crate::hook::{find_iat_slot, patch_slot, validate_vtable};
use crate::render_optimizer as ro;
use core::ffi::c_void;
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering::*};
use types::*;

pub static ORIG_DIRECT3D_CREATE9: AtomicUsize = AtomicUsize::new(0);
pub static ORIG_CREATE_DEVICE: AtomicUsize = AtomicUsize::new(0);
static D3D9_VTABLE: AtomicUsize = AtomicUsize::new(0);
pub static DEVICE_VTABLE: AtomicUsize = AtomicUsize::new(0);
pub static BEHAVIOR_FLAGS: AtomicU32 = AtomicU32::new(0);

/// Installs the IAT hook on the main executable. Returns false if the executable
/// does not import Direct3DCreate9 (then only late attach is possible).
pub fn install_iat_hook() -> bool {
    let exe = crate::win32::module_handle_null() as usize;
    // SAFETY: `exe` is the base of the mapped main image.
    let slot = unsafe { find_iat_slot(exe, "d3d9.dll", "Direct3DCreate9") };
    let Some(slot) = slot else {
        crate::log_warn!("d3d9.dll!Direct3DCreate9 not found in the executable's import table");
        return false;
    };
    // SAFETY: IAT slot of Direct3DCreate9; hook has the identical signature.
    match unsafe {
        patch_slot(
            slot,
            hk_direct3d_create9 as *const () as usize,
            &ORIG_DIRECT3D_CREATE9,
        )
    } {
        Ok(()) => {
            crate::log_info!(
                "IAT hook: Direct3DCreate9 slot {:#x} -> original {:#x}",
                slot as usize,
                ORIG_DIRECT3D_CREATE9.load(Relaxed)
            );
            true
        }
        Err(e) => {
            crate::log_error!("IAT hook failed: {e:?}");
            false
        }
    }
}

unsafe extern "system" fn hk_direct3d_create9(sdk: u32) -> *mut c_void {
    let orig: Direct3DCreate9Fn = core::mem::transmute(ORIG_DIRECT3D_CREATE9.load(Acquire));
    let d3d = orig(sdk);
    crate::log_info!("Direct3DCreate9(sdk {sdk:#x}) -> {d3d:p}");
    if !d3d.is_null() {
        hook_d3d9_object(d3d);
    }
    d3d
}

/// Hooks `IDirect3D9::CreateDevice` on a freshly created IDirect3D9.
///
/// # Safety
/// `d3d` must be a valid IDirect3D9 object.
pub unsafe fn hook_d3d9_object(d3d: *mut c_void) {
    let vt = *(d3d as *const usize);
    if !validate_vtable(vt, d3d9_vt::COUNT) {
        crate::log_error!("IDirect3D9 vtable {vt:#x} failed validation; not hooking");
        return;
    }
    match D3D9_VTABLE.compare_exchange(0, vt, AcqRel, Acquire) {
        Ok(_) => {}
        Err(prev) if prev == vt => return, // already hooked
        Err(prev) => {
            crate::log_warn!(
                "second IDirect3D9 class (vtable {vt:#x}, first {prev:#x}) not hooked"
            );
            return;
        }
    }
    let slot = (vt as *mut usize).add(d3d9_vt::CREATE_DEVICE);
    match patch_slot(
        slot,
        hk_create_device as *const () as usize,
        &ORIG_CREATE_DEVICE,
    ) {
        Ok(()) => crate::log_info!("hooked IDirect3D9::CreateDevice (vtable {vt:#x})"),
        Err(e) => crate::log_error!("hooking CreateDevice failed: {e:?}"),
    }
}

unsafe extern "system" fn hk_create_device(
    this: *mut c_void,
    adapter: u32,
    dev_type: u32,
    focus: *mut c_void,
    behavior: u32,
    pp: *mut D3DPRESENT_PARAMETERS,
    out: *mut *mut Device,
) -> HRESULT {
    let orig: CreateDeviceFn = core::mem::transmute(ORIG_CREATE_DEVICE.load(Acquire));
    let hr = orig(this, adapter, dev_type, focus, behavior, pp, out);
    let params = if pp.is_null() {
        "null".into()
    } else {
        (*pp).describe()
    };
    crate::log_info!(
        "CreateDevice(adapter {adapter}, type {dev_type}, behavior {behavior:#x}{}{}) -> {hr:#x}; {params}",
        if behavior & D3DCREATE_PUREDEVICE != 0 { " PURE" } else { "" },
        if behavior & D3DCREATE_MULTITHREADED != 0 { " MULTITHREADED" } else { "" },
    );
    if hr >= 0 && !out.is_null() && !(*out).is_null() {
        BEHAVIOR_FLAGS.store(behavior, Relaxed);
        ro::set_owner_thread(crate::win32::current_thread_id());
        install_device_hooks(*out, "CreateDevice");
    }
    hr
}

/// Late attach through RenderWare's device pointer (only on a verified exe version).
pub fn late_attach(device_ptr_addr: usize) -> bool {
    let Some(dev) = crate::win32::read_usize_checked(device_ptr_addr) else {
        return false;
    };
    if dev == 0 {
        return false;
    }
    crate::log_info!("late attach: device {dev:#x} found at {device_ptr_addr:#x}");
    // Owner thread is unknown here; it is taken from the first Present call.
    // SAFETY: pointer validated as readable; install_device_hooks validates the vtable.
    unsafe { install_device_hooks(dev as *mut Device, "late attach") }
}

/// Patches the device vtable according to the configuration.
///
/// # Safety
/// `dev` must be a live IDirect3DDevice9.
/// One device-vtable hook: (slot index, hook fn, original-pointer cell, name, is state-tracking).
#[derive(Clone, Copy)]
struct HookSpec {
    index: usize,
    hook: usize,
    orig: &'static AtomicUsize,
    name: &'static str,
    state: bool,
}

/// The device hooks wanted by the configuration.
fn hook_specs(cfg: &sape_core::config::Config) -> Vec<HookSpec> {
    let full = cfg.monitor.level >= sape_core::config::MonitorLevel::Full;
    let filter = cfg.state_filter.enabled;
    let res = cfg.resources.track;
    use hooks::*;
    let all: [(bool, usize, usize, &'static AtomicUsize, &'static str); 21] = [
        (
            cfg.needs_frame_hooks(),
            dev_vt::PRESENT,
            hk_present as *const () as usize,
            &ORIG_PRESENT,
            "Present",
        ),
        (
            cfg.needs_frame_hooks(),
            dev_vt::RESET,
            hk_reset as *const () as usize,
            &ORIG_RESET,
            "Reset",
        ),
        (
            full || filter,
            dev_vt::SET_RENDER_STATE,
            hk_set_render_state as *const () as usize,
            &ORIG_SET_RENDER_STATE,
            "SetRenderState",
        ),
        (
            full || filter,
            dev_vt::SET_SAMPLER_STATE,
            hk_set_sampler_state as *const () as usize,
            &ORIG_SET_SAMPLER_STATE,
            "SetSamplerState",
        ),
        (
            full || filter,
            dev_vt::SET_TEXTURE_STAGE_STATE,
            hk_set_texture_stage_state as *const () as usize,
            &ORIG_SET_TSS,
            "SetTextureStageState",
        ),
        (
            full || filter,
            dev_vt::BEGIN_STATE_BLOCK,
            hk_begin_state_block as *const () as usize,
            &ORIG_BEGIN_SB,
            "BeginStateBlock",
        ),
        (
            full || filter,
            dev_vt::END_STATE_BLOCK,
            hk_end_state_block as *const () as usize,
            &ORIG_END_SB,
            "EndStateBlock",
        ),
        (
            full || filter,
            dev_vt::CREATE_STATE_BLOCK,
            hk_create_state_block as *const () as usize,
            &ORIG_CREATE_SB,
            "CreateStateBlock",
        ),
        (
            full,
            dev_vt::SET_TEXTURE,
            hk_set_texture as *const () as usize,
            &ORIG_SET_TEXTURE,
            "SetTexture",
        ),
        (
            full,
            dev_vt::DRAW_PRIMITIVE,
            hk_draw_primitive as *const () as usize,
            &ORIG_DP,
            "DrawPrimitive",
        ),
        (
            full,
            dev_vt::DRAW_INDEXED_PRIMITIVE,
            hk_draw_indexed_primitive as *const () as usize,
            &ORIG_DIP,
            "DrawIndexedPrimitive",
        ),
        (
            full,
            dev_vt::DRAW_PRIMITIVE_UP,
            hk_draw_primitive_up as *const () as usize,
            &ORIG_DPUP,
            "DrawPrimitiveUP",
        ),
        (
            full,
            dev_vt::DRAW_INDEXED_PRIMITIVE_UP,
            hk_draw_indexed_primitive_up as *const () as usize,
            &ORIG_DIPUP,
            "DrawIndexedPrimitiveUP",
        ),
        (
            full,
            dev_vt::SET_STREAM_SOURCE,
            hk_set_stream_source as *const () as usize,
            &ORIG_SET_STREAM_SOURCE,
            "SetStreamSource",
        ),
        (
            full,
            dev_vt::SET_VERTEX_SHADER,
            hk_set_vertex_shader as *const () as usize,
            &ORIG_SET_VS,
            "SetVertexShader",
        ),
        (
            full,
            dev_vt::SET_PIXEL_SHADER,
            hk_set_pixel_shader as *const () as usize,
            &ORIG_SET_PS,
            "SetPixelShader",
        ),
        (
            full,
            dev_vt::SET_FVF,
            hk_set_fvf as *const () as usize,
            &ORIG_SET_FVF,
            "SetFVF",
        ),
        (
            full,
            dev_vt::SET_VERTEX_DECLARATION,
            hk_set_vertex_declaration as *const () as usize,
            &ORIG_SET_DECL,
            "SetVertexDeclaration",
        ),
        (
            full || res,
            dev_vt::CREATE_TEXTURE,
            hk_create_texture as *const () as usize,
            &ORIG_CREATE_TEXTURE,
            "CreateTexture",
        ),
        (
            full || res,
            dev_vt::CREATE_VERTEX_BUFFER,
            hk_create_vertex_buffer as *const () as usize,
            &ORIG_CREATE_VB,
            "CreateVertexBuffer",
        ),
        (
            full || res,
            dev_vt::CREATE_INDEX_BUFFER,
            hk_create_index_buffer as *const () as usize,
            &ORIG_CREATE_IB,
            "CreateIndexBuffer",
        ),
    ];
    let state_slots = [
        dev_vt::SET_RENDER_STATE,
        dev_vt::SET_SAMPLER_STATE,
        dev_vt::SET_TEXTURE_STAGE_STATE,
        dev_vt::BEGIN_STATE_BLOCK,
        dev_vt::END_STATE_BLOCK,
        dev_vt::CREATE_STATE_BLOCK,
        dev_vt::RESET,
    ];
    all.iter()
        .filter(|e| e.0)
        .map(|&(_, index, hook, orig, name)| HookSpec {
            index,
            hook,
            orig,
            name,
            state: filter && state_slots.contains(&index),
        })
        .collect()
}

/// The hooked device (for the watchdog and the swap-chain Present fallback).
pub static DEVICE: AtomicUsize = AtomicUsize::new(0);
static REHOOKS: AtomicU32 = AtomicU32::new(0);
const MAX_REHOOKS: u32 = 20;

/// Patches the device vtable according to the configuration.
///
/// # Safety
/// `dev` must be a live IDirect3DDevice9.
pub unsafe fn install_device_hooks(dev: *mut Device, origin: &str) -> bool {
    let Some(rt) = crate::runtime() else {
        return false;
    };
    let cfg = &rt.config;
    let Some(vt) = crate::win32::read_usize_checked(dev as usize) else {
        return false;
    };
    log_vtable_owner(dev, vt);
    if !validate_vtable(vt, dev_vt::COUNT) {
        crate::log_error!("device vtable {vt:#x} failed validation ({origin}); not hooking");
        return false;
    }
    match DEVICE_VTABLE.compare_exchange(0, vt, AcqRel, Acquire) {
        Ok(_) => {}
        Err(prev) if prev == vt => {
            crate::log_info!("device vtable {vt:#x} already hooked ({origin})");
            return true;
        }
        Err(prev) => {
            crate::log_warn!(
                "new device class (vtable {vt:#x}, hooked {prev:#x}) - not hooking ({origin})"
            );
            return false;
        }
    }
    DEVICE.store(dev as usize, Release);

    let filter = cfg.state_filter.enabled;
    let mut installed = Vec::new();
    let mut failed = Vec::new();
    for h in hook_specs(cfg) {
        match patch_slot((vt as *mut usize).add(h.index), h.hook, h.orig) {
            Ok(()) => installed.push(h.name),
            Err(e) => failed.push(format!("{}: {e:?}", h.name)),
        }
    }
    crate::log_info!(
        "device hooks ({origin}, vtable {vt:#x}): {}",
        installed.join(", ")
    );
    // State hooks must be all-or-nothing: a filter missing its invalidation
    // hooks (Reset / state blocks) could skip calls it must not skip.
    if !failed.is_empty() {
        crate::log_error!("device hooks failed: {}", failed.join("; "));
        if filter {
            ro::lock_off(format_args!(
                "not all state-tracking hooks could be installed"
            ));
        }
    }
    if filter && BEHAVIOR_FLAGS.load(Relaxed) & D3DCREATE_PUREDEVICE != 0 {
        crate::log_info!("device is PURE: validation mode unavailable");
    }
    if cfg.needs_frame_hooks() {
        hook_swap_chain(dev);
        start_watchdog();
    }
    true
}

fn log_vtable_owner(dev: *mut Device, vt: usize) {
    let present =
        crate::win32::read_usize_checked(vt + dev_vt::PRESENT * core::mem::size_of::<usize>())
            .unwrap_or(0);
    crate::log_info!(
        "device {:p}: vtable {vt:#x} in {}, Present -> {}",
        dev,
        crate::win32::describe_address(vt),
        crate::win32::describe_address(present)
    );
}

/// Hooks `IDirect3DSwapChain9::Present` of the implicit swap chain. Frames
/// presented this way are counted, and used as the frame source if the
/// device's own `Present` is never called.
unsafe fn hook_swap_chain(dev: *mut Device) {
    let vt = *(dev as *const *const usize);
    let get: GetSwapChainFn = core::mem::transmute(*vt.add(dev_vt::GET_SWAP_CHAIN));
    let mut sc: *mut c_void = core::ptr::null_mut();
    if get(dev, 0, &mut sc) < 0 || sc.is_null() {
        crate::log_warn!("GetSwapChain(0) failed; swap-chain Present not tracked");
        return;
    }
    let sc_vt = *(sc as *const usize);
    if validate_vtable(sc_vt, sc_vt::COUNT) {
        match patch_slot(
            (sc_vt as *mut usize).add(sc_vt::PRESENT),
            hooks::hk_swap_chain_present as *const () as usize,
            &hooks::ORIG_SC_PRESENT,
        ) {
            Ok(()) => crate::log_info!(
                "hooked IDirect3DSwapChain9::Present (vtable {sc_vt:#x} in {})",
                crate::win32::describe_address(sc_vt)
            ),
            Err(e) => crate::log_warn!("swap-chain Present hook failed: {e:?}"),
        }
    }
    let release: ReleaseFn = core::mem::transmute(*(sc_vt as *const usize).add(unk_vt::RELEASE));
    release(sc);
}

fn start_watchdog() {
    static STARTED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
    if STARTED.swap(true, AcqRel) {
        return;
    }
    let r = std::thread::Builder::new()
        .name("sape-watchdog".into())
        .spawn(watchdog);
    if let Err(e) = r {
        crate::log_warn!("watchdog thread not started: {e}");
    }
}

/// Once per second: makes sure our hooks are still in the table the device
/// actually uses. Some wrapper mods re-copy the device vtable (e.g. after a
/// Reset), which silently drops every hook installed in the previous copy.
fn watchdog() {
    let Some(rt) = crate::runtime() else { return };
    let specs = hook_specs(&rt.config);
    let mut seconds = 0u32;
    loop {
        std::thread::sleep(std::time::Duration::from_secs(1));
        seconds += 1;
        let dev = DEVICE.load(Acquire);
        let Some(vt) = crate::win32::read_usize_checked(dev) else {
            continue;
        };
        let known = DEVICE_VTABLE.load(Acquire);
        let missing: Vec<&HookSpec> = specs
            .iter()
            .filter(|h| {
                crate::win32::read_usize_checked(vt + h.index * core::mem::size_of::<usize>())
                    != Some(h.hook)
            })
            .collect();
        if !missing.is_empty() && REHOOKS.load(Relaxed) < MAX_REHOOKS {
            let names: Vec<&str> = missing.iter().map(|h| h.name).collect();
            if vt != known {
                crate::log_warn!(
                    "watchdog: device vtable changed {known:#x} -> {vt:#x} ({}); re-hooking: {}",
                    crate::win32::describe_address(vt),
                    names.join(", ")
                );
            } else {
                crate::log_warn!(
                    "watchdog: hooks removed from vtable {vt:#x}; re-hooking: {}",
                    names.join(", ")
                );
            }
            // SAFETY: `vt` is the table the live device uses; validated before patching.
            unsafe { rehook(vt, &specs, &missing) };
            if REHOOKS.load(Relaxed) >= MAX_REHOOKS {
                crate::log_error!(
                    "watchdog: {MAX_REHOOKS} re-hooks - another mod keeps removing SAPE's hooks; giving up to avoid a fight"
                );
            }
        }
        if matches!(seconds, 5 | 15 | 30 | 60) || (seconds % 300 == 0) {
            crate::log_info!(
                "watchdog {seconds}s: device Present calls {}, swap-chain Present calls {}, vtable {vt:#x}, re-hooks {}",
                hooks::DEVICE_PRESENTS.load(Relaxed),
                hooks::SC_PRESENTS.load(Relaxed),
                REHOOKS.load(Relaxed)
            );
        }
    }
}

unsafe fn rehook(vt: usize, specs: &[HookSpec], missing: &[&HookSpec]) {
    REHOOKS.fetch_add(1, Relaxed);
    if !validate_vtable(vt, dev_vt::COUNT) {
        crate::log_error!("watchdog: new vtable {vt:#x} failed validation; not re-hooking");
        return;
    }
    // If every state hook vanished at once (a fresh copy of the table), no call
    // was filtered meanwhile and an invalidation makes the shadow sound again.
    // If only some vanished, a filtered call may have missed an invalidation:
    // lock the filter off for the session.
    let state_total = specs.iter().filter(|h| h.state).count();
    let state_missing = missing.iter().filter(|h| h.state).count();
    if state_missing > 0 && state_missing < state_total {
        ro::lock_off(format_args!(
            "another mod removed only some state-tracking hooks"
        ));
    }
    ro::invalidate_any_thread();
    for h in missing {
        if let Err(e) = patch_slot((vt as *mut usize).add(h.index), h.hook, h.orig) {
            crate::log_error!("watchdog: re-hooking {} failed: {e:?}", h.name);
            if h.state {
                ro::lock_off(format_args!(
                    "state hook {} could not be re-installed",
                    h.name
                ));
            }
        }
    }
    DEVICE_VTABLE.store(vt, Release);
}
