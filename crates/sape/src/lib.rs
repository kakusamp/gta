//! SAPE - San Andreas Performance Engine.
//!
//! 32-bit DLL / ASI plugin for GTA San Andreas (and SA-MP). Milestone 1:
//! measurement infrastructure plus one opt-in, validated optimization
//! (redundant D3D9 state filtering). See `docs/` for research and architecture.
//!
//! Load order: DllMain only parses config, opens the log, identifies the game
//! and patches one IAT slot. All further work happens lazily on the game's
//! own D3D9 calls, outside the loader lock.

#![cfg(windows)]

pub mod d3d9;
pub mod game;
pub mod hook;
pub mod log;
pub mod monitor;
pub mod proxy;
pub mod render_optimizer;
pub mod resources;
pub mod samp;
pub mod win32;

use sape_core::config::Config;
use std::path::PathBuf;
use std::sync::OnceLock;

pub struct Runtime {
    pub config: Config,
    pub dir: PathBuf,
    pub game: game::GameInfo,
}

static RUNTIME: OnceLock<Runtime> = OnceLock::new();

pub fn runtime() -> Option<&'static Runtime> {
    RUNTIME.get()
}

const DEFAULT_INI: &str = include_str!("../../../sape.ini");

/// # Safety
/// Called by the Windows loader.
#[no_mangle]
#[allow(non_snake_case)]
pub unsafe extern "system" fn DllMain(
    module: win32::HMODULE,
    reason: u32,
    _reserved: *mut core::ffi::c_void,
) -> win32::BOOL {
    if reason == win32::DLL_PROCESS_ATTACH {
        win32::DisableThreadLibraryCalls(module);
        init(module);
    }
    // On detach we deliberately do not unhook: at process exit d3d9.dll may
    // already be gone, and ASI plugins are never unloaded mid-session.
    1
}

fn init(module: win32::HMODULE) {
    let dll_path = win32::module_path(module).unwrap_or_else(|| PathBuf::from("sape.dll"));
    let dir = dll_path.parent().map(PathBuf::from).unwrap_or_default();
    let stem = dll_path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "sape".into());
    // Installed as d3d9.dll -> proxy mode. Config/log keep the name "sape" so
    // they never collide with other d3d9 wrappers' d3d9.ini / d3d9.log.
    let proxy_mode = stem.eq_ignore_ascii_case("d3d9");
    let stem = if proxy_mode { "sape".to_string() } else { stem };
    let ini_path = dir.join(format!("{stem}.ini"));

    let (text, created) = match std::fs::read_to_string(&ini_path) {
        Ok(t) => (t, false),
        Err(_) => {
            let _ = std::fs::write(&ini_path, DEFAULT_INI);
            (DEFAULT_INI.to_string(), true)
        }
    };
    let (config, warnings) = Config::from_ini_text(&text);
    // Forwarding must work even if SAPE later decides to stay inactive.
    proxy::configure(module, &dir, &config.proxy.chain);

    log::init(&dir.join(format!("{stem}.log")), config.log_level);
    log_info!(
        "SAPE {} ({}) loaded from {}",
        sape_core::VERSION,
        std::env::consts::ARCH,
        dll_path.display()
    );
    if created {
        log_info!("wrote default configuration to {}", ini_path.display());
    }
    for w in &warnings {
        log_warn!("config: {w}");
    }
    if proxy_mode {
        log_info!(
            "mode: d3d9.dll proxy (forwarding to {})",
            if config.proxy.chain.is_empty() {
                "system d3d9.dll"
            } else {
                &config.proxy.chain
            }
        );
    } else {
        log_info!("mode: plugin (ASI / LoadLibrary), hooking via the executable's import table");
    }

    let game = game::detect();
    log_info!(
        "executable: {} | version {} | base {:#x} size {:#x} timestamp {:#x} | large-address-aware: {}",
        game.exe_name,
        game.version.name(),
        game.image_base,
        game.image_size,
        game.timestamp,
        game.large_address_aware
    );
    if let Some((used, total)) = monitor::address_space() {
        log_info!(
            "virtual address space: {} MB used of {} MB",
            used >> 20,
            total >> 20
        );
    }
    samp::poll_once();

    if game.version == game::GameVersion::Unknown && !config.compat.allow_unknown_exe {
        log_warn!("unrecognised executable and compat.allow_unknown_exe=0: SAPE stays inactive");
        return;
    }

    render_optimizer::configure(&config.state_filter, config.state_filter.validate);
    if config.state_filter.enabled {
        log_info!(
            "state filter hooks enabled; filtering {} at start (rs={} samp={} tss={} validate={})",
            if config.state_filter.filter {
                "ON"
            } else {
                "OFF (analysis only)"
            },
            config.state_filter.render_states,
            config.state_filter.sampler_states,
            config.state_filter.texture_stage_states,
            config.state_filter.validate
        );
    }

    let needs_hooks = config.needs_frame_hooks();
    let late_attach = config.compat.late_attach;
    let version = game.version;
    if RUNTIME.set(Runtime { config, dir, game }).is_err() {
        return; // already initialised (should not happen)
    }
    if !needs_hooks {
        log_info!("all features disabled in config: no hooks installed");
        return;
    }

    if proxy_mode {
        return; // our exported Direct3DCreate9 installs the hooks
    }
    d3d9::install_iat_hook();
    if late_attach {
        if let Some(addr) = game::rw_device_ptr(version) {
            if d3d9::late_attach(addr) {
                log_info!("late attach succeeded (device existed before SAPE loaded)");
            }
        }
    }
}
