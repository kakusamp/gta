//! performance_monitor: frame timing, spike detection, per-frame API counters,
//! process CPU / RAM / address-space / VRAM sampling, CSV capture and hotkeys.
//!
//! Overhead budget: level 1 costs one `Instant::now()` + a few arithmetic ops
//! per frame; system metrics are sampled only once per summary interval. CSV
//! lines are formatted on the render thread into a reused buffer and written
//! by a background thread so file I/O never blocks a frame.

use crate::d3d9::types::{dev_vt, Device, GetAvailableTextureMemFn};
use crate::render_optimizer as ro;
use crate::{resources, win32};
use core::cell::UnsafeCell;
use core::fmt::Write as _;
use core::sync::atomic::{AtomicU32, Ordering::*};
use sape_core::config::Config;
use sape_core::state_cache::FilterCounters;
use sape_core::stats::{summarize, SpikeDetector};
use std::io::Write as _;
use std::sync::mpsc;
use std::time::Instant;

/// Per-frame API counters, incremented from device hooks (any thread).
/// Increments are relaxed load+store (a plain `mov`, not `lock xadd`): a
/// concurrent increment from a second thread may be lost, which is acceptable
/// for statistics and keeps hook overhead minimal.
pub struct FrameCounters {
    pub draw_calls: AtomicU32,
    pub draw_up_calls: AtomicU32,
    pub primitives: AtomicU32,
    pub set_stream_source: AtomicU32,
    pub set_shader: AtomicU32,
    pub set_vertex_format: AtomicU32,
    pub textures_created: AtomicU32,
    pub buffers_created: AtomicU32,
    pub buffer_bytes_created: AtomicU32,
    pub state_block_applies: AtomicU32,
    pub resets: AtomicU32,
}

#[inline]
pub fn bump(c: &AtomicU32, n: u32) {
    c.store(c.load(Relaxed).wrapping_add(n), Relaxed);
}

#[inline]
fn take(c: &AtomicU32) -> u32 {
    c.swap(0, Relaxed)
}

pub static C: FrameCounters = FrameCounters {
    draw_calls: AtomicU32::new(0),
    draw_up_calls: AtomicU32::new(0),
    primitives: AtomicU32::new(0),
    set_stream_source: AtomicU32::new(0),
    set_shader: AtomicU32::new(0),
    set_vertex_format: AtomicU32::new(0),
    textures_created: AtomicU32::new(0),
    buffers_created: AtomicU32::new(0),
    buffer_bytes_created: AtomicU32::new(0),
    state_block_applies: AtomicU32::new(0),
    resets: AtomicU32::new(0),
};

#[derive(Default, Clone, Copy)]
struct FrameSample {
    draws: u32,
    draws_up: u32,
    prims: u32,
    streams: u32,
    shaders: u32,
    vformats: u32,
    tex_created: u32,
    buf_created: u32,
    buf_bytes: u32,
    sb_applies: u32,
    rs: FilterCounters,
    samp: FilterCounters,
    tss: FilterCounters,
    tex: FilterCounters,
}

impl FrameSample {
    fn collect() -> Self {
        let [rs, samp, tss, tex] = ro::take_counters().unwrap_or_default();
        Self {
            draws: take(&C.draw_calls),
            draws_up: take(&C.draw_up_calls),
            prims: take(&C.primitives),
            streams: take(&C.set_stream_source),
            shaders: take(&C.set_shader),
            vformats: take(&C.set_vertex_format),
            tex_created: take(&C.textures_created),
            buf_created: take(&C.buffers_created),
            buf_bytes: take(&C.buffer_bytes_created),
            sb_applies: take(&C.state_block_applies),
            rs,
            samp,
            tss,
            tex,
        }
    }
}

#[derive(Default)]
struct Totals {
    s: [u64; 18],
}

impl Totals {
    fn add(&mut self, f: &FrameSample) {
        let v = [
            f.draws,
            f.draws_up,
            f.prims,
            f.streams,
            f.shaders,
            f.vformats,
            f.tex_created,
            f.buf_created,
            f.buf_bytes,
            f.sb_applies,
            f.rs.calls,
            f.rs.redundant,
            f.rs.skipped,
            f.samp.calls + f.tss.calls,
            f.samp.redundant + f.tss.redundant,
            f.samp.skipped + f.tss.skipped,
            f.tex.calls,
            f.tex.redundant,
        ];
        for (a, b) in self.s.iter_mut().zip(v) {
            *a += b as u64;
        }
    }
}

struct Capture {
    tx: mpsc::Sender<String>,
    buf: String,
    path: std::path::PathBuf,
}

struct State {
    start: Instant,
    last: Option<Instant>,
    frame: u64,
    window_start: Instant,
    window_ms: Vec<f32>,
    scratch: Vec<f32>,
    window_spikes: u32,
    totals: Totals,
    spike: SpikeDetector,
    cpu_last: Option<(u64, Instant)>,
    capture: Option<Capture>,
    keys_prev: [bool; 3],
    ab_last: Instant,
    full: bool,
}

struct Cell(UnsafeCell<Option<State>>);
// SAFETY: only accessed from the render thread inside the Present hook.
unsafe impl Sync for Cell {}
static STATE: Cell = Cell(UnsafeCell::new(None));

/// Called at the start of every `Present` on the render thread.
///
/// # Safety
/// Must only be called from the Present hook (single render thread) with a valid device.
pub unsafe fn on_present(dev: *mut Device, cfg: &Config) {
    let now = Instant::now();
    let st = (*STATE.0.get()).get_or_insert_with(|| {
        crate::log_info!("first Present observed; monitor running");
        let mut s = State {
            start: now,
            last: None,
            frame: 0,
            window_start: now,
            window_ms: Vec::with_capacity(4096),
            scratch: Vec::with_capacity(4096),
            window_spikes: 0,
            totals: Totals::default(),
            spike: SpikeDetector::new(cfg.monitor.spike_factor, cfg.monitor.spike_min_ms),
            cpu_last: None,
            capture: None,
            keys_prev: [false; 3],
            ab_last: now,
            full: cfg.monitor.level >= sape_core::config::MonitorLevel::Full,
        };
        s.cpu_last = process_cpu_100ns().map(|t| (t, now));
        if cfg.monitor.capture_on_start {
            start_capture(&mut s);
        }
        s
    });

    let sample = FrameSample::collect();
    if let Some(last) = st.last {
        let ms = (now - last).as_secs_f64() as f32 * 1000.0;
        st.frame += 1;
        st.window_ms.push(ms);
        st.totals.add(&sample);
        let spike = st.spike.observe(ms);
        if let Some(median) = spike {
            st.window_spikes += 1;
            if cfg.monitor.log_spikes {
                crate::log_info!(
                    "spike frame {}: {ms:.2} ms (median {median:.2}) draws={} prims={} rs={} tex_created={} buffers_created={} ({} KB) sb_applies={}",
                    st.frame,
                    sample.draws + sample.draws_up,
                    sample.prims,
                    sample.rs.calls,
                    sample.tex_created,
                    sample.buf_created,
                    sample.buf_bytes / 1024,
                    sample.sb_applies
                );
            }
        }
        if let Some(cap) = st.capture.as_mut() {
            let _ = writeln!(
                cap.buf,
                "{},{:.3},{:.4},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
                st.frame,
                (now - st.start).as_secs_f64() * 1000.0,
                ms,
                ro::filter_active() as u8,
                sample.draws + sample.draws_up,
                sample.prims,
                sample.rs.calls,
                sample.rs.redundant,
                sample.rs.skipped,
                sample.samp.calls,
                sample.samp.redundant,
                sample.tss.calls,
                sample.tss.redundant,
                sample.tex.calls,
                sample.tex.redundant,
                sample.tex_created,
                sample.buf_created,
                sample.sb_applies,
                spike.is_some() as u8
            );
        }
    }
    st.last = Some(now);

    if cfg.hotkeys.enabled {
        poll_hotkeys(st, dev, cfg, now);
    }

    // A/B mode: flip the filter periodically while capturing.
    let ab = cfg.state_filter.ab_interval_s;
    if ab > 0
        && cfg.state_filter.enabled
        && st.capture.is_some()
        && (now - st.ab_last).as_secs() >= ab as u64
    {
        st.ab_last = now;
        let next = !ro::filter_active();
        if ro::set_filter(next) {
            crate::log_info!(
                "A/B: state filter {} at frame {}",
                if next { "ON" } else { "OFF" },
                st.frame
            );
        }
    }

    if (now - st.window_start).as_millis() as u32 >= cfg.monitor.summary_interval_ms {
        flush_window(st, dev, now);
    }
}

unsafe fn poll_hotkeys(st: &mut State, dev: *mut Device, cfg: &Config, now: Instant) {
    let h = &cfg.hotkeys;
    let keys = [h.toggle_filter, h.dump_summary, h.toggle_capture];
    let mods_ok = !h.require_ctrl_shift
        || (win32::key_down(win32::VK_CONTROL) && win32::key_down(win32::VK_SHIFT));
    let mut pressed = [false; 3];
    for (i, &vk) in keys.iter().enumerate() {
        let down = vk != 0 && win32::key_down(vk as i32);
        pressed[i] = down && !st.keys_prev[i];
        st.keys_prev[i] = down;
    }
    if !pressed.iter().any(|&p| p) || !mods_ok || !win32::process_has_focus() {
        return;
    }
    if pressed[0] {
        if !cfg.state_filter.enabled {
            crate::log_warn!(
                "hotkey: state filter hooks are not installed (state_filter.enabled=0)"
            );
        } else {
            let next = !ro::filter_active();
            if ro::set_filter(next) {
                crate::log_info!(
                    "hotkey: state filter {} at frame {}",
                    if next { "ON" } else { "OFF" },
                    st.frame
                );
            } else {
                crate::log_warn!("hotkey: state filter is locked off after a safety check failure");
            }
        }
    }
    if pressed[1] {
        flush_window(st, dev, now);
    }
    if pressed[2] {
        if st.capture.is_some() {
            stop_capture(st);
        } else {
            start_capture(st);
        }
    }
}

fn start_capture(st: &mut State) {
    let Some(rt) = crate::runtime() else { return };
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let path = rt.dir.join(format!("sape_frames_{ts}.csv"));
    let mut file = match std::fs::File::create(&path) {
        Ok(f) => std::io::BufWriter::new(f),
        Err(e) => {
            crate::log_error!("capture: cannot create {}: {e}", path.display());
            return;
        }
    };
    let (tx, rx) = mpsc::channel::<String>();
    let spawned = std::thread::Builder::new().name("sape-csv".into()).spawn(move || {
        let _ = file.write_all(
            b"frame,t_ms,frame_ms,filter_on,draws,prims,rs_calls,rs_redundant,rs_skipped,samp_calls,samp_redundant,tss_calls,tss_redundant,tex_calls,tex_redundant,tex_created,buf_created,sb_applies,spike\n",
        );
        while let Ok(chunk) = rx.recv() {
            let _ = file.write_all(chunk.as_bytes());
        }
        let _ = file.flush();
    });
    if let Err(e) = spawned {
        crate::log_error!("capture: cannot start writer thread: {e}");
        return;
    }
    crate::log_info!("capture started: {}", path.display());
    st.ab_last = Instant::now();
    st.capture = Some(Capture {
        tx,
        buf: String::with_capacity(256 * 1024),
        path,
    });
}

fn stop_capture(st: &mut State) {
    if let Some(mut cap) = st.capture.take() {
        let _ = cap.tx.send(core::mem::take(&mut cap.buf));
        crate::log_info!(
            "capture stopped: {} (analyze with sape-analyze)",
            cap.path.display()
        );
        // Dropping the sender ends the writer thread after it drains.
    }
}

unsafe fn flush_window(st: &mut State, dev: *mut Device, now: Instant) {
    let secs = (now - st.window_start).as_secs_f64();
    let s = summarize(&st.window_ms, &mut st.scratch);
    let frames = s.frames.max(1) as f64;
    let t = &st.totals.s;
    let per = |i: usize| t[i] as f64 / frames;
    let pct = |num: u64, den: u64| {
        if den == 0 {
            0.0
        } else {
            num as f64 * 100.0 / den as f64
        }
    };

    let mut line = format!(
        "[summary {secs:.1}s] {} frames | avg {:.1} fps ({:.2} ms) | 1% low {:.1} | p99 {:.2} ms | max {:.2} ms | sd {:.2} ms | spikes {}",
        s.frames, s.avg_fps, s.avg_ms, s.low_1pct_fps, s.p99_ms, s.max_ms, s.stddev_ms, st.window_spikes
    );

    // CPU: process time / wall time. 1.00 core = one core fully busy.
    if let (Some(cpu_now), Some((cpu_prev, t_prev))) = (process_cpu_100ns(), st.cpu_last) {
        let wall = (now - t_prev).as_secs_f64();
        if wall > 0.0 {
            let cores = (cpu_now.saturating_sub(cpu_prev)) as f64 / 1e7 / wall;
            let n = std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1);
            let _ = write!(
                line,
                " | CPU {cores:.2} cores ({:.0}% of {n})",
                cores * 100.0 / n as f64
            );
        }
        st.cpu_last = Some((cpu_now, now));
    }
    if let Some((ws, private)) = process_memory() {
        let _ = write!(line, " | WS {} MB, private {} MB", ws >> 20, private >> 20);
    }
    if let Some((used, total)) = address_space() {
        let _ = write!(
            line,
            " | VA {:.0}/{:.0} MB",
            used as f64 / 1048576.0,
            total as f64 / 1048576.0
        );
    }
    if !dev.is_null() {
        let f: GetAvailableTextureMemFn = core::mem::transmute(
            *(*(dev as *const *const usize)).add(dev_vt::GET_AVAILABLE_TEXTURE_MEM),
        );
        let _ = write!(line, " | VRAM avail ~{} MB", f(dev) >> 20);
    }
    if st.full || t[10] > 0 {
        let _ = write!(
            line,
            "\n    per frame: draws {:.0} (UP {:.0}) prims {:.0} streams {:.0} shaders {:.0} vfmt {:.0} | RS {:.0} redundant {:.1}% skipped {:.0} | samp+tss {:.0} redundant {:.1}% skipped {:.0} | SetTexture {:.0} redundant {:.1}% | sb_apply {:.1}",
            per(0) + per(1), per(1), per(2), per(3), per(4), per(5), per(10), pct(t[11], t[10]), per(12),
            per(13), pct(t[14], t[13]), per(15), per(16), pct(t[17], t[16]), per(9),
        );
        let _ = write!(
            line,
            "\n    created in window: textures {} buffers {} ({} KB)",
            t[6],
            t[7],
            t[8] / 1024
        );
    }
    let r = resources::snapshot();
    if r.created_total > 0 {
        let _ = write!(
            line,
            "\n    live textures {} | est. bytes default {} MB managed {} MB sysmem {} MB",
            r.live_textures,
            r.live_bytes[0] >> 20,
            r.live_bytes[1] >> 20,
            r.live_bytes[2] >> 20
        );
    }
    let _ = write!(
        line,
        " | filter {}",
        if ro::filter_active() { "ON" } else { "off" }
    );
    crate::log_info!("{line}");

    if let Some(cap) = st.capture.as_mut() {
        let chunk = core::mem::replace(&mut cap.buf, String::with_capacity(256 * 1024));
        if cap.tx.send(chunk).is_err() {
            crate::log_error!("capture writer thread exited; stopping capture");
            st.capture = None;
        }
    }

    st.window_ms.clear();
    st.totals = Totals::default();
    st.window_spikes = 0;
    st.window_start = now;
}

fn process_cpu_100ns() -> Option<u64> {
    let (mut c, mut e, mut k, mut u) = Default::default();
    // SAFETY: valid out-pointers; pseudo-handle needs no closing.
    let ok = unsafe {
        win32::GetProcessTimes(win32::GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u)
    };
    (ok != 0).then(|| win32::FILETIME::as_u64(k) + win32::FILETIME::as_u64(u))
}

fn process_memory() -> Option<(usize, usize)> {
    let mut pmc = win32::PROCESS_MEMORY_COUNTERS {
        cb: core::mem::size_of::<win32::PROCESS_MEMORY_COUNTERS>() as u32,
        ..Default::default()
    };
    // SAFETY: valid out-pointer with matching cb.
    let ok =
        unsafe { win32::K32GetProcessMemoryInfo(win32::GetCurrentProcess(), &mut pmc, pmc.cb) };
    (ok != 0).then_some((pmc.WorkingSetSize, pmc.PagefileUsage))
}

/// (used, total) user-mode virtual address space. This is the resource that
/// actually runs out first in heavily modded 32-bit GTA SA.
pub fn address_space() -> Option<(u64, u64)> {
    let mut m = win32::MEMORYSTATUSEX {
        dwLength: core::mem::size_of::<win32::MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    // SAFETY: valid out-pointer with dwLength set.
    let ok = unsafe { win32::GlobalMemoryStatusEx(&mut m) };
    (ok != 0).then_some((m.ullTotalVirtual - m.ullAvailVirtual, m.ullTotalVirtual))
}
