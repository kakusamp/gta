# 0001 — Redundant D3D9 state filter

Status: **implemented, correctness-tested, not benchmarked in-game.** Default: off.

## What it changes

Keeps a shadow copy of render states (`D3DRS_*`, 256 slots), sampler states
(pixel samplers 0–15 × `D3DSAMP_*`) and texture stage states (stages 0–7 ×
`D3DTSS_*`). When the game (or anything else in the process) sets a state to
the value it already has, the call is answered with `D3D_OK` without calling
into d3d9.dll.

With `filter=0` the same machinery only **counts** redundant calls
("analysis mode"), which is how we find out whether the filter is worth
enabling.

## Why it should improve performance

Each `Set*` call into the D3D9 runtime / driver costs CPU time (validation,
state bookkeeping, possibly locking). GTA SA renders through several layers
that set state independently: RenderWare's D3D9 driver, GTA's custom
pipelines, D3DX (used by SA-MP for text), SA-MP itself and other mods.

**Supporting evidence so far:** a skipped call costs ~5 ns instead of ~22 ns on
wined3d (see `BENCHMARKING.md`). **Counter-evidence to check:** RenderWare is
believed to cache render states itself (**[H]**, `RESEARCH.md` §3), so the
redundancy rate in the real game may be low. Expected gain is therefore small
(≤ 1% of frame time) unless level-2 counters show thousands of redundant calls
per frame, e.g. in SA-MP with heavy text/nametag rendering.

## Functions intercepted

All are COM vtable slots of the device's class (`__stdcall`, `this` first),
indices from `d3d9.h`, patched by atomic slot exchange (`hook::patch_slot`):

| Method | Index | Role |
|---|---|---|
| `IDirect3DDevice9::SetRenderState` | 57 | filtered |
| `IDirect3DDevice9::SetSamplerState` | 69 | filtered |
| `IDirect3DDevice9::SetTextureStageState` | 67 | filtered |
| `IDirect3DDevice9::BeginStateBlock` / `EndStateBlock` | 60 / 61 | recording flag |
| `IDirect3DDevice9::CreateStateBlock` | 59 | discovers state-block class |
| `IDirect3DStateBlock9::Apply` | 5 | invalidates shadow |
| `IDirect3DDevice9::Reset` | 16 | invalidates shadow (before and after) |
| `IDirect3DDevice9::Present` | 17 | counters, hotkeys, A/B switching |
| `GetRenderState` / `GetSamplerState` / `GetTextureStageState` | 58 / 68 / 66 | called (not hooked) in validation mode |

Not filtered: vertex samplers (257–260), the displacement-map sampler (256),
`SetTexture` (counted only), shaders, streams, transforms.

## Correctness argument and tests

See the invariants in [`../ARCHITECTURE.md`](../ARCHITECTURE.md#safety-invariants-of-the-state-filter).
Unit tests (`sape-core/src/state_cache.rs`) cover the decision logic. The
Wine smoke test (`tools/smoke-test`) runs on a real D3D9 implementation and
verifies device state with `Get*` after:

* repeated redundant calls,
* state-block recording (recorded values must not enter the shadow),
* `Apply` of a recorded block and of a `CreateStateBlock` block,
* `Reset`.

Mutation testing: removing the recording flag, the `Apply` invalidation, or
both `Reset` invalidations each makes the smoke test fail.
`validate=1` passes the same suite with zero mismatches.

## Compatibility risks

| Risk | Effect | Mitigation |
|---|---|---|
| A mod hooked these methods *before* SAPE and changes the value it forwards | Shadow holds the value we saw, the device holds the modified value → a later call could be wrongly skipped | Run once with `validate=1`: any mismatch is logged and filtering is locked off |
| State changed through a path that bypasses the device vtable (e.g. a wrapper calling the real device directly underneath us) | Same as above | Same; plus such a wrapper's own calls go to *its* inner device, which we do not shadow |
| Second state-block class in the process | Its `Apply` would be invisible | Detected; filtering is locked off automatically |
| Multithreaded state setting | Data race on shadow | Shadow is render-thread-only; foreign calls pass through and force invalidation |
| Pure device | Validation unavailable | Detected; validation disables itself, filter keeps working |
| Pre-existing device state unknown at late attach | None | Every slot starts unknown; first call always passes through |

No visual change is possible if the invariants hold, because no call that
would change device state is ever skipped.

## Benchmark results

* Wine micro-benchmark: see `BENCHMARKING.md` (~17 ns saved per redundant call).
* **In-game (first session, SA-MP + modpack, see BENCHMARKING.md): 0.0 %
  redundant render/sampler/texture-stage state calls** at ~284 render-state
  calls per frame. Expected benefit in that setup: negligible (< 0.01 ms per
  frame). Verdict so far: **keep off by default**; it stays available for
  setups where level-2 statistics show real redundancy.
* Still required before any default change:
  1. Level-2 redundancy statistics in scenes S1, S3, S5.
  2. A/B captures (`ab_interval_s=10`) in S1, S3 and S5, 3 sessions each, on
     the Microsoft runtime and on DXVK.

## How to disable

* Turn off at runtime: **Ctrl+Shift+F11** (only switches filtering; the hooks stay).
* Turn off completely: `[state_filter] enabled = 0` (no hooks installed; the default).
