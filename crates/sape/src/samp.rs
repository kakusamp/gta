//! samp_compatibility: detection only.
//!
//! SAPE never touches SA-MP memory, network code or synchronization. It only
//! identifies the loaded samp.dll so benchmark logs record which client was
//! running and so later modules can refuse to enable features that were not
//! validated against that client build.

use crate::{hook, win32};
use core::sync::atomic::{AtomicBool, Ordering::*};

/// samp.dll entry-point RVA -> client build.
/// Source: community SA-MP client tooling (e.g. SAMP-API version detection).
/// Confidence: COMMUNITY-DOCUMENTED. Used only for a log label.
const KNOWN_BUILDS: &[(u32, &str)] = &[
    (0x0031_DF13, "0.3.7-R1"),
    (0x0031_95DD, "0.3.7-R2"),
    (0x000C_C490, "0.3.7-R3"),
    (0x000C_C4D0, "0.3.7-R3-1"),
    (0x000C_BCB0, "0.3.7-R4"),
    (0x000F_DB60, "0.3.DL-R1"),
];

static REPORTED: AtomicBool = AtomicBool::new(false);
pub static PRESENT: AtomicBool = AtomicBool::new(false);

/// Logs the SA-MP client once, as soon as it is loaded.
pub fn poll_once() {
    if REPORTED.load(Relaxed) {
        return;
    }
    let h = win32::module_handle("samp.dll");
    if h.is_null() {
        return;
    }
    if REPORTED.swap(true, AcqRel) {
        return;
    }
    PRESENT.store(true, Relaxed);
    let base = h as usize;
    let entry = hook::pe_optional_u32(base, 16).unwrap_or(0); // AddressOfEntryPoint
    let size = hook::pe_optional_u32(base, 56).unwrap_or(0); // SizeOfImage
    let ts = hook::pe_file_header_u32(base, 4).unwrap_or(0);
    let build = KNOWN_BUILDS
        .iter()
        .find(|(e, _)| *e == entry)
        .map(|(_, n)| *n)
        .unwrap_or("unknown build");
    crate::log_info!(
        "SA-MP client detected: {build} (base {base:#x}, entry RVA {entry:#x}, size {size:#x}, timestamp {ts:#x}). SAPE does not modify SA-MP."
    );
}
