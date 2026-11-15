//! IDirect3DDevice9 / IDirect3DStateBlock9 hook functions.
//!
//! Rules for every hook in this file:
//! * Signature identical to the COM method (this first, `extern "system"`).
//! * Never panic, never allocate, never log on the per-call path.
//! * Never hold a reference to shared state across the call to the original
//!   (the original may re-enter our hooks via another mod's wrapper).

#![allow(clippy::missing_safety_doc)] // COM method hooks: called only by D3D9 through the vtable

use super::types::*;
use crate::monitor::{self, bump, C};
use crate::render_optimizer::{self as ro, StageKind};
use crate::{hook, resources};
use core::ffi::c_void;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering::*};
use std::time::Instant;

pub static ORIG_PRESENT: AtomicUsize = AtomicUsize::new(0);
pub static ORIG_SC_PRESENT: AtomicUsize = AtomicUsize::new(0);
/// Frames seen through IDirect3DDevice9::Present / IDirect3DSwapChain9::Present.
pub static DEVICE_PRESENTS: AtomicU64 = AtomicU64::new(0);
pub static SC_PRESENTS: AtomicU64 = AtomicU64::new(0);
pub static ORIG_RESET: AtomicUsize = AtomicUsize::new(0);
pub static ORIG_SET_RENDER_STATE: AtomicUsize = AtomicUsize::new(0);
pub static ORIG_SET_SAMPLER_STATE: AtomicUsize = AtomicUsize::new(0);
pub static ORIG_SET_TSS: AtomicUsize = AtomicUsize::new(0);
pub static ORIG_BEGIN_SB: AtomicUsize = AtomicUsize::new(0);
pub static ORIG_END_SB: AtomicUsize = AtomicUsize::new(0);
pub static ORIG_CREATE_SB: AtomicUsize = AtomicUsize::new(0);
pub static ORIG_SET_TEXTURE: AtomicUsize = AtomicUsize::new(0);
pub static ORIG_DP: AtomicUsize = AtomicUsize::new(0);
pub static ORIG_DIP: AtomicUsize = AtomicUsize::new(0);
pub static ORIG_DPUP: AtomicUsize = AtomicUsize::new(0);
pub static ORIG_DIPUP: AtomicUsize = AtomicUsize::new(0);
pub static ORIG_SET_STREAM_SOURCE: AtomicUsize = AtomicUsize::new(0);
pub static ORIG_SET_VS: AtomicUsize = AtomicUsize::new(0);
pub static ORIG_SET_PS: AtomicUsize = AtomicUsize::new(0);
pub static ORIG_SET_FVF: AtomicUsize = AtomicUsize::new(0);
pub static ORIG_SET_DECL: AtomicUsize = AtomicUsize::new(0);
pub static ORIG_CREATE_TEXTURE: AtomicUsize = AtomicUsize::new(0);
pub static ORIG_CREATE_VB: AtomicUsize = AtomicUsize::new(0);
pub static ORIG_CREATE_IB: AtomicUsize = AtomicUsize::new(0);

static SB_VTABLE: AtomicUsize = AtomicUsize::new(0);
static ORIG_SB_APPLY: AtomicUsize = AtomicUsize::new(0);

macro_rules! orig {
    ($slot:ident as $ty:ty) => {
        core::mem::transmute::<usize, $ty>($slot.load(Acquire))
    };
}

// --- frame ------------------------------------------------------------------

pub unsafe extern "system" fn hk_present(
    dev: *mut Device,
    src: *const c_void,
    dst: *const c_void,
    wnd: *mut c_void,
    dirty: *const c_void,
) -> HRESULT {
    let orig = orig!(ORIG_PRESENT as PresentFn);
    DEVICE_PRESENTS.fetch_add(1, Relaxed);
    frame(dev);
    orig(dev, src, dst, wnd, dirty)
}

unsafe fn frame(dev: *mut Device) {
    if let Some(rt) = crate::runtime() {
        // Late attach: the first Present tells us which thread renders.
        ro::set_owner_thread(crate::win32::current_thread_id());
        if ro::OWNER_THREAD.load(Relaxed) == crate::win32::current_thread_id() {
            crate::samp::poll_once();
            monitor::on_present(dev, &rt.config);
        }
    }
}

/// Frames presented through the swap chain are counted; they drive the monitor
/// only if the device's own Present has never been seen (no double counting).
pub unsafe extern "system" fn hk_swap_chain_present(
    sc: *mut c_void,
    src: *const c_void,
    dst: *const c_void,
    wnd: *mut c_void,
    dirty: *const c_void,
    flags: u32,
) -> HRESULT {
    let orig = orig!(ORIG_SC_PRESENT as SwapChainPresentFn);
    if SC_PRESENTS.fetch_add(1, Relaxed) == 0 {
        crate::log_info!("first swap-chain Present observed");
    }
    if DEVICE_PRESENTS.load(Relaxed) == 0 {
        let dev = super::DEVICE.load(Acquire) as *mut Device;
        if !dev.is_null() {
            frame(dev);
        }
    }
    orig(sc, src, dst, wnd, dirty, flags)
}

pub unsafe extern "system" fn hk_reset(
    dev: *mut Device,
    pp: *mut D3DPRESENT_PARAMETERS,
) -> HRESULT {
    let orig = orig!(ORIG_RESET as ResetFn);
    bump(&C.resets, 1);
    ro::invalidate_any_thread();
    ro::RECORDING.store(false, Relaxed);
    let t = Instant::now();
    let hr = orig(dev, pp);
    // Reset returns the device to its default state.
    ro::invalidate_any_thread();
    let params = if pp.is_null() {
        "null".into()
    } else {
        (*pp).describe()
    };
    crate::log_info!(
        "device Reset -> {hr:#x} in {:.1} ms; {params}",
        t.elapsed().as_secs_f64() * 1000.0
    );
    hr
}

// --- state ------------------------------------------------------------------

pub unsafe extern "system" fn hk_set_render_state(
    dev: *mut Device,
    state: u32,
    value: u32,
) -> HRESULT {
    ro::set_render_state(
        dev,
        state,
        value,
        orig!(ORIG_SET_RENDER_STATE as SetRenderStateFn),
    )
}

pub unsafe extern "system" fn hk_set_sampler_state(
    dev: *mut Device,
    sampler: u32,
    ty: u32,
    value: u32,
) -> HRESULT {
    ro::set_stage_state(
        dev,
        StageKind::Sampler,
        sampler,
        ty,
        value,
        orig!(ORIG_SET_SAMPLER_STATE as SetStageStateFn),
    )
}

pub unsafe extern "system" fn hk_set_texture_stage_state(
    dev: *mut Device,
    stage: u32,
    ty: u32,
    value: u32,
) -> HRESULT {
    ro::set_stage_state(
        dev,
        StageKind::TextureStage,
        stage,
        ty,
        value,
        orig!(ORIG_SET_TSS as SetStageStateFn),
    )
}

pub unsafe extern "system" fn hk_begin_state_block(dev: *mut Device) -> HRESULT {
    let hr = orig!(ORIG_BEGIN_SB as BeginStateBlockFn)(dev);
    if hr >= 0 {
        ro::RECORDING.store(true, Release);
    }
    hr
}

pub unsafe extern "system" fn hk_end_state_block(
    dev: *mut Device,
    out: *mut *mut c_void,
) -> HRESULT {
    let hr = orig!(ORIG_END_SB as EndStateBlockFn)(dev, out);
    ro::RECORDING.store(false, Release);
    if hr >= 0 && !out.is_null() {
        hook_state_block(*out);
    }
    hr
}

pub unsafe extern "system" fn hk_create_state_block(
    dev: *mut Device,
    ty: u32,
    out: *mut *mut c_void,
) -> HRESULT {
    let hr = orig!(ORIG_CREATE_SB as CreateStateBlockFn)(dev, ty, out);
    if hr >= 0 && !out.is_null() {
        hook_state_block(*out);
    }
    hr
}

/// Every state block must have its Apply hooked, otherwise the shadow state could
/// go stale. A state block whose class we could not hook disables filtering.
unsafe fn hook_state_block(sb: *mut c_void) {
    if sb.is_null() {
        return;
    }
    let vt = *(sb as *const usize);
    if SB_VTABLE.load(Acquire) == vt {
        return;
    }
    match SB_VTABLE.compare_exchange(0, vt, AcqRel, Acquire) {
        Ok(_) => {
            let ok = hook::validate_vtable(vt, sb_vt::COUNT)
                && hook::patch_slot((vt as *mut usize).add(sb_vt::APPLY), hk_state_block_apply as *const () as usize, &ORIG_SB_APPLY).is_ok();
            if ok {
                crate::log_info!("hooked IDirect3DStateBlock9::Apply (vtable {vt:#x})");
            } else {
                ro::lock_off(format_args!("could not hook IDirect3DStateBlock9::Apply"));
            }
        }
        Err(prev) if prev == vt => {}
        Err(prev) => ro::lock_off(format_args!(
            "second state-block class seen (vtable {vt:#x}, hooked {prev:#x}); its Apply is not tracked"
        )),
    }
}

unsafe extern "system" fn hk_state_block_apply(this: *mut c_void) -> HRESULT {
    let hr = orig!(ORIG_SB_APPLY as StateBlockApplyFn)(this);
    bump(&C.state_block_applies, 1);
    ro::invalidate_any_thread();
    hr
}

pub unsafe extern "system" fn hk_set_texture(
    dev: *mut Device,
    stage: u32,
    tex: *mut c_void,
) -> HRESULT {
    let hr = orig!(ORIG_SET_TEXTURE as SetTextureFn)(dev, stage, tex);
    ro::note_set_texture(stage, tex, hr);
    hr
}

// --- draws ------------------------------------------------------------------

pub unsafe extern "system" fn hk_draw_primitive(
    dev: *mut Device,
    prim: u32,
    start: u32,
    count: u32,
) -> HRESULT {
    bump(&C.draw_calls, 1);
    bump(&C.primitives, count);
    orig!(ORIG_DP as DrawPrimitiveFn)(dev, prim, start, count)
}

pub unsafe extern "system" fn hk_draw_indexed_primitive(
    dev: *mut Device,
    prim: u32,
    base_vertex: i32,
    min_index: u32,
    num_vertices: u32,
    start_index: u32,
    prim_count: u32,
) -> HRESULT {
    bump(&C.draw_calls, 1);
    bump(&C.primitives, prim_count);
    orig!(ORIG_DIP as DrawIndexedPrimitiveFn)(
        dev,
        prim,
        base_vertex,
        min_index,
        num_vertices,
        start_index,
        prim_count,
    )
}

pub unsafe extern "system" fn hk_draw_primitive_up(
    dev: *mut Device,
    prim: u32,
    prim_count: u32,
    data: *const c_void,
    stride: u32,
) -> HRESULT {
    bump(&C.draw_up_calls, 1);
    bump(&C.primitives, prim_count);
    orig!(ORIG_DPUP as DrawPrimitiveUPFn)(dev, prim, prim_count, data, stride)
}

pub unsafe extern "system" fn hk_draw_indexed_primitive_up(
    dev: *mut Device,
    prim: u32,
    min_index: u32,
    num_vertices: u32,
    prim_count: u32,
    indices: *const c_void,
    index_format: u32,
    vertices: *const c_void,
    stride: u32,
) -> HRESULT {
    bump(&C.draw_up_calls, 1);
    bump(&C.primitives, prim_count);
    orig!(ORIG_DIPUP as DrawIndexedPrimitiveUPFn)(
        dev,
        prim,
        min_index,
        num_vertices,
        prim_count,
        indices,
        index_format,
        vertices,
        stride,
    )
}

// --- binding counters --------------------------------------------------------

pub unsafe extern "system" fn hk_set_stream_source(
    dev: *mut Device,
    stream: u32,
    vb: *mut c_void,
    offset: u32,
    stride: u32,
) -> HRESULT {
    bump(&C.set_stream_source, 1);
    orig!(ORIG_SET_STREAM_SOURCE as SetStreamSourceFn)(dev, stream, vb, offset, stride)
}

pub unsafe extern "system" fn hk_set_vertex_shader(dev: *mut Device, p: *mut c_void) -> HRESULT {
    bump(&C.set_shader, 1);
    orig!(ORIG_SET_VS as SetPtrFn)(dev, p)
}

pub unsafe extern "system" fn hk_set_pixel_shader(dev: *mut Device, p: *mut c_void) -> HRESULT {
    bump(&C.set_shader, 1);
    orig!(ORIG_SET_PS as SetPtrFn)(dev, p)
}

pub unsafe extern "system" fn hk_set_fvf(dev: *mut Device, fvf: u32) -> HRESULT {
    bump(&C.set_vertex_format, 1);
    orig!(ORIG_SET_FVF as SetFvfFn)(dev, fvf)
}

pub unsafe extern "system" fn hk_set_vertex_declaration(
    dev: *mut Device,
    p: *mut c_void,
) -> HRESULT {
    bump(&C.set_vertex_format, 1);
    orig!(ORIG_SET_DECL as SetPtrFn)(dev, p)
}

// --- resources -----------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
pub unsafe extern "system" fn hk_create_texture(
    dev: *mut Device,
    w: u32,
    h: u32,
    levels: u32,
    usage: u32,
    format: u32,
    pool: u32,
    out: *mut *mut c_void,
    shared: *mut c_void,
) -> HRESULT {
    let hr = orig!(ORIG_CREATE_TEXTURE as CreateTextureFn)(
        dev, w, h, levels, usage, format, pool, out, shared,
    );
    if hr >= 0 && !out.is_null() {
        bump(&C.textures_created, 1);
        resources::on_texture_created(*out, w, h, levels, format, pool);
    }
    hr
}

pub unsafe extern "system" fn hk_create_vertex_buffer(
    dev: *mut Device,
    len: u32,
    usage: u32,
    fvf: u32,
    pool: u32,
    out: *mut *mut c_void,
    shared: *mut c_void,
) -> HRESULT {
    let hr = orig!(ORIG_CREATE_VB as CreateVertexBufferFn)(dev, len, usage, fvf, pool, out, shared);
    if hr >= 0 {
        bump(&C.buffers_created, 1);
        bump(&C.buffer_bytes_created, len);
    }
    hr
}

pub unsafe extern "system" fn hk_create_index_buffer(
    dev: *mut Device,
    len: u32,
    usage: u32,
    format: u32,
    pool: u32,
    out: *mut *mut c_void,
    shared: *mut c_void,
) -> HRESULT {
    let hr =
        orig!(ORIG_CREATE_IB as CreateIndexBufferFn)(dev, len, usage, format, pool, out, shared);
    if hr >= 0 {
        bump(&C.buffers_created, 1);
        bump(&C.buffer_bytes_created, len);
    }
    hr
}
