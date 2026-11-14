//! Configuration model (`sape.ini`). Every optimization is individually
//! switchable, and every *optimization* defaults to OFF until it has
//! published benchmark results (see `docs/optimizations/`).

use crate::ini::{parse_bool, parse_u32, Ini};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogLevel {
    Error = 0,
    Warn = 1,
    Info = 2,
    Debug = 3,
}

/// How much the performance monitor intercepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MonitorLevel {
    /// No monitoring hooks at all.
    Off = 0,
    /// Only `Present`/`Reset` are hooked: frame times, CPU, RAM, VRAM.
    FrameTime = 1,
    /// Additionally hooks draw / state / resource calls to count them.
    Full = 2,
}

#[derive(Debug, Clone)]
pub struct MonitorConfig {
    pub level: MonitorLevel,
    /// Interval of the summary line written to the log.
    pub summary_interval_ms: u32,
    /// Start writing the per-frame CSV immediately (otherwise via hotkey).
    pub capture_on_start: bool,
    /// A frame is a spike if `ms > max(spike_min_ms, spike_factor * rolling median)`.
    pub spike_factor: f32,
    pub spike_min_ms: f32,
    /// Log one line per detected spike with that frame's counters.
    pub log_spikes: bool,
}

#[derive(Debug, Clone)]
pub struct StateFilterConfig {
    /// Install the state-tracking hooks (needed for both analysis and filtering).
    pub enabled: bool,
    /// Actually skip redundant calls. If false, redundancy is only *counted*.
    pub filter: bool,
    pub render_states: bool,
    pub sampler_states: bool,
    pub texture_stage_states: bool,
    /// Benchmark helper: flip `filter` every N seconds while capturing (0 = off).
    pub ab_interval_s: u32,
    /// Safety/test mode: verify every skipped call with `Get*` against the real
    /// device; on any mismatch filtering is locked off for the session.
    /// Costs performance - use for compatibility testing, not for benchmarks.
    pub validate: bool,
}

#[derive(Debug, Clone)]
pub struct ResourceConfig {
    /// Track texture / vertex buffer / index buffer creation and texture lifetime.
    pub track: bool,
}

#[derive(Debug, Clone)]
pub struct HotkeyConfig {
    pub enabled: bool,
    /// Require Ctrl+Shift together with the key (avoids SA-MP F-key conflicts).
    pub require_ctrl_shift: bool,
    pub toggle_filter: u32,
    pub dump_summary: u32,
    pub toggle_capture: u32,
}

#[derive(Debug, Clone)]
pub struct CompatConfig {
    /// If Direct3D was initialised before SAPE loaded, attach to the existing
    /// device through the RenderWare device pointer (gta_sa 1.0 US only).
    pub late_attach: bool,
    /// Run D3D9-only features even on an unrecognised executable.
    pub allow_unknown_exe: bool,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub log_level: LogLevel,
    pub monitor: MonitorConfig,
    pub state_filter: StateFilterConfig,
    pub resources: ResourceConfig,
    pub hotkeys: HotkeyConfig,
    pub compat: CompatConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            log_level: LogLevel::Info,
            monitor: MonitorConfig {
                level: MonitorLevel::FrameTime,
                summary_interval_ms: 5000,
                capture_on_start: false,
                spike_factor: 2.5,
                spike_min_ms: 25.0,
                log_spikes: true,
            },
            state_filter: StateFilterConfig {
                enabled: false,
                filter: false,
                render_states: true,
                sampler_states: true,
                texture_stage_states: true,
                ab_interval_s: 0,
                validate: false,
            },
            resources: ResourceConfig { track: false },
            hotkeys: HotkeyConfig {
                enabled: true,
                require_ctrl_shift: true,
                toggle_filter: 0x7A,  // VK_F11
                dump_summary: 0x7B,   // VK_F12
                toggle_capture: 0x79, // VK_F10
            },
            compat: CompatConfig {
                late_attach: true,
                allow_unknown_exe: true,
            },
        }
    }
}

impl Config {
    /// Parses configuration text. Unknown or malformed values keep their
    /// defaults and produce a warning string (never a hard failure: a broken
    /// INI must not prevent the game from starting).
    pub fn from_ini_text(text: &str) -> (Self, Vec<String>) {
        let ini = Ini::parse(text);
        let mut c = Config::default();
        let mut w = Vec::new();

        if let Some(v) = ini.get("general", "log_level") {
            match v.to_ascii_lowercase().as_str() {
                "error" => c.log_level = LogLevel::Error,
                "warn" => c.log_level = LogLevel::Warn,
                "info" => c.log_level = LogLevel::Info,
                "debug" => c.log_level = LogLevel::Debug,
                _ => w.push(format!("general.log_level: unknown value '{v}'")),
            }
        }

        if let Some(v) = ini.get("monitor", "level") {
            match parse_u32(v) {
                Some(0) => c.monitor.level = MonitorLevel::Off,
                Some(1) => c.monitor.level = MonitorLevel::FrameTime,
                Some(2) => c.monitor.level = MonitorLevel::Full,
                _ => w.push(format!("monitor.level: expected 0, 1 or 2, got '{v}'")),
            }
        }
        read_u32(
            &ini,
            "monitor",
            "summary_interval_ms",
            &mut c.monitor.summary_interval_ms,
            &mut w,
        );
        c.monitor.summary_interval_ms = c.monitor.summary_interval_ms.max(250);
        read_bool(
            &ini,
            "monitor",
            "capture_on_start",
            &mut c.monitor.capture_on_start,
            &mut w,
        );
        read_f32(
            &ini,
            "monitor",
            "spike_factor",
            &mut c.monitor.spike_factor,
            &mut w,
        );
        read_f32(
            &ini,
            "monitor",
            "spike_min_ms",
            &mut c.monitor.spike_min_ms,
            &mut w,
        );
        read_bool(
            &ini,
            "monitor",
            "log_spikes",
            &mut c.monitor.log_spikes,
            &mut w,
        );

        let s = &mut c.state_filter;
        read_bool(&ini, "state_filter", "enabled", &mut s.enabled, &mut w);
        read_bool(&ini, "state_filter", "filter", &mut s.filter, &mut w);
        read_bool(
            &ini,
            "state_filter",
            "render_states",
            &mut s.render_states,
            &mut w,
        );
        read_bool(
            &ini,
            "state_filter",
            "sampler_states",
            &mut s.sampler_states,
            &mut w,
        );
        read_bool(
            &ini,
            "state_filter",
            "texture_stage_states",
            &mut s.texture_stage_states,
            &mut w,
        );
        read_u32(
            &ini,
            "state_filter",
            "ab_interval_s",
            &mut s.ab_interval_s,
            &mut w,
        );
        read_bool(&ini, "state_filter", "validate", &mut s.validate, &mut w);

        read_bool(&ini, "resources", "track", &mut c.resources.track, &mut w);

        let h = &mut c.hotkeys;
        read_bool(&ini, "hotkeys", "enabled", &mut h.enabled, &mut w);
        read_bool(
            &ini,
            "hotkeys",
            "require_ctrl_shift",
            &mut h.require_ctrl_shift,
            &mut w,
        );
        read_u32(
            &ini,
            "hotkeys",
            "toggle_filter",
            &mut h.toggle_filter,
            &mut w,
        );
        read_u32(&ini, "hotkeys", "dump_summary", &mut h.dump_summary, &mut w);
        read_u32(
            &ini,
            "hotkeys",
            "toggle_capture",
            &mut h.toggle_capture,
            &mut w,
        );

        read_bool(
            &ini,
            "compat",
            "late_attach",
            &mut c.compat.late_attach,
            &mut w,
        );
        read_bool(
            &ini,
            "compat",
            "allow_unknown_exe",
            &mut c.compat.allow_unknown_exe,
            &mut w,
        );

        if c.state_filter.filter && !c.state_filter.enabled {
            w.push("state_filter.filter=1 has no effect while state_filter.enabled=0".into());
        }
        (c, w)
    }

    /// True if any feature needs `Present`/`Reset` hooks.
    pub fn needs_frame_hooks(&self) -> bool {
        self.monitor.level >= MonitorLevel::FrameTime
            || self.state_filter.enabled
            || self.resources.track
    }
}

fn read_bool(ini: &Ini, s: &str, k: &str, out: &mut bool, w: &mut Vec<String>) {
    if let Some(v) = ini.get(s, k) {
        match parse_bool(v) {
            Some(b) => *out = b,
            None => w.push(format!("{s}.{k}: expected boolean, got '{v}'")),
        }
    }
}

fn read_u32(ini: &Ini, s: &str, k: &str, out: &mut u32, w: &mut Vec<String>) {
    if let Some(v) = ini.get(s, k) {
        match parse_u32(v) {
            Some(n) => *out = n,
            None => w.push(format!("{s}.{k}: expected integer, got '{v}'")),
        }
    }
}

fn read_f32(ini: &Ini, s: &str, k: &str, out: &mut f32, w: &mut Vec<String>) {
    if let Some(v) = ini.get(s, k) {
        match v.parse::<f32>() {
            Ok(n) if n.is_finite() && n >= 0.0 => *out = n,
            _ => w.push(format!("{s}.{k}: expected non-negative number, got '{v}'")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_safe() {
        let c = Config::default();
        assert!(!c.state_filter.enabled, "optimizations must default to off");
        assert!(!c.state_filter.filter);
        assert!(!c.resources.track);
        assert_eq!(c.monitor.level, MonitorLevel::FrameTime);
    }

    #[test]
    fn parses_values_and_reports_bad_ones() {
        let (c, w) = Config::from_ini_text(
            "[monitor]\nlevel=2\nspike_factor=3\n[state_filter]\nenabled=1\nfilter=on\nab_interval_s=10\n[hotkeys]\ntoggle_filter=0x78\n[resources]\ntrack=maybe\n",
        );
        assert_eq!(c.monitor.level, MonitorLevel::Full);
        assert_eq!(c.monitor.spike_factor, 3.0);
        assert!(c.state_filter.enabled && c.state_filter.filter);
        assert_eq!(c.state_filter.ab_interval_s, 10);
        assert_eq!(c.hotkeys.toggle_filter, 0x78);
        assert!(!c.resources.track);
        assert_eq!(w.len(), 1, "{w:?}");
    }

    #[test]
    fn summary_interval_is_clamped() {
        let (c, _) = Config::from_ini_text("[monitor]\nsummary_interval_ms=1");
        assert_eq!(c.monitor.summary_interval_ms, 250);
    }

    #[test]
    fn frame_hooks_needed() {
        let (c, _) = Config::from_ini_text("[monitor]\nlevel=0");
        assert!(!c.needs_frame_hooks());
        let (c, _) = Config::from_ini_text("[monitor]\nlevel=0\n[resources]\ntrack=1");
        assert!(c.needs_frame_hooks());
    }
}
