# SAPE — San Andreas Performance Engine

A performance-optimization framework for **GTA San Andreas (32-bit)** and
**SA-MP**, written in Rust. It is **not** a graphics mod, shader injector, ENB
or ReShade alternative. It never lowers graphics quality, disables systems or
touches SA-MP networking.

> **Status: milestone 1, measurement first.** SAPE currently ships a
> low-overhead performance monitor and one opt-in optimization (redundant
> D3D9 state filtering, off by default) whose correctness is covered by
> automated tests. **No in-game benchmark results exist yet**, so no
> performance claims are made. See [`docs/BENCHMARKING.md`](docs/BENCHMARKING.md).

## Documents

* [`docs/RESEARCH.md`](docs/RESEARCH.md): GTA SA / RenderWare / D3D9 / SA-MP research, with every claim labelled *verified*, *community-documented* or *hypothesis*.
* [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md): modules, boot sequence, hook policy, threading, safety invariants, roadmap.
* [`docs/BENCHMARKING.md`](docs/BENCHMARKING.md): metrics, A/B protocol, scenes, results so far.
* [`docs/optimizations/`](docs/optimizations/): one record per optimization (what, why, hooks, risks, results, how to disable).

## What it does today

| Feature | Config | Default |
|---|---|---|
| Frame time, FPS, 1%/0.1% lows, spike detection, CPU, RAM, **32-bit address space**, VRAM estimate (log summary) | `[monitor] level=1` | on |
| Draw calls, primitives, state changes and their redundancy %, state-block applies, resource creation | `[monitor] level=2` | off |
| Per-frame CSV capture + offline analyzer | hotkey / `capture_on_start` | off |
| Texture lifetime + estimated live texture memory per pool | `[resources] track=1` | off |
| **0001 Redundant D3D9 state filter** (render/sampler/texture-stage states) | `[state_filter] enabled=1, filter=1` | off |
| A/B benchmark mode (flip the filter every N seconds while capturing) | `ab_interval_s` | off |
| Validation mode (verify every skipped call against the device) | `validate=1` | off |
| Game version, LAA flag and SA-MP client build detection (read-only) | always | on |

Hotkeys (with Ctrl+Shift, only while the game has focus): **F10** start/stop
capture, **F11** toggle the state filter, **F12** write a summary now.

## Build

GTA SA is a 32-bit process, so the DLL must target **i686**.

```sh
# Windows (MSVC toolchain)
rustup target add i686-pc-windows-msvc
cargo build -p sape --release --target i686-pc-windows-msvc

# Linux cross-compile (mingw-w64)
sudo apt install gcc-mingw-w64-i686
rustup target add i686-pc-windows-gnu
cargo build -p sape --release --target i686-pc-windows-gnu

# Host-side tests and the analyzer
cargo test -p sape-core
cargo run -p sape-analyze --release -- sape_frames_*.csv

# Correctness smoke test under Wine (Linux)
tools/smoke-test/run.sh
```

Output: `target/<triple>/release/sape.dll`.

## Install

1. Install an ASI loader (e.g. Ultimate ASI Loader). SA-MP and CLEO setups usually already have one.
2. Copy `sape.dll` into the game folder (or `scripts/`) and rename it to **`sape.asi`**.
3. Start the game once: `sape.ini` and `sape.log` appear next to the ASI.
4. To uninstall, delete `sape.asi`. Nothing else is modified.

Supported: Windows 10/11, `gta_sa.exe` 1.0 US (other versions get D3D9-level
features only), SA-MP 0.3.7 / 0.3.DL, with or without DXVK / ReShade.

## Contributing benchmark data

The most valuable contribution right now is **in-game measurements**: run
scenes S1/S3/S5 from `docs/BENCHMARKING.md` with `monitor.level=2` and attach
`sape.log` plus an A/B capture CSV, together with your hardware, driver,
D3D9 implementation (Microsoft runtime or DXVK) and mod list.

## License

MIT
