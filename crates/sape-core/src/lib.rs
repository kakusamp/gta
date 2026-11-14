//! Platform-independent core of SAPE (San Andreas Performance Engine).
//!
//! Everything in this crate is plain, safe Rust with no Windows or game
//! dependencies, so it can be unit-tested on any host. The DLL crate (`sape`)
//! wires these pieces into Direct3D 9 / GTA SA hooks.

pub mod config;
pub mod ini;
pub mod state_cache;
pub mod stats;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
