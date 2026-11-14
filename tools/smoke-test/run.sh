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
[ "$fail" = 0 ] && echo "ALL SMOKE CASES PASSED" || { echo "SMOKE CASES FAILED"; exit 1; }
