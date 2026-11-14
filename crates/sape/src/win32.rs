//! Minimal hand-written Win32 FFI. Only what SAPE actually uses, so the DLL has
//! no third-party dependencies. Signatures follow the Windows SDK headers
//! (`memoryapi.h`, `libloaderapi.h`, `processthreadsapi.h`, `psapi.h`,
//! `sysinfoapi.h`, `winuser.h`). `extern "system"` is `__stdcall` on x86.

#![allow(non_snake_case, non_camel_case_types, clippy::upper_case_acronyms)]

use core::ffi::c_void;

pub type BOOL = i32;
pub type HANDLE = *mut c_void;
pub type HMODULE = *mut c_void;
pub type HWND = *mut c_void;

pub const DLL_PROCESS_ATTACH: u32 = 1;
pub const DLL_PROCESS_DETACH: u32 = 0;

pub const MEM_COMMIT: u32 = 0x1000;
pub const MEM_IMAGE: u32 = 0x0100_0000;
pub const PAGE_NOACCESS: u32 = 0x01;
pub const PAGE_GUARD: u32 = 0x100;
pub const PAGE_EXECUTE_READWRITE: u32 = 0x40;
/// READONLY | READWRITE | WRITECOPY | EXECUTE_READ | EXECUTE_READWRITE | EXECUTE_WRITECOPY
pub const PAGE_READABLE_MASK: u32 = 0x02 | 0x04 | 0x08 | 0x20 | 0x40 | 0x80;
pub const PAGE_EXECUTABLE_MASK: u32 = 0x10 | 0x20 | 0x40 | 0x80;

pub const VK_SHIFT: i32 = 0x10;
pub const VK_CONTROL: i32 = 0x11;

#[repr(C)]
#[derive(Default)]
pub struct MEMORY_BASIC_INFORMATION {
    pub BaseAddress: usize,
    pub AllocationBase: usize,
    pub AllocationProtect: u32,
    #[cfg(target_pointer_width = "64")]
    pub PartitionId: u16,
    pub RegionSize: usize,
    pub State: u32,
    pub Protect: u32,
    pub Type: u32,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct FILETIME {
    pub dwLowDateTime: u32,
    pub dwHighDateTime: u32,
}

impl FILETIME {
    pub fn as_u64(self) -> u64 {
        ((self.dwHighDateTime as u64) << 32) | self.dwLowDateTime as u64
    }
}

#[repr(C)]
#[derive(Default)]
pub struct PROCESS_MEMORY_COUNTERS {
    pub cb: u32,
    pub PageFaultCount: u32,
    pub PeakWorkingSetSize: usize,
    pub WorkingSetSize: usize,
    pub QuotaPeakPagedPoolUsage: usize,
    pub QuotaPagedPoolUsage: usize,
    pub QuotaPeakNonPagedPoolUsage: usize,
    pub QuotaNonPagedPoolUsage: usize,
    /// "Commit charge" / private bytes of the process.
    pub PagefileUsage: usize,
    pub PeakPagefileUsage: usize,
}

#[repr(C)]
#[derive(Default)]
pub struct MEMORYSTATUSEX {
    pub dwLength: u32,
    pub dwMemoryLoad: u32,
    pub ullTotalPhys: u64,
    pub ullAvailPhys: u64,
    pub ullTotalPageFile: u64,
    pub ullAvailPageFile: u64,
    /// User-mode virtual address space of *this* process (2 GB, or 4 GB for a
    /// large-address-aware 32-bit exe on 64-bit Windows).
    pub ullTotalVirtual: u64,
    pub ullAvailVirtual: u64,
    pub ullAvailExtendedVirtual: u64,
}

#[link(name = "kernel32")]
extern "system" {
    pub fn VirtualProtect(
        addr: *mut c_void,
        size: usize,
        new_protect: u32,
        old_protect: *mut u32,
    ) -> BOOL;
    pub fn VirtualQuery(
        addr: *const c_void,
        buf: *mut MEMORY_BASIC_INFORMATION,
        len: usize,
    ) -> usize;
    pub fn GetModuleHandleW(name: *const u16) -> HMODULE;
    pub fn GetModuleFileNameW(module: HMODULE, buf: *mut u16, size: u32) -> u32;
    pub fn GetProcAddress(module: HMODULE, name: *const u8) -> *mut c_void;
    pub fn DisableThreadLibraryCalls(module: HMODULE) -> BOOL;
    pub fn GetCurrentThreadId() -> u32;
    pub fn GetCurrentProcessId() -> u32;
    pub fn GetCurrentProcess() -> HANDLE;
    pub fn GetProcessTimes(
        process: HANDLE,
        creation: *mut FILETIME,
        exit: *mut FILETIME,
        kernel: *mut FILETIME,
        user: *mut FILETIME,
    ) -> BOOL;
    pub fn K32GetProcessMemoryInfo(
        process: HANDLE,
        counters: *mut PROCESS_MEMORY_COUNTERS,
        cb: u32,
    ) -> BOOL;
    pub fn GlobalMemoryStatusEx(buf: *mut MEMORYSTATUSEX) -> BOOL;
    pub fn OutputDebugStringA(s: *const u8);
}

#[link(name = "user32")]
extern "system" {
    pub fn GetAsyncKeyState(vk: i32) -> i16;
    pub fn GetForegroundWindow() -> HWND;
    pub fn GetWindowThreadProcessId(hwnd: HWND, pid: *mut u32) -> u32;
}

/// Null-terminated UTF-16 string.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(core::iter::once(0)).collect()
}

/// True if `[addr, addr+len)` is committed, readable, non-guard memory in a single region.
pub fn is_readable(addr: usize, len: usize) -> bool {
    if addr == 0 {
        return false;
    }
    let mut mbi = MEMORY_BASIC_INFORMATION::default();
    // SAFETY: VirtualQuery only writes into `mbi`; any address is a valid query.
    let n = unsafe {
        VirtualQuery(
            addr as *const c_void,
            &mut mbi,
            core::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
        )
    };
    if n == 0 || mbi.State != MEM_COMMIT || mbi.Protect & (PAGE_GUARD | PAGE_NOACCESS) != 0 {
        return false;
    }
    if mbi.Protect & PAGE_READABLE_MASK == 0 {
        return false;
    }
    let end = mbi.BaseAddress.saturating_add(mbi.RegionSize);
    addr.checked_add(len).is_some_and(|e| e <= end)
}

/// Returns (protect, type) of the region containing `addr`.
pub fn region_info(addr: usize) -> Option<(u32, u32)> {
    let mut mbi = MEMORY_BASIC_INFORMATION::default();
    // SAFETY: as above.
    let n = unsafe {
        VirtualQuery(
            addr as *const c_void,
            &mut mbi,
            core::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
        )
    };
    (n != 0 && mbi.State == MEM_COMMIT).then_some((mbi.Protect, mbi.Type))
}

/// Reads a `u32` after validating the address. Never faults.
pub fn read_u32_checked(addr: usize) -> Option<u32> {
    if is_readable(addr, 4) {
        // SAFETY: range validated as committed + readable. Unaligned reads are fine on x86.
        Some(unsafe { core::ptr::read_unaligned(addr as *const u32) })
    } else {
        None
    }
}

pub fn read_usize_checked(addr: usize) -> Option<usize> {
    if is_readable(addr, core::mem::size_of::<usize>()) {
        // SAFETY: as above.
        Some(unsafe { core::ptr::read_unaligned(addr as *const usize) })
    } else {
        None
    }
}

pub fn module_handle(name: &str) -> HMODULE {
    let w = wide(name);
    // SAFETY: `w` is a valid null-terminated wide string.
    unsafe { GetModuleHandleW(w.as_ptr()) }
}

/// Base address of the main executable.
pub fn module_handle_null() -> HMODULE {
    // SAFETY: NULL requests the calling process's executable.
    unsafe { GetModuleHandleW(core::ptr::null()) }
}

// HMODULE is an opaque handle; an invalid one makes the API fail, nothing is dereferenced.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub fn module_path(module: HMODULE) -> Option<std::path::PathBuf> {
    let mut buf = vec![0u16; 1024];
    // SAFETY: buffer length passed correctly.
    let n = unsafe { GetModuleFileNameW(module, buf.as_mut_ptr(), buf.len() as u32) } as usize;
    if n == 0 || n >= buf.len() {
        return None;
    }
    Some(std::path::PathBuf::from(String::from_utf16_lossy(
        &buf[..n],
    )))
}

pub fn current_thread_id() -> u32 {
    // SAFETY: no preconditions.
    unsafe { GetCurrentThreadId() }
}

/// True if the foreground window belongs to this process (hotkeys are global otherwise).
pub fn process_has_focus() -> bool {
    // SAFETY: plain user32 queries with valid out-pointer.
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_null() {
            return false;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, &mut pid);
        pid == GetCurrentProcessId()
    }
}

pub fn key_down(vk: i32) -> bool {
    // SAFETY: no preconditions.
    unsafe { GetAsyncKeyState(vk) as u16 & 0x8000 != 0 }
}
