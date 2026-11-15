# SAPE — Architecture

## Workspace

```
Cargo.toml                    workspace, release profile (LTO, panic=abort)
.cargo/config.toml            i686 targets (msvc + mingw cross), static CRT
sape.ini                      default configuration (embedded into the DLL)
crates/
  sape-core/                  platform-independent, unit-tested on any host
    ini.rs                    dependency-free INI parser
    config.rs                 configuration_manager: typed config + validation
    stats.rs                  frame statistics (avg, 1%/0.1% low, pXX), spike detector
    state_cache.rs            O(1)-invalidate shadow state + filter decision logic
  sape/                       the 32-bit DLL / ASI (cdylib, Windows only)
    lib.rs                    DllMain, init, Runtime (config, paths, game info)
    win32.rs                  hand-written Win32 FFI + safe memory probes
    log.rs                    logging_system
    hook.rs                   pointer-slot patching (vtable / IAT), PE import walk
    d3d9/mod.rs               d3d9_hook: IAT → CreateDevice → device vtable
    d3d9/hooks.rs             hook functions (one per intercepted method)
    d3d9/types.rs             D3D9 ABI: vtable indices, signatures, structs
    render_optimizer.rs       redundant-state detection/filter (optimization 0001)
    monitor.rs                performance_monitor: timing, counters, CSV, hotkeys
    resources.rs              memory_manager (measurement): texture/buffer tracking
    game.rs                   renderware_hook foundation: version detection, address table
    samp.rs                   samp_compatibility: client detection (read-only)
    proxy.rs                  d3d9.dll proxy mode: forwards documented d3d9 exports, optional chain-load
tools/
  sape-analyze/               offline CSV analyzer (A/B comparison)
  smoke-test/                 C harness + Wine runner for correctness tests
docs/                         research, architecture, benchmarking, per-optimization docs
```

No third-party crates. The DLL imports only system DLLs (kernel32, user32,
ntdll, msvcrt, bcryptprimitives, api-ms-win-core-synch).

## Module status

| Requested module | Location | Milestone 1 status |
|---|---|---|
| d3d9_hook | `sape/src/d3d9/` | ✅ IAT + vtable hooks, late attach |
| renderware_hook | `sape/src/game.rs` | 🟡 version detection + read-only address table; no game hooks yet (by design) |
| samp_compatibility | `sape/src/samp.rs` | 🟡 detection only |
| memory_manager | `sape/src/resources.rs` | 🟡 measurement: textures/buffers, VA usage |
| render_optimizer | `sape/src/render_optimizer.rs` | ✅ optimization 0001 (opt-in) |
| entity_optimizer | — | ⏳ needs internal instrumentation first (B5–B7) |
| streaming_optimizer | — | ⏳ needs streaming instrumentation first (B3/B4) |
| performance_monitor | `sape/src/monitor.rs` | ✅ (log + CSV; on-screen overlay planned) |
| adaptive_engine | — | ⏳ design below; no code until ≥2 optimizations are proven |
| configuration_manager | `sape-core/src/config.rs` | ✅ |
| logging_system | `sape/src/log.rs` | ✅ |

## Load modes

The same binary runs in two modes, selected by its file name:

| File name | Mode | Entry into D3D9 |
|---|---|---|
| `d3d9.dll` (next to the exe) | **proxy** | The game imports *our* `Direct3DCreate9`; we forward to `[proxy] chain` or `<system dir>\d3d9.dll` (loaded lazily on the first call, never under the loader lock) and hook the returned IDirect3D9. Config/log are named `sape.ini`/`sape.log`. |
| anything else (`sape.asi`, ...) | **plugin** | IAT patch of the executable's `d3d9.dll!Direct3DCreate9` import (+ late attach). |

Only documented `d3d9.h` exports are provided in proxy mode: `Direct3DCreate9`,
`Direct3DCreate9Ex` (forwarded, not hooked), and the seven `D3DPERF_*`
functions. Undocumented internal exports are not provided because their
signatures are not published. Both modes are covered by the Wine smoke test.

## Boot sequence

```
ASI loader / LoadLibrary
  └─ DllMain(PROCESS_ATTACH)            loader lock held: memory ops only
       ├─ read <dll>.ini (create default if missing), open <dll>.log
       ├─ game::detect()                 version signature, LAA flag (read-only, VirtualQuery-checked)
       ├─ samp::poll_once()              log samp.dll build if already loaded
       ├─ d3d9::install_iat_hook()       patch exe IAT slot of d3d9!Direct3DCreate9
       └─ d3d9::late_attach()            only if the device already exists (1.0 US)

game: Direct3DCreate9 ──► hk_direct3d_create9 ──► patch IDirect3D9::CreateDevice
game: CreateDevice    ──► hk_create_device    ──► record render thread, install_device_hooks()
game: Present         ──► hk_present          ──► monitor::on_present() ──► original
```

Nothing heavier than a pointer write happens under the loader lock. Threads
(the CSV writer) are created lazily from `Present`.

## Hook installation policy

* Hooks are installed **per enabled feature**: with `monitor.level=1` only
  `Present` and `Reset` are patched; with everything off, nothing is patched.
* Slots are patched with an atomic compare-exchange after storing the original
  pointer, so a concurrent caller can never reach a hook whose original is
  unset (`hook::patch_slot`).
* Vtables are validated before patching: readable, inside a mapped image,
  every entry pointing at executable memory.
* Only the first device / state-block / texture *class* is hooked. A second
  class (different vtable) is logged and left alone; for state blocks this
  also locks the filter off, because an untracked `Apply` would make the
  shadow state unsound.
* State-tracking hooks are all-or-nothing: if any of them fails to install,
  filtering is locked off.
* No unhooking at process exit (d3d9.dll may already be unloaded).

## Threading model

* **Render thread** = thread that created the device (or, after late attach,
  the first thread calling `Present`).
* `render_optimizer::Shadow` and `monitor::State` are touched only on the
  render thread. A state call from another thread is passed straight through
  and sets a `DIRTY` flag; the render thread invalidates the whole shadow on
  its next call.
* Hot-path counters are relaxed atomics updated with load+store (plain `mov`,
  no `lock` prefix). Counts from concurrent threads may be lost; this is
  accepted for statistics.
* Hooks never hold a reference into shared state across the call to the
  original method, so re-entrant calls (other wrappers calling back into the
  device) are sound.

## Safety invariants of the state filter

1. A value is "known" only after a `Set*` call that **succeeded**, on the
   render thread, **outside** state-block recording.
2. Everything that changes state behind our back invalidates:
   `Reset` (before and after), `IDirect3DStateBlock9::Apply`, calls from other
   threads, failed calls.
3. Skipped calls return `D3D_OK`, which is what the device returned for the
   identical call that populated the entry.
4. `state_filter.validate=1` re-checks every skip against `Get*` and locks the
   filter off on the first mismatch.

Invariants 1–2 are covered by `tools/smoke-test/harness.c`; mutation testing
showed the harness fails if the recording flag, the `Apply` invalidation or
both `Reset` invalidations are removed.

## Adding an optimization

1. Write `docs/optimizations/NNNN-name.md` from the template in
   [`optimizations/README.md`](optimizations/README.md): what changes, why,
   intercepted functions, risks, how to disable.
2. Add a config section with the feature **off by default**, plus a runtime
   toggle if it can be switched safely mid-frame (enables A/B capture).
3. Put pure logic in `sape-core` with unit tests; put hooks in `sape`.
4. Extend the smoke harness with checks that **fail** when the optimization's
   safety mechanism is removed (mutation-test it).
5. Benchmark per [`BENCHMARKING.md`](BENCHMARKING.md); only then consider a
   default change.

## Adaptive engine (design only)

The adaptive engine will *select among proven optimizations*, never invent
behaviour. Inputs: the monitor's per-window signals (draws/frame, redundant
%, spike rate, VA usage, resource-creation rate, CPU vs. GPU bound).
Outputs: enabling/disabling individual optimizations that are marked
`adaptive_safe` in their documentation (no visual or gameplay effect when
toggled). It will not change graphics settings, draw distance, network
behaviour or frame limiting. Hysteresis and minimum dwell times will avoid
oscillation, and every decision will be logged. Implementation waits until
at least two optimizations have shown measurable in-game gains.

## Planned next steps (each independently benchmarked)

1. **In-game baseline captures** (single player, SA-MP light, SA-MP heavy map)
   with level 1 and level 2 to quantify B1/B2/B3 and SAPE's own overhead.
2. GPU timestamp queries → CPU/GPU-bound classification in the summary.
3. Optional on-screen overlay (drawn with our own minimal state-block-guarded
   quad/text renderer, so it does not perturb the game's state).
4. Redundant `SetTexture` / `SetStreamSource` / shader filtering (only if level-2
   data shows meaningful redundancy).
5. First internal instrumentation (read-only timing of render-list
   construction and streaming), with call-site byte verification.
