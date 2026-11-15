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
pub unsafe fn install_device_hooks(dev: *mut Device, origin: &str) -> bool {
    let Some(rt) = crate::runtime() else {
        return false;
    };
    let cfg = &rt.config;
    let Some(vt) = crate::win32::read_usize_checked(dev as usize) else {
        return false;
    };
    // Who owns the vtable and its Present? Tells us which wrapper sits below us.
    let present =
        crate::win32::read_usize_checked(vt + dev_vt::PRESENT * core::mem::size_of::<usize>())
            .unwrap_or(0);
    crate::log_info!(
        "device {:p}: vtable {vt:#x} in {}, Present -> {}",
        dev,
        crate::win32::describe_address(vt),
        crate::win32::describe_address(present)
    );
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

    let full = cfg.monitor.level >= sape_core::config::MonitorLevel::Full;
    let filter = cfg.state_filter.enabled;
    let res = cfg.resources.track;
    let pure = BEHAVIOR_FLAGS.load(Relaxed) & D3DCREATE_PUREDEVICE != 0;

    use hooks::*;
    let table: &[(bool, usize, usize, &AtomicUsize, &str)] = &[
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

    // State hooks must be all-or-nothing: a filter missing its invalidation
    // hooks (Reset / state blocks) could skip calls it must not skip.
    let mut installed = Vec::new();
    let mut failed = Vec::new();
    for &(wanted, index, hook, orig, name) in table {
        if !wanted {
            continue;
        }
        match patch_slot((vt as *mut usize).add(index), hook, orig) {
            Ok(()) => installed.push(name),
            Err(e) => failed.push(format!("{name}: {e:?}")),
        }
    }
    crate::log_info!(
        "device hooks ({origin}, vtable {vt:#x}): {}",
        installed.join(", ")
    );
    if !failed.is_empty() {
        crate::log_error!("device hooks failed: {}", failed.join("; "));
        if filter {
            ro::lock_off(format_args!(
                "not all state-tracking hooks could be installed"
            ));
        }
    }
    if filter && pure {
        crate::log_info!("device is PURE: validation mode unavailable");
    }
    true
}
