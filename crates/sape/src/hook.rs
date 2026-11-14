//! Hooking primitives: pointer-slot patching for COM vtables and the import
//! address table (IAT).
//!
//! SAPE deliberately uses *no inline/trampoline detours* in this milestone:
//! pointer-slot patching needs no instruction decoding, is atomic on x86
//! (aligned 4-byte store), chains naturally with other mods that hooked the
//! same slot earlier, and cannot corrupt code bytes.

use crate::win32::{self, PAGE_EXECUTE_READWRITE};
use core::ffi::c_void;
use core::sync::atomic::{AtomicUsize, Ordering};

#[derive(Debug)]
pub enum PatchError {
    Unreadable,
    ProtectFailed,
}

/// Atomically replaces the function pointer in `slot` with `new`, storing the
/// previous value in `orig` **before** the new pointer becomes visible, so that
/// a concurrent call through the slot can always forward to the original.
///
/// Idempotent: if the slot already contains `new`, nothing is changed.
///
/// # Safety
/// `slot` must point to a function-pointer slot (vtable entry / IAT entry) whose
/// signature matches the hook stored in `new`.
pub unsafe fn patch_slot(
    slot: *mut usize,
    new: usize,
    orig: &AtomicUsize,
) -> Result<(), PatchError> {
    let addr = slot as usize;
    if !win32::is_readable(addr, core::mem::size_of::<usize>()) {
        return Err(PatchError::Unreadable);
    }
    let mut old_protect = 0u32;
    // EXECUTE_READWRITE (not READWRITE): if the slot shares a page with code,
    // removing execute permission would crash any thread running there.
    if win32::VirtualProtect(
        slot as *mut c_void,
        core::mem::size_of::<usize>(),
        PAGE_EXECUTE_READWRITE,
        &mut old_protect,
    ) == 0
    {
        return Err(PatchError::ProtectFailed);
    }
    // SAFETY: slot is aligned (vtables / IATs are pointer-aligned) and now writable.
    let atomic_slot = &*(slot as *const AtomicUsize);
    let mut current = atomic_slot.load(Ordering::Acquire);
    if current != new {
        loop {
            orig.store(current, Ordering::Release);
            match atomic_slot.compare_exchange(current, new, Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => break,
                Err(now) if now == new => break, // someone installed our hook concurrently
                Err(now) => current = now,
            }
        }
    }
    let mut ignored = 0u32;
    win32::VirtualProtect(
        slot as *mut c_void,
        core::mem::size_of::<usize>(),
        old_protect,
        &mut ignored,
    );
    Ok(())
}

/// Restores `orig` into `slot` if the slot still contains our hook (`ours`).
/// If another mod chained on top of us, the slot is left alone (unhooking would
/// break their chain).
///
/// # Safety
/// Same requirements as [`patch_slot`].
pub unsafe fn unpatch_slot(slot: *mut usize, ours: usize, orig: &AtomicUsize) -> bool {
    let o = orig.load(Ordering::Acquire);
    if o == 0 || !win32::is_readable(slot as usize, core::mem::size_of::<usize>()) {
        return false;
    }
    let mut old_protect = 0u32;
    if win32::VirtualProtect(
        slot as *mut c_void,
        core::mem::size_of::<usize>(),
        PAGE_EXECUTE_READWRITE,
        &mut old_protect,
    ) == 0
    {
        return false;
    }
    let ok = (*(slot as *const AtomicUsize))
        .compare_exchange(ours, o, Ordering::AcqRel, Ordering::Acquire)
        .is_ok();
    let mut ignored = 0u32;
    win32::VirtualProtect(
        slot as *mut c_void,
        core::mem::size_of::<usize>(),
        old_protect,
        &mut ignored,
    );
    ok
}

/// Validates that `vtable` looks like a real COM vtable with `entries` slots:
/// readable, inside a mapped image (DLL/EXE), and every entry points into
/// executable memory.
pub fn validate_vtable(vtable: usize, entries: usize) -> bool {
    if !win32::is_readable(vtable, entries * core::mem::size_of::<usize>()) {
        return false;
    }
    match win32::region_info(vtable) {
        Some((_, ty)) if ty == win32::MEM_IMAGE => {}
        _ => return false,
    }
    (0..entries).all(|i| {
        // SAFETY: range validated readable above.
        let f = unsafe { *(vtable as *const usize).add(i) };
        matches!(win32::region_info(f), Some((p, _)) if p & win32::PAGE_EXECUTABLE_MASK != 0)
    })
}

// ---------------------------------------------------------------------------
// IAT patching
// ---------------------------------------------------------------------------

/// Finds the IAT slot through which `module` imports `func` from `dll`.
///
/// Walks the PE import directory. Matching is by name via the import name
/// table (OriginalFirstThunk); if the INT is absent (some packed/bound images),
/// falls back to comparing IAT values with the export address of `dll!func`.
///
/// # Safety
/// `module` must be the base address of a mapped PE image.
pub unsafe fn find_iat_slot(module: usize, dll: &str, func: &str) -> Option<*mut usize> {
    let rd16 = |off: usize| win32::read_u32_checked(module + off).map(|v| v as u16);
    let rd32 = |off: usize| win32::read_u32_checked(module + off);

    if rd16(0)? != 0x5A4D {
        return None; // "MZ"
    }
    let nt = rd32(0x3C)? as usize;
    if rd32(nt)? != 0x0000_4550 {
        return None; // "PE\0\0"
    }
    let opt = nt + 24;
    let magic = rd16(opt)?;
    let (dir_off, thunk_size) = match magic {
        0x10B => (96usize, 4usize),  // PE32
        0x20B => (112usize, 8usize), // PE32+
        _ => return None,
    };
    if thunk_size != core::mem::size_of::<usize>() {
        return None; // image bitness differs from ours
    }
    let import_rva = rd32(opt + dir_off + 8)? as usize; // DataDirectory[1]
    if import_rva == 0 {
        return None;
    }

    let export_addr = {
        let h = win32::module_handle(dll);
        if h.is_null() {
            0
        } else {
            let mut name = func.as_bytes().to_vec();
            name.push(0);
            win32::GetProcAddress(h, name.as_ptr()) as usize
        }
    };

    let mut desc = module + import_rva;
    loop {
        let oft = win32::read_u32_checked(desc)? as usize;
        let name_rva = win32::read_u32_checked(desc + 12)? as usize;
        let ft = win32::read_u32_checked(desc + 16)? as usize;
        if name_rva == 0 && ft == 0 {
            return None;
        }
        if read_cstr(module + name_rva).is_some_and(|n| n.eq_ignore_ascii_case(dll)) {
            for i in 0.. {
                let iat_slot = module + ft + i * thunk_size;
                let iat_val = win32::read_usize_checked(iat_slot)?;
                if iat_val == 0 {
                    break;
                }
                let by_name = if oft != 0 {
                    let thunk = win32::read_usize_checked(module + oft + i * thunk_size)?;
                    let ordinal_flag = 1usize << (thunk_size * 8 - 1);
                    thunk & ordinal_flag == 0
                        && read_cstr(module + thunk + 2).is_some_and(|n| n == func)
                } else {
                    false
                };
                if by_name || (export_addr != 0 && iat_val == export_addr) {
                    return Some(iat_slot as *mut usize);
                }
            }
        }
        desc += 20; // sizeof(IMAGE_IMPORT_DESCRIPTOR)
    }
}

fn read_cstr(addr: usize) -> Option<String> {
    let mut out = Vec::new();
    for i in 0..256 {
        let b = if win32::is_readable(addr + i, 1) {
            // SAFETY: validated readable.
            unsafe { *((addr + i) as *const u8) }
        } else {
            return None;
        };
        if b == 0 {
            return String::from_utf8(out).ok();
        }
        out.push(b);
    }
    None
}

/// Reads a PE header field of a loaded module (offset relative to the optional header).
pub fn pe_optional_u32(module: usize, offset: usize) -> Option<u32> {
    let nt = win32::read_u32_checked(module + 0x3C)? as usize;
    win32::read_u32_checked(module + nt + 24 + offset)
}

/// IMAGE_FILE_HEADER field at `offset` (u16 fields read as low half).
pub fn pe_file_header_u32(module: usize, offset: usize) -> Option<u32> {
    let nt = win32::read_u32_checked(module + 0x3C)? as usize;
    win32::read_u32_checked(module + nt + 4 + offset)
}
