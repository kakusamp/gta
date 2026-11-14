//! renderware_hook foundation: game executable identification and the table of
//! known in-game addresses.
//!
//! Milestone 1 performs **read-only** access to game memory, and only after
//! validating that the address is mapped and readable. No game code is patched.
//! Every address below carries its source and confidence; nothing is used on an
//! executable whose version signature does not match.

use crate::{hook, win32};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameVersion {
    Us10,
    Eu10,
    Us11,
    Eu11,
    Steam,
    Unknown,
}

impl GameVersion {
    pub fn name(self) -> &'static str {
        match self {
            GameVersion::Us10 => "1.0 US",
            GameVersion::Eu10 => "1.0 EU",
            GameVersion::Us11 => "1.01 US",
            GameVersion::Eu11 => "1.01 EU",
            GameVersion::Steam => "Steam (3.0)",
            GameVersion::Unknown => "unknown",
        }
    }
}

/// Version signatures: the dword at `addr` equals `0x94BF` in the given exe.
///
/// Source: community version-detection code used by plugin-sdk / CLEO-era
/// tooling. Confidence: COMMUNITY-DOCUMENTED (widely used, not re-derived by us).
/// Verification: SAPE logs which signature matched plus the exe's PE
/// TimeDateStamp and SizeOfImage so mismatches can be reported.
const VERSION_SIGNATURES: &[(usize, u32, GameVersion)] = &[
    (0x0082_457C, 0x94BF, GameVersion::Us10),
    (0x0082_45BC, 0x94BF, GameVersion::Eu10),
    (0x0082_52FC, 0x94BF, GameVersion::Us11),
    (0x0082_533C, 0x94BF, GameVersion::Eu11),
    (0x0085_EC4A, 0x94BF, GameVersion::Steam),
];

/// Known addresses (gta_sa.exe 1.0 US).
pub mod addr_us10 {
    /// `IDirect3DDevice9*` used by RenderWare's D3D9 driver
    /// (what `RwD3D9GetCurrentD3DDevice()` returns).
    /// Source: plugin-sdk (`RwD3D9Device`), MTA:SA. Confidence: COMMUNITY-DOCUMENTED.
    /// Use: read-only late attach; the value is validated as a COM object whose
    /// vtable lies in a loaded image with executable entries before any use.
    pub const RW_D3D9_DEVICE: usize = 0x00C9_7C28;
}

#[derive(Debug, Clone)]
pub struct GameInfo {
    pub version: GameVersion,
    pub exe_name: String,
    pub image_base: usize,
    pub image_size: u32,
    pub timestamp: u32,
    pub large_address_aware: bool,
}

pub fn detect() -> GameInfo {
    let exe = win32::module_handle_null() as usize;
    let image_size = hook::pe_optional_u32(exe, 56).unwrap_or(0); // SizeOfImage
    let timestamp = hook::pe_file_header_u32(exe, 4).unwrap_or(0); // TimeDateStamp
    let characteristics = hook::pe_file_header_u32(exe, 18).unwrap_or(0) & 0xFFFF;
    let in_image = |a: usize| a >= exe && a + 4 <= exe + image_size as usize;

    let version = VERSION_SIGNATURES
        .iter()
        .find(|(a, v, _)| in_image(*a) && win32::read_u32_checked(*a) == Some(*v))
        .map(|&(_, _, ver)| ver)
        .unwrap_or(GameVersion::Unknown);

    let exe_name = win32::module_path(exe as win32::HMODULE)
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_default();

    GameInfo {
        version,
        exe_name,
        image_base: exe,
        image_size,
        timestamp,
        large_address_aware: characteristics & 0x20 != 0, // IMAGE_FILE_LARGE_ADDRESS_AWARE
    }
}

/// Address of RenderWare's device pointer for this version, if known.
pub fn rw_device_ptr(v: GameVersion) -> Option<usize> {
    match v {
        GameVersion::Us10 => Some(addr_us10::RW_D3D9_DEVICE),
        _ => None,
    }
}
