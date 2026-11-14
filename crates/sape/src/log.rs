//! logging_system: synchronous, line-buffered file log (`sape.log` next to the DLL).
//!
//! Hot paths (per-draw / per-state hooks) never log. Per-frame code logs only on
//! rare events (spikes, resets, hotkeys) and the periodic summary.

use sape_core::config::LogLevel;
use std::fmt;
use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

static FILE: Mutex<Option<File>> = Mutex::new(None);
static LEVEL: AtomicU8 = AtomicU8::new(LogLevel::Info as u8);
static START: OnceLock<Instant> = OnceLock::new();

pub fn init(path: &Path, level: LogLevel) {
    START.get_or_init(Instant::now);
    LEVEL.store(level as u8, Ordering::Relaxed);
    if let Ok(f) = File::create(path) {
        if let Ok(mut g) = FILE.lock() {
            *g = Some(f);
        }
    }
}

pub fn set_level(level: LogLevel) {
    LEVEL.store(level as u8, Ordering::Relaxed);
}

pub fn enabled(level: LogLevel) -> bool {
    level as u8 <= LEVEL.load(Ordering::Relaxed)
}

pub fn write(level: LogLevel, args: fmt::Arguments) {
    if !enabled(level) {
        return;
    }
    let t = START
        .get()
        .map(|s| s.elapsed().as_secs_f64())
        .unwrap_or(0.0);
    let tag = match level {
        LogLevel::Error => "ERROR",
        LogLevel::Warn => "WARN ",
        LogLevel::Info => "INFO ",
        LogLevel::Debug => "DEBUG",
    };
    let line = format!("[{t:10.3}] {tag} {args}\n");
    // Poisoning can only happen if a writer panicked; with panic=abort it cannot.
    if let Ok(mut g) = FILE.lock() {
        if let Some(f) = g.as_mut() {
            let _ = f.write_all(line.as_bytes());
        }
    }
    if level == LogLevel::Error {
        let mut c = line.into_bytes();
        c.push(0);
        // SAFETY: null-terminated buffer.
        unsafe { crate::win32::OutputDebugStringA(c.as_ptr()) };
    }
}

#[macro_export]
macro_rules! log_error { ($($t:tt)*) => { $crate::log::write(sape_core::config::LogLevel::Error, format_args!($($t)*)) } }
#[macro_export]
macro_rules! log_warn { ($($t:tt)*) => { $crate::log::write(sape_core::config::LogLevel::Warn, format_args!($($t)*)) } }
#[macro_export]
macro_rules! log_info { ($($t:tt)*) => { $crate::log::write(sape_core::config::LogLevel::Info, format_args!($($t)*)) } }
#[macro_export]
macro_rules! log_debug { ($($t:tt)*) => { $crate::log::write(sape_core::config::LogLevel::Debug, format_args!($($t)*)) } }
