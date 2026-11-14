//! render_optimizer: redundant D3D9 state-change detection and (optional) filtering.
//!
//! See `docs/optimizations/0001-redundant-state-filter.md` for the full
//! description, risks and benchmark protocol.
//!
//! Threading model
//! ---------------
//! The shadow state is owned by the *render thread* (the thread that created
//! the device, or for a late attach the first thread that calls `Present`).
//! Only that thread ever touches [`Shadow`]. A `Set*` call arriving on any
//! other thread is passed through untouched and marks the shadow dirty, which
//! the render thread turns into a full invalidation on its next call.

use crate::d3d9::types::*;
use crate::win32;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering::*};
use sape_core::state_cache::{commit, decide, Decision, FilterCounters, StateCache};

pub struct Shadow {
    pub rs: StateCache<RENDER_STATE_SLOTS>,
    pub samp: [StateCache<SAMPLER_STATE_SLOTS>; SAMPLERS],
    pub tss: [StateCache<TSS_SLOTS>; TEXTURE_STAGES],
    /// Bound texture per pixel sampler (low 32 bits of the pointer). Counting only.
    pub tex: StateCache<SAMPLERS>,
    pub rs_k: FilterCounters,
    pub samp_k: FilterCounters,
    pub tss_k: FilterCounters,
    pub tex_k: FilterCounters,
    pub invalidations: u32,
}

impl Shadow {
    const fn new() -> Self {
        const S: StateCache<SAMPLER_STATE_SLOTS> = StateCache::new();
        const T: StateCache<TSS_SLOTS> = StateCache::new();
        Self {
            rs: StateCache::new(),
            samp: [S; SAMPLERS],
            tss: [T; TEXTURE_STAGES],
            tex: StateCache::new(),
            rs_k: FilterCounters::ZERO,
            samp_k: FilterCounters::ZERO,
            tss_k: FilterCounters::ZERO,
            tex_k: FilterCounters::ZERO,
            invalidations: 0,
        }
    }

    pub fn invalidate_all(&mut self) {
        self.rs.invalidate_all();
        self.samp.iter_mut().for_each(StateCache::invalidate_all);
        self.tss.iter_mut().for_each(StateCache::invalidate_all);
        self.tex.invalidate_all();
        self.invalidations = self.invalidations.wrapping_add(1);
    }
}

struct RenderCell(UnsafeCell<Shadow>);
// SAFETY: access is restricted to the single owner thread by `shadow()`.
unsafe impl Sync for RenderCell {}

static SHADOW: RenderCell = RenderCell(UnsafeCell::new(Shadow::new()));
pub static OWNER_THREAD: AtomicU32 = AtomicU32::new(0);
static DIRTY: AtomicBool = AtomicBool::new(false);
/// True between BeginStateBlock and EndStateBlock: Set* calls are recorded into
/// the block and do NOT change device state, so they must not touch the cache.
pub static RECORDING: AtomicBool = AtomicBool::new(false);

/// Runtime switch (hotkey / A-B mode).
pub static FILTER_ON: AtomicBool = AtomicBool::new(false);
/// Set when a safety check fails; filtering cannot be re-enabled this session.
pub static FILTER_LOCKED_OFF: AtomicBool = AtomicBool::new(false);
pub static CAT_RS: AtomicBool = AtomicBool::new(true);
pub static CAT_SAMP: AtomicBool = AtomicBool::new(true);
pub static CAT_TSS: AtomicBool = AtomicBool::new(true);
/// Validation mode: verify every skipped call with Get* and lock filtering off on mismatch.
pub static VALIDATE: AtomicBool = AtomicBool::new(false);

pub fn configure(cfg: &sape_core::config::StateFilterConfig, validate: bool) {
    CAT_RS.store(cfg.render_states, Relaxed);
    CAT_SAMP.store(cfg.sampler_states, Relaxed);
    CAT_TSS.store(cfg.texture_stage_states, Relaxed);
    FILTER_ON.store(cfg.filter, Relaxed);
    VALIDATE.store(validate, Relaxed);
}

/// Returns the shadow if called on the owner thread; otherwise marks it dirty.
#[inline]
pub fn shadow() -> Option<*mut Shadow> {
    let owner = OWNER_THREAD.load(Relaxed);
    if owner == 0 {
        return None;
    }
    if owner != win32::current_thread_id() {
        DIRTY.store(true, Release);
        return None;
    }
    let p = SHADOW.0.get();
    if DIRTY.load(Relaxed) && DIRTY.swap(false, AcqRel) {
        // SAFETY: owner thread, no other live reference (callers never hold one across calls).
        unsafe { (*p).invalidate_all() };
    }
    Some(p)
}

/// Invalidate from any thread (Reset, StateBlock::Apply, ...).
pub fn invalidate_any_thread() {
    match shadow() {
        // SAFETY: owner thread.
        Some(p) => unsafe { (*p).invalidate_all() },
        None => DIRTY.store(true, Release),
    }
}

pub fn set_owner_thread(tid: u32) {
    if OWNER_THREAD
        .compare_exchange(0, tid, AcqRel, Relaxed)
        .is_ok()
    {
        crate::log_info!("render thread = {tid}");
    }
}

#[inline]
fn filtering(cat: &AtomicBool) -> bool {
    FILTER_ON.load(Relaxed) && cat.load(Relaxed) && !FILTER_LOCKED_OFF.load(Relaxed)
}

pub fn set_filter(on: bool) -> bool {
    if on && FILTER_LOCKED_OFF.load(Relaxed) {
        return false;
    }
    FILTER_ON.store(on, Relaxed);
    true
}

pub fn filter_active() -> bool {
    FILTER_ON.load(Relaxed) && !FILTER_LOCKED_OFF.load(Relaxed)
}

/// Permanently disables filtering for this session.
pub fn lock_off(reason: core::fmt::Arguments) {
    if !FILTER_LOCKED_OFF.swap(true, AcqRel) {
        crate::log_error!("state filter permanently disabled: {reason}");
    }
    invalidate_any_thread();
}

/// Snapshot + reset of the per-frame filter counters (render thread only).
pub fn take_counters() -> Option<[FilterCounters; 4]> {
    let p = shadow()?;
    // SAFETY: owner thread.
    let s = unsafe { &mut *p };
    Some([
        s.rs_k.take(),
        s.samp_k.take(),
        s.tss_k.take(),
        s.tex_k.take(),
    ])
}

// ---------------------------------------------------------------------------
// Call routing used by the device hooks
// ---------------------------------------------------------------------------

#[inline]
unsafe fn vfn(dev: *mut Device, index: usize) -> usize {
    *(*(dev as *const *const usize)).add(index)
}

/// Routes `SetRenderState`.
///
/// # Safety
/// `dev` must be a valid device; `orig` its original SetRenderState.
#[inline]
pub unsafe fn set_render_state(
    dev: *mut Device,
    state: u32,
    value: u32,
    orig: SetRenderStateFn,
) -> HRESULT {
    let Some(sh) = shadow() else {
        return orig(dev, state, value);
    };
    let tracking = !RECORDING.load(Relaxed);
    let d = {
        let s = &mut *sh;
        decide(
            &s.rs,
            state as usize,
            value,
            tracking,
            filtering(&CAT_RS),
            &mut s.rs_k,
        )
    };
    if d == Decision::Skip && (!VALIDATE.load(Relaxed) || validate_render_state(dev, state, value))
    {
        return D3D_OK;
    }
    let hr = orig(dev, state, value);
    if matches!(d, Decision::PassThrough { track: true } | Decision::Skip) {
        commit(&mut (*sh).rs, state as usize, value, hr);
    }
    hr
}

unsafe fn validate_render_state(dev: *mut Device, state: u32, expected: u32) -> bool {
    let get: GetRenderStateFn =
        core::mem::transmute(vfn(dev, crate::d3d9::types::dev_vt::GET_RENDER_STATE));
    let mut actual = 0u32;
    let hr = get(dev, state, &mut actual);
    validation_outcome(
        hr,
        actual == expected,
        format_args!("render state {state}: cache {expected:#x}, device {actual:#x}"),
    )
}

fn validation_outcome(hr: HRESULT, matched: bool, what: core::fmt::Arguments) -> bool {
    if hr < 0 {
        // Pure devices cannot answer Get*; stop validating but keep running.
        if VALIDATE.swap(false, Relaxed) {
            crate::log_warn!(
                "state validation unavailable (Get* returned {hr:#x}); validation disabled"
            );
        }
        return true;
    }
    if !matched {
        lock_off(format_args!("validation mismatch: {what}"));
        return false;
    }
    true
}

/// Which per-stage cache a stage-state call uses.
#[derive(Clone, Copy)]
pub enum StageKind {
    Sampler,
    TextureStage,
}

/// Routes `SetSamplerState` / `SetTextureStageState`.
///
/// # Safety
/// `dev` must be a valid device; `orig` the matching original method.
#[inline]
pub unsafe fn set_stage_state(
    dev: *mut Device,
    kind: StageKind,
    stage: u32,
    ty: u32,
    value: u32,
    orig: SetStageStateFn,
) -> HRESULT {
    let Some(sh) = shadow() else {
        return orig(dev, stage, ty, value);
    };
    let tracking = !RECORDING.load(Relaxed);
    let st = stage as usize;
    let idx = ty as usize;
    let d = {
        let s = &mut *sh;
        match kind {
            StageKind::Sampler => match s.samp.get(st) {
                Some(c) => decide(c, idx, value, tracking, filtering(&CAT_SAMP), &mut s.samp_k),
                None => decide(&s.samp[0], usize::MAX, value, false, false, &mut s.samp_k),
            },
            StageKind::TextureStage => match s.tss.get(st) {
                Some(c) => decide(c, idx, value, tracking, filtering(&CAT_TSS), &mut s.tss_k),
                None => decide(&s.tss[0], usize::MAX, value, false, false, &mut s.tss_k),
            },
        }
    };
    if d == Decision::Skip
        && (!VALIDATE.load(Relaxed) || validate_stage_state(dev, kind, stage, ty, value))
    {
        return D3D_OK;
    }
    let hr = orig(dev, stage, ty, value);
    if matches!(d, Decision::PassThrough { track: true } | Decision::Skip) {
        let s = &mut *sh;
        match kind {
            StageKind::Sampler => commit(&mut s.samp[st], idx, value, hr),
            StageKind::TextureStage => commit(&mut s.tss[st], idx, value, hr),
        }
    }
    hr
}

unsafe fn validate_stage_state(
    dev: *mut Device,
    kind: StageKind,
    stage: u32,
    ty: u32,
    expected: u32,
) -> bool {
    let index = match kind {
        StageKind::Sampler => dev_vt::GET_SAMPLER_STATE,
        StageKind::TextureStage => dev_vt::GET_TEXTURE_STAGE_STATE,
    };
    let get: GetStageStateFn = core::mem::transmute(vfn(dev, index));
    let mut actual = 0u32;
    let hr = get(dev, stage, ty, &mut actual);
    let name = match kind {
        StageKind::Sampler => "sampler",
        StageKind::TextureStage => "texture stage",
    };
    validation_outcome(
        hr,
        actual == expected,
        format_args!("{name} {stage} state {ty}: cache {expected:#x}, device {actual:#x}"),
    )
}

/// Counts `SetTexture` redundancy (never skips: texture binding is left untouched
/// in this milestone).
///
/// # Safety
/// Must be called from a device hook.
#[inline]
pub unsafe fn note_set_texture(stage: u32, tex: *mut core::ffi::c_void, hr: HRESULT) {
    // Pointer equality is only exact on 32-bit, where the pointer fits in the u32 cache.
    if cfg!(not(target_pointer_width = "32")) {
        return;
    }
    let Some(sh) = shadow() else { return };
    let s = &mut *sh;
    let tracking = !RECORDING.load(Relaxed);
    let v = tex as usize as u32;
    let _ = decide(&s.tex, stage as usize, v, tracking, false, &mut s.tex_k);
    if tracking {
        commit(&mut s.tex, stage as usize, v, hr);
    }
}
