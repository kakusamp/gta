# SAPE — Technical Research

This document records what we know, what we believe and what we still have to
measure before writing optimization code for GTA San Andreas (32-bit) and SA-MP.

## Evidence labels

Every claim carries one of these labels. Code may only *depend* on **[V]**
claims, and on **[C]** claims only behind a runtime validation that refuses to
act when the claim does not hold.

| Label | Meaning |
|---|---|
| **[V]** Verified | Established from a primary specification (Windows SDK headers / Microsoft docs, PE format) **or** reproduced by SAPE's own tests in this repository. |
| **[C]** Community-documented | Published by long-running reverse-engineering projects (plugin-sdk, gta-reversed, MTA:SA, SilentPatch, SA-MP client tooling). Widely relied on, but **not re-derived by us**. |
| **[H]** Hypothesis | Plausible but unmeasured. Each one names the measurement that would confirm or reject it. |

---

## 1. GTA SA 32-bit process architecture

### 1.1 Executables

* **[C]** Retail PC versions: 1.0 US, 1.0 EU, 1.01 US/EU, Steam 3.0 (and later
  re-releases). The 1.0 US executable is the de-facto modding baseline;
  **SA-MP 0.3.x requires 1.0 US**.
* **[C]** Versions can be told apart by a dword equal to `0x94BF` at a
  version-specific address (`0x82457C` = 1.0 US, `0x8245BC` = 1.0 EU,
  `0x8252FC` = 1.01 US, `0x82533C` = 1.01 EU, `0x85EC4A` = Steam). SAPE reads
  these only after `VirtualQuery` confirms the address is inside the mapped
  image and readable, and logs the PE `TimeDateStamp` / `SizeOfImage` so a
  wrong match can be reported (`crates/sape/src/game.rs`).
* **[C]** The 1.0 US executable is loaded at its preferred base `0x400000`;
  game addresses are absolute.
* **[V, at runtime]** Whether the executable is *large-address-aware* is read
  from the PE header and logged at startup. Without LAA a 32-bit process gets
  2 GB of user address space; with LAA on 64-bit Windows it gets 4 GB.
  **[C]** The stock executable is commonly not LAA and "4 GB patches" are
  widespread. Heavy SA-MP servers (thousands of custom objects / textures)
  exhaust *virtual address space* long before physical RAM, so the monitor
  reports address-space usage (`VA used/total`) in every summary.

### 1.2 Threads

* **[C]** Game simulation and rendering run on the **main thread**. A separate
  streaming thread (CdStream) performs asynchronous reads from IMG archives;
  turning the read buffer into models/textures (RenderWare stream parsing,
  texture creation) happens back on the main thread.
* **[V, by design]** SAPE therefore treats "the thread that created the D3D9
  device" as the render thread, and refuses to touch its shadow state from any
  other thread (`render_optimizer.rs`).
* **[H]** Moving parts of streaming conversion (e.g. TXD decoding) off the main
  thread is a candidate for spike reduction. *Measurement first*: correlate
  spikes with `tex_created` / `buf_created` (monitor level 2) and later with
  instrumented streaming functions.

### 1.3 Frame pacing

* **[C]** The in-game "frame limiter" option caps the frame rate (≈25 FPS in
  SA); several game systems are known to misbehave at high frame rates, and
  SA-MP provides its own `/fpslimit`.
* **Policy**: SAPE does **not** change frame limiting, vsync or presentation
  intervals (rule "avoid unnecessary frame synchronization changes"). It only
  measures Present-to-Present time.

---

## 2. Direct3D 9 interception

### 2.1 ABI facts

* **[V]** COM methods are `__stdcall` with `this` as the first stack argument;
  Rust's `extern "system"` is `__stdcall` on x86.
* **[V]** Vtable indices are the declaration order in `d3d9.h`, including the
  three `IUnknown` methods. Used here: `IDirect3D9::CreateDevice` = 16;
  `IDirect3DDevice9`: `GetAvailableTextureMem` 4, `Reset` 16, `Present` 17,
  `CreateTexture` 23, `CreateVertexBuffer` 26, `CreateIndexBuffer` 27,
  `SetRenderState` 57, `GetRenderState` 58, `CreateStateBlock` 59,
  `BeginStateBlock` 60, `EndStateBlock` 61, `SetTexture` 65,
  `GetTextureStageState` 66, `SetTextureStageState` 67, `GetSamplerState` 68,
  `SetSamplerState` 69, `DrawPrimitive` 81, `DrawIndexedPrimitive` 82,
  `DrawPrimitiveUP` 83, `DrawIndexedPrimitiveUP` 84, `SetVertexDeclaration` 87,
  `SetFVF` 89, `SetVertexShader` 92, `SetStreamSource` 100, `SetPixelShader`
  107; total 119 methods. `IDirect3DStateBlock9::Apply` = 5.
  **Reproduced** by the smoke test: every hooked method is called through the
  real vtable (wined3d) and behaves correctly.
* **[V]** Between `BeginStateBlock` and `EndStateBlock`, state-setting calls are
  *recorded* into the block and do **not** change device state.
  `IDirect3DStateBlock9::Apply` changes device state without any `Set*` call
  through the device. `Reset` returns the device to default state.
  All three are reproduced by `tools/smoke-test/harness.c`, and each of SAPE's
  corresponding invalidations was shown to be necessary by mutation testing
  (removing it makes the harness fail).
* **[V]** `D3DCREATE_PUREDEVICE` devices do not support `Get*` for state that
  can live in state blocks — SAPE's validation mode turns itself off on such a
  device instead of failing.

### 2.2 Interception methods compared

| Method | How | Pros | Cons | SAPE |
|---|---|---|---|---|
| Proxy `d3d9.dll` | Replacement DLL in the game folder that wraps every interface | No patching; sees everything | Conflicts with the many mods that are *also* `d3d9.dll` (ReShade, ENB, DXVK, other wrappers); must wrap ~119 methods × several interfaces | No |
| IAT patch | Replace `Direct3DCreate9` in the game exe's import table | One pointer write; no code bytes touched; chains with others | Only catches the import if done before the game calls it | **Yes** (primary) |
| Vtable slot patch | Replace selected entries in the shared COM vtable | Atomic 4-byte write; only the methods we need; chains with wrappers below *and* above us | Affects every object of that class in the process (normally one device) | **Yes** |
| Inline detour | Overwrite the first bytes of a function with a jump | Catches calls that bypass vtables | Needs instruction decoding / trampolines; fragile with other detours | No (not needed for D3D9) |

* **[V, smoke test]** IAT patch + vtable patching works against an executable
  that imports `Direct3DCreate9` statically, which is how the 1.0 US exe is
  built **[C]**.
* **Late attach**: if the device exists before SAPE loads, SAPE reads
  RenderWare's device pointer — **[C]** `0xC97C28` on 1.0 US (the value
  returned by `RwD3D9GetCurrentD3DDevice`) — validates that the object's vtable
  lies in a mapped image with executable entries, and only then patches it.

### 2.3 What sits between the game and the GPU

* **[C]** Many players run SA through **DXVK** (D3D9→Vulkan), ReShade or ENB,
  each of which exports its own `d3d9.dll`. Vtable patching hooks whichever
  device object the game receives, so SAPE measures the calls the *game*
  makes. Optimizations must be benchmarked on both the Microsoft runtime and
  DXVK, because their per-call costs differ.
* **[H]** samp.dll installs its own D3D9 hooks to draw chat, nametags, 3D text
  labels and dialogs, and uses D3DX (`ID3DXFont`, `ID3DXSprite`). D3DX sprites
  save/restore device state with **state blocks**. *Measurement*: level 2 log
  shows `sb_apply` per frame; a non-zero value in SA-MP and zero in single
  player would confirm it. SAPE's filter already invalidates on every `Apply`.

---

## 3. RenderWare rendering architecture (as used by GTA SA)

* **[C]** GTA SA uses RenderWare Graphics 3.6 with the D3D9 driver.
  Scene objects: `RpWorld` (unused for most rendering in SA), `RpClump`
  (hierarchy of frames + atomics: vehicles, peds), `RpAtomic` (one geometry
  instance: buildings, objects), `RpGeometry` split into `RpMesh`es — one per
  material.
* **[C]** Geometry is "instanced" once into D3D9 vertex/index buffers
  (RenderWare resource arena / `resEntry`), then each mesh is drawn with one
  `DrawIndexedPrimitive`. Consequence **[H]**: draw calls per frame ≈ Σ over
  visible atomics of their material count. There is no hardware instancing;
  100 identical SA-MP objects cost 100× the draws.
* **[C]** GTA adds custom pipelines: buildings with day/night vertex colours
  (`CCustomBuildingDNPipeline`), vehicles with environment maps / specular
  (`CCustomCarEnvMapPipeline`), skinned peds (RpSkin), plus a large 2D layer
  (HUD, fonts, sprites, radar).
* **[C]** RenderWare's D3D9 driver keeps its own cache of render states and
  only forwards changed values. **[H]** If so, the redundant-state rate seen at
  the D3D9 boundary is low for RW-originated calls and the gain of
  optimization 0001 comes mostly from other clients (D3DX/SA-MP, custom
  pipelines, mods). *Measurement*: monitor level 2, `RS redundant %` in single
  player vs. SA-MP vs. heavy SA-MP server. **This number decides whether
  0001 is worth enabling at all.**
* **[C]** Frame structure (simplified, names from gta-reversed):
  `Idle` → camera update → `CRenderer::ConstructRenderList` (scan world
  sectors, distance/visibility tests, LOD selection) → `CRenderer::PreRender`
  → `RenderScene` (roads, opaque entities, then alpha/fading lists sorted by
  `CVisibilityPlugins`) → effects/particles (`FxManager`), post effects
  (`CPostEffects`) → 2D (HUD, radar, fonts) → `Present`.

### 3.1 Calling conventions of game functions (for later internal hooks)

* **[C]** Member functions are MSVC `__thiscall` (`this` in ECX), free
  functions `__cdecl`; some are compiler-optimized with non-standard register
  usage. Rust stable supports `extern "thiscall"` (x86), and naked functions
  for custom conventions.
* **Policy** for internal hooks (not used in milestone 1): prefer **call-site
  redirection** (rewrite the `E8 rel32` of one verified call instruction) over
  function-entry detours; before patching, compare the bytes at the call site
  with the expected original encoding and refuse on mismatch (another mod got
  there first, or wrong exe version). Every address goes into `game.rs` with
  source, version and confidence.

---

## 4. Bottleneck candidates

All **[H]** until measured. Each line lists the instrument that decides it.

| # | Candidate | Why it is plausible | How SAPE will measure |
|---|---|---|---|
| B1 | **Draw-call / API overhead** (CPU-bound in the driver/runtime) | One DIP per material per atomic; SA-MP maps add thousands of objects | Level 2: draws/frame, prims/frame vs. frame time across scenes |
| B2 | **Redundant state changes** | Multiple layers (RW, GTA pipelines, D3DX, SA-MP, mods) set state independently | Level 2: `RS/samp/tss redundant %` |
| B3 | **Streaming spikes** | Model/texture conversion and D3D resource creation on the main thread | Spike lines include `tex_created`/`buffers_created`; later: instrument streaming |
| B4 | **SA-MP object churn** | Server-side streamers create/destroy objects as players move | Same as B3, plus correlate with SA-MP object counts (future read-only probe) |
| B5 | **Entity scan / visibility cost** | `ConstructRenderList` cost grows with entity density and draw distance | Requires internal timing hooks around render-list construction |
| B6 | **Alpha-list sorting** | Sorted per frame; grows with transparent objects (fences, foliage, SA-MP maps) | Internal timing hook |
| B7 | **Ped/vehicle animation & skinning** | Each streamed SA-MP player is a full ped with blended animations | Internal timing hook on ped processing |
| B8 | **Address-space exhaustion / fragmentation** | 32-bit process, large texture sets | `VA used/total` in summary; live texture estimate; later allocation tracking |
| B9 | **GPU-bound scenes** (high resolution, shadows, MSAA) | Then CPU optimizations show no FPS gain | Future: D3D9 timestamp queries to split CPU vs. GPU time |

---

## 5. SA-MP specific overhead

* **Hard rules**: SAPE never reads or writes SA-MP network state, never changes
  RPC/sync behaviour, never changes what other players see, and never alters
  object/pickup creation semantics. samp.dll is only *identified*
  (`samp.rs`: entry-point RVA → build, **[C]** table).
* Safe areas to investigate (all **[H]**):
  1. Rendering cost of what SA-MP adds per frame (nametags, 3D labels, chat,
     textdraws): D3D9-level counters already isolate `DrawPrimitiveUP` and
     state-block traffic.
  2. Spikes during object streaming: resource creation counters per spike.
  3. Many streamed players/vehicles: entity rendering and animation cost (needs
     internal instrumentation, B5–B7).
* Not safe / out of scope: reducing sync rates, skipping remote-player
  updates, culling network entities from simulation, changing streaming
  distances that the server relies on.

---

## 6. What is possible with D3D9 alone vs. internal hooks

| Optimization / instrument | D3D9 only | Needs GTA SA hooks |
|---|---|---|
| Frame time, spikes, 1%/0.1% lows | ✅ (done) | |
| Draw / state / resource counters | ✅ (done) | |
| Redundant render/sampler/TSS filter | ✅ (0001, opt-in) | |
| Redundant `SetTexture` / stream / shader filtering | ✅ (counting done) | |
| Texture creation tracking, live texture memory estimate | ✅ (done) | |
| Dynamic buffer churn detection (DEFAULT-pool VB/IB recreation) | ✅ (counters done) | |
| CPU vs. GPU bound classification | ✅ (timestamp queries, planned) | |
| Duplicate texture detection (same content uploaded twice) | ⚠️ possible (hash on `LockRect`/`UnlockRect`), expensive | better with TXD knowledge |
| Spike attribution to model loads | partial | ✅ streaming hooks |
| Visibility / culling improvements | ❌ | ✅ |
| Entity batching / material sorting | ❌ (no scene knowledge) | ✅ (deep change, high risk) |
| Animation / entity update cost | ❌ | ✅ |
| Allocation profiling (RwMalloc, operator new) | ❌ | ✅ |

---

## 7. Rules derived from this research

1. **Measure before optimizing.** Milestone 1 ships measurement plus exactly
   one opt-in optimization whose correctness is test-covered.
2. **Every optimization is off by default** until its document in
   `docs/optimizations/` contains in-game benchmark results.
3. **No game-memory writes** without a verified address table for the exact
   executable, byte-pattern verification at the patch site, and a config switch.
4. **When unsure, pass through.** The filter's failure mode must be "missed
   optimization", never "skipped call"; validation mode exists to prove it.
5. **Benchmark on both** the Microsoft D3D9 runtime and DXVK; results on one do
   not transfer to the other.

## References

* Windows SDK `d3d9.h`, `d3d9types.h`; Microsoft Docs: Direct3D 9 state
  blocks, `IDirect3DDevice9::Reset`, `D3DCREATE` flags; PE/COFF specification.
* plugin-sdk (DK22Pac et al.), gta-reversed, MTA:SA source, SilentPatch:
  community reverse-engineering of GTA SA (source of every **[C]** address).
* SA-MP client tooling (SAMP-API) for client build identification.
