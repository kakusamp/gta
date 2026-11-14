# SAPE — Benchmarking Protocol

The baseline is **original GTA SA behaviour without SAPE**. An optimization is
accepted only if its gain exceeds the run-to-run noise of repeated captures.

## Metrics (fixed definitions, `sape-core/src/stats.rs`)

| Metric | Definition |
|---|---|
| avg FPS | frames / total seconds (not the mean of per-frame FPS) |
| avg frame time | mean Present-to-Present interval (ms) |
| 1% low FPS | 1000 / mean frame time of the slowest 1% of frames |
| 0.1% low FPS | same with the slowest 0.1% |
| p99 / p99.9 | nearest-rank percentile of frame time |
| spike | frame > max(`spike_min_ms`, `spike_factor` × rolling median of the last 120 frames) |
| CPU | process CPU time / wall time, in cores (1.00 = one core fully busy) |
| VA | used / total user-mode virtual address space |

## Configurations to compare

| Name | Setup | Purpose |
|---|---|---|
| **B0** | SAPE not installed; external capture (e.g. PresentMon) | True baseline |
| **B1** | `monitor.level=1` | SAPE's own minimum overhead (compare with B0 via PresentMon) |
| **B2** | `level=2` and/or `state_filter.enabled=1, filter=0` | Overhead of counting hooks; produces redundancy statistics |
| **O-n** | B2 + optimization n on | Effect of optimization n |

Measure B1 against B0 at least once per release, so SAPE's own cost is known.
The comparison for an optimization is **O-n vs. B2** (same hooks, only the
decision differs), and the A/B mode does exactly that.

## A/B capture (recommended)

```ini
[monitor]
level = 1
[state_filter]
enabled = 1
filter = 0
ab_interval_s = 10
```

1. Go to the test location and stand still (or follow a fixed route).
2. Press **Ctrl+Shift+F10** to start the capture. The filter flips every 10 s.
3. After at least 3 minutes, press Ctrl+Shift+F10 again.
4. `sape-analyze --warmup 300 --settle 60 sape_frames_*.csv`

Interleaving ON/OFF within one session cancels slow drift (thermal throttling,
background tasks, streaming state) that ruins "run A, restart, run B"
comparisons.

## Scenes

| ID | Scene | Stresses |
|---|---|---|
| S1 | Single player, Grove Street, noon, standing | Baseline world rendering |
| S2 | Single player, driving the LS freeway loop | Streaming (B3) |
| S3 | SA-MP, empty server, same spot as S1 | samp.dll overhead (HUD, chat) |
| S4 | SA-MP, ≥50 players and ≥50 vehicles streamed | Entities, animation (B5–B7) |
| S5 | SA-MP, custom map with ≥2000 objects in view | Draw calls, state changes (B1/B2) |
| S6 | SA-MP, flying across a heavy map | Object churn spikes (B4) |

Record hardware, OS, GPU driver, the D3D9 implementation (Microsoft runtime or
DXVK version), resolution, in-game settings, frame limiter state and all
other installed mods for every result.

## Repetition and acceptance

* ≥3 independent sessions per scene; report the per-session values, not only the pooled ones.
* An effect is real only if it has the same sign in every session and exceeds
  the spread between sessions of the same configuration.
* Frame time matters more than FPS: report Δ avg ms, Δ p99 ms and Δ 1% low.

### Cautionary example (from our own smoke test)

The Wine harness injects one deliberate 80 ms stall. In one A/B capture that
stall happened to land in the OFF group, and `sape-analyze` printed
`1% low +175%` for "filter ON". That is noise from a single frame, not an
effect of the filter. It is why `sape-analyze` reports each session separately
and why the acceptance rule above requires consistency across sessions.

## Micro-benchmark results so far

Environment: Wine 9.0 / wined3d over Mesa llvmpipe (software rendering),
Xvfb, 4 vCPU Intel Xeon @ 2.1 GHz cloud VM. Harness: `tools/smoke-test`,
2 M `SetRenderState` calls per measurement, rep 2 of 3 shown (reps 0–2 in parentheses).

| Configuration | Redundant call (ns) | Changing call (ns) |
|---|---|---|
| SAPE not loaded | 22.7 (21.3–22.7) | 26.3 (23.2–26.3) |
| monitor level 1 (no state hooks) | 20.5 (20.5–28.0) | 20.9 (20.9–27.5) |
| analysis only (hooks, filter off) | 27.6 (25.2–27.6) | 28.5 (24.2–28.5) |
| filter on | **5.0** (5.0–5.5) | 23.6 (23.2–24.3) |

Reading:

* A skipped redundant call costs ~5 ns instead of ~22 ns, a saving of ~17 ns
  per redundant call **on wined3d**. Per-call costs on the Microsoft runtime
  and on DXVK are different and have not been measured yet.
* The hooks add a few ns per call when not filtering (analysis mode). The
  noise between reps (±4 ns) is of the same order, so this overhead is not
  precisely resolved.
* Scale check: 2,000 redundant calls/frame × 17 ns ≈ 0.034 ms per frame, about
  0.2% of a 16.7 ms frame. **Optimization 0001 can only matter if the game
  issues many thousands of redundant state calls per frame**, which is exactly
  what the in-game level-2 measurements must establish first.

**No in-game benchmark results exist yet.** They require Windows with GTA SA
1.0 US, which is not available in the development environment.
