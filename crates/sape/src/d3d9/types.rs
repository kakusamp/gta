//! Direct3D 9 ABI definitions used by SAPE.
//!
//! Vtable indices are the 0-based declaration order of the methods in the
//! Windows SDK `d3d9.h` (`DECLARE_INTERFACE_(IDirect3DDevice9, IUnknown)`),
//! including the three IUnknown methods. They are part of the COM ABI and are
//! identical for every d3d9.dll (Microsoft runtime, DXVK, wrappers).
//! Only the entries SAPE uses are listed.

#![allow(non_camel_case_types, non_snake_case, dead_code)]

use core::ffi::c_void;

pub type HRESULT = i32;
pub const D3D_OK: HRESULT = 0;

pub type Device = c_void;

// --- IDirect3D9 -------------------------------------------------------------
pub mod d3d9_vt {
    pub const CREATE_DEVICE: usize = 16;
    pub const COUNT: usize = 17;
}

// --- IDirect3DDevice9 -------------------------------------------------------
pub mod dev_vt {
    pub const GET_AVAILABLE_TEXTURE_MEM: usize = 4;
    pub const RESET: usize = 16;
    pub const PRESENT: usize = 17;
    pub const CREATE_TEXTURE: usize = 23;
    pub const CREATE_VERTEX_BUFFER: usize = 26;
    pub const CREATE_INDEX_BUFFER: usize = 27;
    pub const SET_RENDER_STATE: usize = 57;
    pub const GET_RENDER_STATE: usize = 58;
    pub const CREATE_STATE_BLOCK: usize = 59;
    pub const BEGIN_STATE_BLOCK: usize = 60;
    pub const END_STATE_BLOCK: usize = 61;
    pub const SET_TEXTURE: usize = 65;
    pub const GET_TEXTURE_STAGE_STATE: usize = 66;
    pub const SET_TEXTURE_STAGE_STATE: usize = 67;
    pub const GET_SAMPLER_STATE: usize = 68;
    pub const SET_SAMPLER_STATE: usize = 69;
    pub const DRAW_PRIMITIVE: usize = 81;
    pub const DRAW_INDEXED_PRIMITIVE: usize = 82;
    pub const DRAW_PRIMITIVE_UP: usize = 83;
    pub const DRAW_INDEXED_PRIMITIVE_UP: usize = 84;
    pub const SET_VERTEX_DECLARATION: usize = 87;
    pub const SET_FVF: usize = 89;
    pub const SET_VERTEX_SHADER: usize = 92;
    pub const SET_STREAM_SOURCE: usize = 100;
    pub const SET_PIXEL_SHADER: usize = 107;
    /// Total number of methods in IDirect3DDevice9 (last is CreateQuery = 118).
    pub const COUNT: usize = 119;
}

// --- IDirect3DStateBlock9 ---------------------------------------------------
pub mod sb_vt {
    pub const APPLY: usize = 5;
    pub const COUNT: usize = 6;
}

// --- IDirect3DTexture9 (IUnknown part) ---------------------------------------
pub mod unk_vt {
    pub const RELEASE: usize = 2;
}

// --- Constants ---------------------------------------------------------------
pub const D3DCREATE_PUREDEVICE: u32 = 0x10;
pub const D3DCREATE_MULTITHREADED: u32 = 0x4;
pub const D3DCREATE_SOFTWARE_VERTEXPROCESSING: u32 = 0x20;
pub const D3DCREATE_HARDWARE_VERTEXPROCESSING: u32 = 0x40;
pub const D3DCREATE_MIXED_VERTEXPROCESSING: u32 = 0x80;

pub const D3DPOOL_DEFAULT: u32 = 0;
pub const D3DPOOL_MANAGED: u32 = 1;
pub const D3DPOOL_SYSTEMMEM: u32 = 2;
pub const D3DPOOL_SCRATCH: u32 = 3;

/// Highest D3DRENDERSTATETYPE is D3DRS_BLENDOPALPHA = 209.
pub const RENDER_STATE_SLOTS: usize = 256;
/// D3DSAMPLERSTATETYPE: D3DSAMP_ADDRESSU = 1 .. D3DSAMP_DMAPOFFSET = 13.
pub const SAMPLER_STATE_SLOTS: usize = 14;
/// Pixel samplers 0..15. Displacement-map (256) and vertex samplers (257..260) bypass the cache.
pub const SAMPLERS: usize = 16;
/// D3DTEXTURESTAGESTATETYPE: D3DTSS_COLOROP = 1 .. D3DTSS_CONSTANT = 32.
pub const TSS_SLOTS: usize = 33;
pub const TEXTURE_STAGES: usize = 8;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct D3DPRESENT_PARAMETERS {
    pub BackBufferWidth: u32,
    pub BackBufferHeight: u32,
    pub BackBufferFormat: u32,
    pub BackBufferCount: u32,
    pub MultiSampleType: u32,
    pub MultiSampleQuality: u32,
    pub SwapEffect: u32,
    pub hDeviceWindow: *mut c_void,
    pub Windowed: i32,
    pub EnableAutoDepthStencil: i32,
    pub AutoDepthStencilFormat: u32,
    pub Flags: u32,
    pub FullScreen_RefreshRateInHz: u32,
    pub PresentationInterval: u32,
}

impl D3DPRESENT_PARAMETERS {
    pub fn describe(&self) -> String {
        let interval = match self.PresentationInterval {
            0 => "DEFAULT(vsync)".to_string(),
            1 => "ONE(vsync)".to_string(),
            0x8000_0000 => "IMMEDIATE".to_string(),
            v => format!("0x{v:X}"),
        };
        format!(
            "{}x{} fmt={} buffers={} msaa={}/{} swap={} windowed={} depth={}(fmt {}) refresh={} interval={}",
            self.BackBufferWidth,
            self.BackBufferHeight,
            self.BackBufferFormat,
            self.BackBufferCount,
            self.MultiSampleType,
            self.MultiSampleQuality,
            self.SwapEffect,
            self.Windowed != 0,
            self.EnableAutoDepthStencil != 0,
            self.AutoDepthStencilFormat,
            self.FullScreen_RefreshRateInHz,
            interval
        )
    }
}

// --- Method signatures (this pointer first; __stdcall on x86) ---------------
pub type Direct3DCreate9Fn = unsafe extern "system" fn(sdk_version: u32) -> *mut c_void;
pub type CreateDeviceFn = unsafe extern "system" fn(
    this: *mut c_void,
    adapter: u32,
    dev_type: u32,
    focus: *mut c_void,
    behavior: u32,
    pp: *mut D3DPRESENT_PARAMETERS,
    out: *mut *mut Device,
) -> HRESULT;

pub type GetAvailableTextureMemFn = unsafe extern "system" fn(this: *mut Device) -> u32;
pub type ResetFn =
    unsafe extern "system" fn(this: *mut Device, pp: *mut D3DPRESENT_PARAMETERS) -> HRESULT;
pub type PresentFn = unsafe extern "system" fn(
    this: *mut Device,
    src: *const c_void,
    dst: *const c_void,
    wnd: *mut c_void,
    dirty: *const c_void,
) -> HRESULT;
pub type CreateTextureFn = unsafe extern "system" fn(
    this: *mut Device,
    width: u32,
    height: u32,
    levels: u32,
    usage: u32,
    format: u32,
    pool: u32,
    out: *mut *mut c_void,
    shared: *mut c_void,
) -> HRESULT;
pub type CreateVertexBufferFn = unsafe extern "system" fn(
    this: *mut Device,
    length: u32,
    usage: u32,
    fvf: u32,
    pool: u32,
    out: *mut *mut c_void,
    shared: *mut c_void,
) -> HRESULT;
pub type CreateIndexBufferFn = unsafe extern "system" fn(
    this: *mut Device,
    length: u32,
    usage: u32,
    format: u32,
    pool: u32,
    out: *mut *mut c_void,
    shared: *mut c_void,
) -> HRESULT;
pub type SetRenderStateFn =
    unsafe extern "system" fn(this: *mut Device, state: u32, value: u32) -> HRESULT;
pub type GetRenderStateFn =
    unsafe extern "system" fn(this: *mut Device, state: u32, value: *mut u32) -> HRESULT;
pub type CreateStateBlockFn =
    unsafe extern "system" fn(this: *mut Device, ty: u32, out: *mut *mut c_void) -> HRESULT;
pub type BeginStateBlockFn = unsafe extern "system" fn(this: *mut Device) -> HRESULT;
pub type EndStateBlockFn =
    unsafe extern "system" fn(this: *mut Device, out: *mut *mut c_void) -> HRESULT;
pub type SetTextureFn =
    unsafe extern "system" fn(this: *mut Device, stage: u32, tex: *mut c_void) -> HRESULT;
pub type SetStageStateFn =
    unsafe extern "system" fn(this: *mut Device, stage: u32, ty: u32, value: u32) -> HRESULT;
pub type GetStageStateFn =
    unsafe extern "system" fn(this: *mut Device, stage: u32, ty: u32, value: *mut u32) -> HRESULT;
pub type DrawPrimitiveFn =
    unsafe extern "system" fn(this: *mut Device, prim: u32, start: u32, count: u32) -> HRESULT;
pub type DrawIndexedPrimitiveFn = unsafe extern "system" fn(
    this: *mut Device,
    prim: u32,
    base_vertex: i32,
    min_index: u32,
    num_vertices: u32,
    start_index: u32,
    prim_count: u32,
) -> HRESULT;
pub type DrawPrimitiveUPFn = unsafe extern "system" fn(
    this: *mut Device,
    prim: u32,
    prim_count: u32,
    data: *const c_void,
    stride: u32,
) -> HRESULT;
pub type DrawIndexedPrimitiveUPFn = unsafe extern "system" fn(
    this: *mut Device,
    prim: u32,
    min_index: u32,
    num_vertices: u32,
    prim_count: u32,
    indices: *const c_void,
    index_format: u32,
    vertices: *const c_void,
    stride: u32,
) -> HRESULT;
pub type SetPtrFn = unsafe extern "system" fn(this: *mut Device, p: *mut c_void) -> HRESULT;
pub type SetFvfFn = unsafe extern "system" fn(this: *mut Device, fvf: u32) -> HRESULT;
pub type SetStreamSourceFn = unsafe extern "system" fn(
    this: *mut Device,
    stream: u32,
    vb: *mut c_void,
    offset: u32,
    stride: u32,
) -> HRESULT;
pub type StateBlockApplyFn = unsafe extern "system" fn(this: *mut c_void) -> HRESULT;
pub type ReleaseFn = unsafe extern "system" fn(this: *mut c_void) -> u32;

/// Estimated size in bytes of a texture's mip chain. Unknown formats return 0.
pub fn estimate_texture_bytes(width: u32, height: u32, levels: u32, format: u32) -> u64 {
    const fn fourcc(s: &[u8; 4]) -> u32 {
        (s[0] as u32) | (s[1] as u32) << 8 | (s[2] as u32) << 16 | (s[3] as u32) << 24
    }
    // (bits per pixel, block-compressed)
    let (bpp, bc): (u64, bool) = match format {
        21 | 22 | 32 | 31 => (32, false), // A8R8G8B8, X8R8G8B8, A8B8G8R8, A2B10G10R10
        23..=26 => (16, false),           // R5G6B5, X1R5G5B5, A1R5G5B5, A4R4G4B4
        28 | 41 | 50 => (8, false),       // A8, P8, L8
        51 => (16, false),                // A8L8
        f if f == fourcc(b"DXT1") => (4, true),
        f if f == fourcc(b"DXT2")
            || f == fourcc(b"DXT3")
            || f == fourcc(b"DXT4")
            || f == fourcc(b"DXT5") =>
        {
            (8, true)
        }
        _ => return 0,
    };
    let (mut w, mut h) = (width.max(1) as u64, height.max(1) as u64);
    let max_levels = if levels == 0 { 32 } else { levels };
    let mut total = 0u64;
    for _ in 0..max_levels {
        total += if bc {
            w.div_ceil(4) * h.div_ceil(4) * bpp * 16 / 8
        } else {
            w * h * bpp / 8
        };
        if w == 1 && h == 1 {
            break;
        }
        w = (w / 2).max(1);
        h = (h / 2).max(1);
    }
    total
}
