#!/usr/bin/env bash
# Builds sape.dll (i686-pc-windows-gnu) and the C harness, then runs the
# correctness smoke test under Wine in several SAPE configurations.
# Requirements (Debian/Ubuntu): gcc-mingw-w64-i686, wine, wine32:i386,
# libgl1-mesa-dri:i386, xvfb.   Usage: tools/smoke-test/run.sh [workdir]
set -euo pipefail
root="$(cd "$(dirname "$0")/../.." && pwd)"
work="${1:-$root/target/smoke}"
mkdir -p "$work"
cargo build -p sape --release --target i686-pc-windows-gnu --manifest-path "$root/Cargo.toml"
cp "$root/target/i686-pc-windows-gnu/release/sape.dll" "$work/"
i686-w64-mingw32-gcc -O2 -Wall "$root/tools/smoke-test/harness.c" -o "$work/harness.exe" -ld3d9 -luser32 -lgdi32
export WINEDEBUG=-all WINEPREFIX="$work/prefix"

fail=0
run_case() { # name, ini-body, harness-args...
  local name="$1" ini="$2"; shift 2
  rm -f "$work/d3d9.dll"
  printf '%b' "$ini" > "$work/sape.ini"
  rm -f "$work/sape.log"
  echo "=== $name"
  if (cd "$work" && timeout 300 xvfb-run -a -s "-screen 0 1024x768x24" wine harness.exe "$@"); then :; else
    echo "!!! case '$name' failed"; fail=1
  fi
  [ -f "$work/sape.log" ] && grep -E "ERROR|summary|per frame" "$work/sape.log" | head -6 || true
}

run_case "baseline (SAPE not loaded)" "" none 120 bench
run_case "monitor level 1"            "[monitor]\nlevel=1\nsummary_interval_ms=1000\n" sape.dll 120 bench
run_case "analysis only"              "[monitor]\nlevel=2\nsummary_interval_ms=1000\n[state_filter]\nenabled=1\nfilter=0\n[resources]\ntrack=1\n" sape.dll 120 bench
run_case "filter on"                  "[monitor]\nlevel=2\nsummary_interval_ms=1000\n[state_filter]\nenabled=1\nfilter=1\n[resources]\ntrack=1\n" sape.dll 120 bench
run_case "filter on + validate"       "[monitor]\nlevel=1\nsummary_interval_ms=1000\n[state_filter]\nenabled=1\nfilter=1\nvalidate=1\n" sape.dll 120
# d3d9.dll proxy mode: the harness imports d3d9.dll statically (like gta_sa.exe)
# and does NOT load SAPE itself; the loader must pick our d3d9.dll.
proxy_case() {
  printf '%b' "$2" > "$work/sape.ini"; rm -f "$work/sape.log"
  cp "$work/sape.dll" "$work/d3d9.dll"
  echo "=== $1"
  if (cd "$work" && WINEDLLOVERRIDES="d3d9=n,b" timeout 300 xvfb-run -a -s "-screen 0 1024x768x24" wine harness.exe none 120 bench) \
     && grep -q "mode: d3d9.dll proxy" "$work/sape.log" && grep -q "device hooks" "$work/sape.log"; then :; else
    echo "!!! case '$1' failed"; fail=1
  fi
  rm -f "$work/d3d9.dll"
}
# A mod below SAPE that moves the device vtable to the heap (seen in a real
# GTA SA modpack). SAPE must still install its hooks and stay correct.
shadow_case() {
  printf '%b' "$2" > "$work/sape.ini"; rm -f "$work/sape.log"
  echo "=== $1"
  if (cd "$work" && timeout 300 xvfb-run -a -s "-screen 0 1024x768x24" wine harness.exe sape.dll 120 nobench shadow) \
     && grep -q "device hooks (CreateDevice" "$work/sape.log" && grep -q "heap/private memory" "$work/sape.log"; then :; else
    echo "!!! case '$1' failed"; fail=1
  fi
}
shadow_case "wrapped device (heap vtable), filter on" "[monitor]\nlevel=2\n[state_filter]\nenabled=1\nfilter=1\n[resources]\ntrack=1\n"
shadow_case "wrapped device (heap vtable), validate" "[monitor]\nlevel=1\n[state_filter]\nenabled=1\nfilter=1\nvalidate=1\n"

proxy_case "d3d9.dll proxy, filter on" "[monitor]\nlevel=2\n[state_filter]\nenabled=1\nfilter=1\n[resources]\ntrack=1\n"
proxy_case "d3d9.dll proxy, validate" "[monitor]\nlevel=1\n[state_filter]\nenabled=1\nfilter=1\nvalidate=1\n"
[ "$fail" = 0 ] && echo "ALL SMOKE CASES PASSED" || { echo "SMOKE CASES FAILED"; exit 1; }
