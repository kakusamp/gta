//! memory_manager (milestone 1: measurement only).
//!
//! Tracks D3D9 resource creation and texture lifetime so that streaming /
//! SA-MP object churn can be correlated with frame-time spikes, and so later
//! memory optimizations have a baseline. Nothing here changes game behaviour.

use crate::d3d9::types::*;
use crate::hook;
use core::ffi::c_void;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering::*};
use std::collections::HashMap;
use std::sync::Mutex;

/// Per-texture info keyed by COM pointer. Only textures created after the hook
/// was installed are tracked, so "live" values can never go negative.
static LIVE: Mutex<Option<HashMap<usize, (u64, u32)>>> = Mutex::new(None);
/// Live estimated bytes per pool (DEFAULT, MANAGED, SYSTEMMEM, SCRATCH).
pub static LIVE_BYTES: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];
pub static LIVE_COUNT: AtomicUsize = AtomicUsize::new(0);
pub static CREATED_TOTAL: AtomicU64 = AtomicU64::new(0);

static TEXTURE_VTABLE: AtomicUsize = AtomicUsize::new(0);
static ORIG_TEX_RELEASE: AtomicUsize = AtomicUsize::new(0);

/// Called after a successful `CreateTexture`.
///
/// # Safety
/// `tex` must be a valid IDirect3DTexture9 just returned by the device.
pub unsafe fn on_texture_created(
    tex: *mut c_void,
    w: u32,
    h: u32,
    levels: u32,
    format: u32,
    pool: u32,
) {
    if tex.is_null() {
        return;
    }
    let vt = *(tex as *const usize);
    if TEXTURE_VTABLE
        .compare_exchange(0, vt, AcqRel, Relaxed)
        .is_ok()
    {
        let slot = (vt as *mut usize).add(unk_vt::RELEASE);
        match hook::patch_slot(
            slot,
            hk_texture_release as *const () as usize,
            &ORIG_TEX_RELEASE,
        ) {
            Ok(()) => crate::log_info!(
                "texture lifetime tracking: hooked IDirect3DTexture9::Release (vtable {vt:#x})"
            ),
            Err(e) => crate::log_warn!("texture lifetime tracking unavailable: {e:?}"),
        }
    }
    if TEXTURE_VTABLE.load(Relaxed) != vt || ORIG_TEX_RELEASE.load(Acquire) == 0 {
        return; // different texture class (e.g. a wrapper) - creation counted elsewhere only
    }
    let bytes = estimate_texture_bytes(w, h, levels, format);
    let pool_idx = (pool as usize).min(3);
    CREATED_TOTAL.fetch_add(1, Relaxed);
    if let Ok(mut g) = LIVE.lock() {
        let map = g.get_or_insert_with(HashMap::new);
        if let Some((old_b, old_p)) = map.insert(tex as usize, (bytes, pool)) {
            // Address reused without us seeing the final Release: correct the totals.
            LIVE_BYTES[(old_p as usize).min(3)].fetch_sub(old_b, Relaxed);
            LIVE_COUNT.fetch_sub(1, Relaxed);
        }
        LIVE_BYTES[pool_idx].fetch_add(bytes, Relaxed);
        LIVE_COUNT.fetch_add(1, Relaxed);
    }
}

unsafe extern "system" fn hk_texture_release(this: *mut c_void) -> u32 {
    let orig: ReleaseFn = core::mem::transmute(ORIG_TEX_RELEASE.load(Acquire));
    let key = this as usize;
    let refs = orig(this);
    if refs == 0 {
        // `this` is dangling now; only its numeric value is used as a key.
        if let Ok(mut g) = LIVE.lock() {
            if let Some((b, p)) = g.as_mut().and_then(|m| m.remove(&key)) {
                LIVE_BYTES[(p as usize).min(3)].fetch_sub(b, Relaxed);
                LIVE_COUNT.fetch_sub(1, Relaxed);
            }
        }
    }
    refs
}

pub struct ResourceSnapshot {
    pub live_textures: usize,
    pub live_bytes: [u64; 4],
    pub created_total: u64,
}

pub fn snapshot() -> ResourceSnapshot {
    ResourceSnapshot {
        live_textures: LIVE_COUNT.load(Relaxed),
        live_bytes: [0, 1, 2, 3].map(|i| LIVE_BYTES[i].load(Relaxed)),
        created_total: CREATED_TOTAL.load(Relaxed),
    }
}
