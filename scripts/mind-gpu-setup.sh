#!/usr/bin/env bash
# Set up and measure AMD GPU offload (Vulkan) for SAIL's local models (#34).
#
#   scripts/mind-gpu-setup.sh [--model PATH]... [--skip-build] [--dry-run]
#
# Steps, each one stopping with an explanation if it fails:
#  1. Inventory: GPU (lspci), render node access, Vulkan driver (vulkaninfo). Refuses a
#     software rasterizer (llvmpipe) as "the GPU".
#  2. Build llama.cpp with Vulkan at a pinned release into /srv/sail/llama.cpp-vulkan (the
#     runtime SSD; builds and binaries are hot paths). The CPU build is left untouched.
#  3. Check that llama-server lists the AMD device.
#  4. Benchmark every catalog model whose weights are installed (sail_common::models::CATALOG,
#     looked up in $SAIL_RUNTIME/models and $SAIL_DATA/models), or the --model paths given,
#     with llama-bench: CPU only, then all layers on the GPU with the MoE experts kept in
#     system RAM (--n-cpu-moe), sweeping how many layers' experts stay on the CPU. Results
#     go to $SAIL_DATA/mind/bench/ as JSON.
#  5. Recommend the fastest configuration for the default model (the one saved in Settings >
#     Models, else the primary). Only when it beats CPU-only on BOTH prompt processing and
#     generation does it write $SAIL_DATA/mind/gpu.env, which run.sh loads. Otherwise SAIL
#     stays on CPU (vision §5: enable Vulkan only after measuring). gpu.env holds one
#     configuration, so re-run this after changing the default model.
#
# Run it on the SAIL host, with SAIL stopped (the benchmark needs the RAM and the GPU), as
# the account that runs SAIL, without sudo. It needs no root except to install build
# packages, which it only prints.
set -euo pipefail

# The commit SAIL's own llama-server is built from (/home/main/llama.cpp, build 12014). It
# has --cpu-moe/--n-cpu-moe and the lfm3 architecture (LFM3 Omni) that b11600 lacked.
LLAMA_TAG="${LLAMA_TAG:-b12014}"
LLAMA_REPO="https://github.com/ggml-org/llama.cpp"
RUNTIME="${SAIL_RUNTIME:-/srv/sail}"
BUILD_DIR="${SAIL_LLAMA_VULKAN_DIR:-$RUNTIME/llama.cpp-vulkan}"
DATA="${SAIL_DATA:-/srv/sail-data}"
OUT_DIR="$DATA/mind/bench"
ENV_FILE="$DATA/mind/gpu.env"
THREADS="${SAIL_MODEL_THREADS:-5}"
MODELS=()
SKIP_BUILD=0
DRY_RUN=0
# Experts of this many layers stay on the CPU. 99 = all; lower values move more experts
# onto the GPU until VRAM runs out. llama-bench skips settings that fail to load.
NCMOE_SWEEP="${NCMOE_SWEEP:-99,40,36,32,28,24}"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --model) MODELS+=("${2:?}"); shift 2 ;;
    --skip-build) SKIP_BUILD=1; shift ;;
    --dry-run) DRY_RUN=1; shift ;;
    -h|--help) awk 'NR > 1 && /^#/ { sub(/^# ?/, ""); print; next } NR > 1 { exit }' "$0"; exit 0 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
done

say() { printf '\n== %s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }
run() { if [[ $DRY_RUN == 1 ]]; then printf 'would run:'; printf ' %q' "$@"; printf '\n'; else "$@"; fi; }

# "<id> <file>" per catalog model, primary first, read from the Rust catalog so the list
# here never drifts from Settings > Models.
CATALOG_RS="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/crates/sail-common/src/models.rs"
catalog() {
  python3 - "$CATALOG_RS" <<'CATALOG_PY'
import re, sys
src = open(sys.argv[1]).read()
primary = re.search(r'PRIMARY_MODEL_FILE: &str = "([^"]+)"', src).group(1)
block = src[src.index("pub const CATALOG"):]
for m in re.finditer(r'id: "([^"]+)",\s*name: "[^"]*",\s*file: (PRIMARY_MODEL_FILE|"[^"]+")', block):
    print(m.group(1), primary if m.group(2) == "PRIMARY_MODEL_FILE" else m.group(2).strip('"'))
CATALOG_PY
}
find_weights() {
  local dir
  for dir in "$RUNTIME/models" "$DATA/models"; do
    [[ -f $dir/$1 ]] && { echo "$dir/$1"; return 0; }
  done
  return 1
}
CATALOG_LINES=$(catalog) || die "could not read the model catalog from $CATALOG_RS"
[[ -n $CATALOG_LINES ]] || die "no models parsed from $CATALOG_RS"
# gpu.env is tuned for the model SAIL loads: the saved default, else the primary.
DEFAULT_ID=$(python3 -c 'import json, sys; print(json.load(open(sys.argv[1]))["model"])' \
  "$DATA/mind/default-model.json" 2>/dev/null || true)
DEFAULT_FILE=$(awk -v id="$DEFAULT_ID" '$1 == id { print $2 }' <<<"$CATALOG_LINES")
[[ -n $DEFAULT_FILE ]] || DEFAULT_FILE=$(head -1 <<<"$CATALOG_LINES" | awk '{ print $2 }')

if [[ ${#MODELS[@]} -eq 0 ]]; then
  while read -r _ file; do
    if path=$(find_weights "$file"); then MODELS+=("$path"); fi
  done <<<"$CATALOG_LINES"
fi
[[ ${#MODELS[@]} -gt 0 ]] || die "no catalog models found in $RUNTIME/models or $DATA/models; pass --model PATH"
echo "models to benchmark: ${MODELS[*]##*/}"
echo "gpu.env is tuned for the default model: $DEFAULT_FILE"

# ---------------------------------------------------------------- 1. inventory
say "1. GPU and Vulkan inventory"
lspci -nn 2>/dev/null | grep -Ei 'VGA|Display|3D' || echo "(lspci found no display adapter)"
lspci -nn 2>/dev/null | grep -Eiq 'VGA.*(AMD|ATI)|Display.*(AMD|ATI)' \
  || die "no AMD GPU found by lspci"

render=$(ls /dev/dri/renderD* 2>/dev/null | head -1 || true)
[[ -n $render ]] || die "no /dev/dri/renderD* node: is the amdgpu driver loaded? (lsmod | grep amdgpu)"
if [[ ! -r $render || ! -w $render ]]; then
  grp=$(stat -c %G "$render")
  die "$(id -un) cannot open $render (group $grp). Run: sudo usermod -aG $grp $(id -un), then log in again"
fi
echo "render node: $render (accessible)"

if ! command -v vulkaninfo >/dev/null; then
  die "vulkaninfo missing. Install: sudo apt install vulkan-tools mesa-vulkan-drivers libvulkan-dev glslc spirv-headers cmake build-essential"
fi
vk=$(vulkaninfo --summary 2>/dev/null || true)
echo "$vk" | grep -E 'deviceName|driverName|driverInfo|apiVersion' || true
echo "$vk" | grep -Eq 'deviceName.*(AMD|Radeon|RX)' \
  || die "Vulkan does not report the AMD GPU (only a software device?). Check mesa-vulkan-drivers (RADV)"

# ------------------------------------------------------------------- 2. build
say "2. llama.cpp $LLAMA_TAG with Vulkan in $BUILD_DIR"
SERVER="$BUILD_DIR/build/bin/llama-server"
BENCH="$BUILD_DIR/build/bin/llama-bench"
if [[ $SKIP_BUILD == 0 ]]; then
  for tool in git cmake glslc c++; do
    command -v "$tool" >/dev/null || die "$tool missing. Install: sudo apt install git cmake build-essential glslc libvulkan-dev spirv-headers"
  done
  if [[ -e $BUILD_DIR && ! -w $BUILD_DIR ]]; then
    die "$BUILD_DIR is not writable by $(id -un) (left by a run with sudo?). Run: sudo chown -R $(id -un): $BUILD_DIR"
  fi
  if [[ ! -d $BUILD_DIR/.git ]]; then
    run git clone --depth 1 --branch "$LLAMA_TAG" "$LLAMA_REPO" "$BUILD_DIR"
  else
    run git -C "$BUILD_DIR" fetch --depth 1 origin tag "$LLAMA_TAG"
    run git -C "$BUILD_DIR" checkout --quiet "$LLAMA_TAG"
  fi
  run cmake -S "$BUILD_DIR" -B "$BUILD_DIR/build" -DGGML_VULKAN=ON -DCMAKE_BUILD_TYPE=Release
  run cmake --build "$BUILD_DIR/build" --target llama-server llama-bench -j "$(nproc)"
fi
[[ $DRY_RUN == 1 ]] && { echo "(dry run: stopping before device checks and benchmarks)"; exit 0; }
[[ -x $SERVER && -x $BENCH ]] || die "build output missing: $SERVER / $BENCH"

# ----------------------------------------------------------------- 3. devices
say "3. Devices llama.cpp can use"
devices=$("$SERVER" --list-devices 2>&1 || true)
echo "$devices"
device=$(echo "$devices" | grep -Eo '^ *Vulkan[0-9]+' | head -1 | tr -d ' ')
[[ -n $device ]] || die "llama-server lists no Vulkan device"
echo "$devices" | grep -E "^ *$device" | grep -Eiq 'AMD|Radeon|RX' \
  || die "$device is not the AMD GPU; refusing to benchmark a software device"
vram_mib=$(echo "$devices" | grep -E "^ *$device" | grep -Eo '[0-9]+ MiB' | head -1 | grep -Eo '[0-9]+' || echo "?")
echo "using $device (${vram_mib} MiB reported)"

# ------------------------------------------------------------------ 4. bench
say "4. Benchmarks (stop SAIL first; this takes several minutes per model)"
# Match process names (-x), not command lines: -f also matched compilers and shells whose
# arguments merely mention llama-server, such as a llama.cpp build in progress.
if pgrep -x 'llama-server|sail-core|sail-dialogue|saild' >/dev/null; then
  die "SAIL or llama-server is running; stop it (./run.sh --stop) so the measurement is fair"
fi
mkdir -p "$OUT_DIR"
stamp=$(date +%Y%m%d-%H%M%S)
best_report="$OUT_DIR/$stamp-summary.txt"
: > "$best_report"
for model in "${MODELS[@]}"; do
  name=$(basename "$model" .gguf)
  out="$OUT_DIR/$stamp-$name.json"
  echo "-- $name"
  cpu_json=$("$BENCH" -m "$model" -t "$THREADS" -ngl 0 -dev none -fa 1 -p 512 -n 128 -r 2 -o json 2>/dev/null || echo '[]')
  gpu_json=$("$BENCH" -m "$model" -t "$THREADS" -ngl 99 -dev "$device" -ncmoe "$NCMOE_SWEEP" -fa 1 -p 512 -n 128 -r 2 -o json 2>/dev/null || echo '[]')
  printf '{"model":"%s","device":"%s","vram_mib":"%s","llama_tag":"%s","cpu":%s,"gpu":%s}\n' \
    "$name" "$device" "$vram_mib" "$LLAMA_TAG" "$cpu_json" "$gpu_json" > "$out"
  echo "raw results: $out"
  python3 - "$out" >> "$best_report" <<'PY'
import json, sys
r = json.load(open(sys.argv[1]))
def rate(rows, prompt):
    # llama-bench rows: n_prompt>0 is prompt processing, n_gen>0 is generation.
    key = "n_prompt" if prompt else "n_gen"
    return [x for x in rows if x.get(key, 0) > 0]
cpu_pp = max((x["avg_ts"] for x in rate(r["cpu"], True)), default=0)
cpu_tg = max((x["avg_ts"] for x in rate(r["cpu"], False)), default=0)
best = None
for x in rate(r["gpu"], False):
    n = x.get("n_cpu_moe")
    pp = max((y["avg_ts"] for y in rate(r["gpu"], True) if y.get("n_cpu_moe") == n), default=0)
    if best is None or x["avg_ts"] > best[2]:
        best = (n, pp, x["avg_ts"])
print(f"{r['model']}: cpu pp={cpu_pp:.1f} tg={cpu_tg:.1f} tok/s", end="")
if best is None:
    print("; gpu: no configuration loaded")
    print(f"RESULT {r['model']} none 0 0 {cpu_pp:.2f} {cpu_tg:.2f}")
else:
    n, pp, tg = best
    print(f"; best gpu --n-cpu-moe {n}: pp={pp:.1f} tg={tg:.1f} tok/s")
    print(f"RESULT {r['model']} {n} {pp:.2f} {tg:.2f} {cpu_pp:.2f} {cpu_tg:.2f}")
PY
done

# --------------------------------------------------------------- 5. decide
say "5. Recommendation"
grep -v '^RESULT' "$best_report"
# Tune for the default model SAIL loads. `|| true`: under pipefail a grep with no match
# would otherwise end the script right here without a word.
pick=$(grep "^RESULT ${DEFAULT_FILE%.gguf} " "$best_report" | head -1 || true)
if [[ -z $pick ]]; then
  pick=$(grep '^RESULT' "$best_report" | head -1 || true)
  echo "The default model ($DEFAULT_FILE) was not benchmarked; using the first result instead."
fi
[[ -n $pick ]] || die "no benchmark results in $best_report"
read -r _ model n gpu_pp gpu_tg cpu_pp cpu_tg <<<"$pick"
wins=$(python3 -c "print(int($gpu_pp > $cpu_pp * 1.05 and $gpu_tg > $cpu_tg * 1.05))")
if [[ $n == none || $wins != 1 ]]; then
  echo "GPU offload did not beat CPU-only by at least 5% on both prompt and generation for $model."
  echo "Keeping SAIL on CPU. Nothing written. Measurements are in $OUT_DIR."
  rm -f "$ENV_FILE"
  exit 0
fi
cpu_moe=$([[ $n -ge 99 ]] && echo all || echo "$n")
mkdir -p "$(dirname "$ENV_FILE")"
cat > "$ENV_FILE" <<EOF
# Written by scripts/mind-gpu-setup.sh on $(date -Iseconds) from $pick
# CPU-only: prompt ${cpu_pp} tok/s, generation ${cpu_tg} tok/s
# GPU ($device, experts of $n layers in RAM): prompt ${gpu_pp} tok/s, generation ${gpu_tg} tok/s
# run.sh loads this unless --backend/--cpu is given. Delete it to go back to CPU.
SAIL_MODEL_BACKEND=hybrid
SAIL_VULKAN_DEVICE=$device
SAIL_GPU_LAYERS=99
SAIL_CPU_MOE=$cpu_moe
SAIL_LLAMA_SERVER=$SERVER
EOF
echo "GPU offload wins for $model: wrote $ENV_FILE"
cat "$ENV_FILE"
echo "Start SAIL normally (./run.sh); it now uses the GPU. Re-run this script after driver or model changes."
