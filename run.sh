#!/usr/bin/env bash
# ==============================================================================
# SAIL Core Unified Runner
#
# Launches the complete SAIL assistant runtime with automated TLS certificate
# detection/generation, robust target directory permissions, model discovery,
# and friendly status reporting.
# ==============================================================================
set -euo pipefail

# 1. Resolve repository root
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$SCRIPT_DIR"
if [[ ! -f "$REPO_ROOT/Cargo.toml" && -f "$SCRIPT_DIR/../Cargo.toml" ]]; then
  REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
fi
cd "$REPO_ROOT"

# Defaults
HOST="${SAIL_HOST:-0.0.0.0}"
PORT="${SAIL_PORT:-8088}"
ENABLE_TLS=true
RELEASE=false
KILL_FIRST=false
ACTION="run"
# Keep the model server loaded across SAIL restarts unless --no-persist / SAIL_LLAMA_PERSIST=0.
export SAIL_LLAMA_PERSIST="${SAIL_LLAMA_PERSIST:-1}"
# The sandbox runner reads PDFs (scripts/setup-sail-runner.sh). Use it when it's installed
# and this shell can reach its socket (membership of the `sail` group).
if [[ -z "${SAIL_RUNNER_SOCKET:-}" ]] && id -u sail-runner >/dev/null 2>&1; then
  if [[ -S /run/sail-runner/command.sock && -w /run/sail-runner/command.sock ]]; then
    export SAIL_RUNNER_SOCKET=/run/sail-runner/command.sock
    SAIL_RUNNER_UID="$(id -u sail-runner)"
    export SAIL_RUNNER_UID
  elif [[ -d /run/sail-runner ]]; then
    echo "note: the PDF runner is installed but this shell can't reach it; open a new login shell (group 'sail') and restart SAIL" >&2
  fi
fi
EXTRA_ARGS=()

# 2. Parse command-line flags
while [[ $# -gt 0 ]]; do
  case "$1" in
    --release)
      RELEASE=true
      shift
      ;;
    --debug)
      RELEASE=false
      shift
      ;;
    --http|--no-tls)
      ENABLE_TLS=false
      shift
      ;;
    --tls)
      ENABLE_TLS=true
      shift
      ;;
    --port)
      PORT="$2"
      shift 2
      ;;
    --host)
      HOST="$2"
      shift 2
      ;;
    --kill|--stop)
      ACTION="kill"
      shift
      ;;
    --stop-model)
      ACTION="stop-model"
      shift
      ;;
    --no-persist)
      export SAIL_LLAMA_PERSIST=0
      shift
      ;;
    --restart)
      KILL_FIRST=true
      shift
      ;;
    --status)
      ACTION="status"
      shift
      ;;
    --cert)
      export SAIL_TLS_CERT="$2"
      shift 2
      ;;
    --key)
      export SAIL_TLS_KEY="$2"
      shift 2
      ;;
    --model)
      export SAIL_MODEL_PATH="$2"
      shift 2
      ;;
    --backend)
      export SAIL_MODEL_BACKEND="$2"
      shift 2
      ;;
    --cpu)
      export SAIL_MODEL_BACKEND="cpu"
      shift
      ;;
    --vulkan)
      export SAIL_MODEL_BACKEND="vulkan"
      shift
      ;;
    --hybrid)
      export SAIL_MODEL_BACKEND="hybrid"
      shift
      ;;
    --cpu-moe)
      export SAIL_CPU_MOE="$2"
      shift 2
      ;;
    --gpu-layers|-ngl)
      export SAIL_GPU_LAYERS="$2"
      shift 2
      ;;
    --threads|-t)
      export SAIL_MODEL_THREADS="$2"
      shift 2
      ;;
    --device)
      export SAIL_VULKAN_DEVICE="$2"
      shift 2
      ;;
    --cache-type-k|-ctk)
      export SAIL_CACHE_TYPE_K="$2"
      shift 2
      ;;
    --cache-type-v|-ctv)
      export SAIL_CACHE_TYPE_V="$2"
      shift 2
      ;;
    --no-flash-attn)
      export SAIL_FLASH_ATTN="0"
      shift
      ;;
    --cache-reuse)
      export SAIL_CACHE_REUSE="$2"
      shift 2
      ;;
    -h|--help)
      cat << 'EOF'
Usage: ./run.sh [OPTIONS] [-- CARGO_ARGS...]

Runs the complete SAIL Core assistant with voice dialogue, Guardian security,
and local inference. Automatically provisions and passes TLS certificates.

Options:
  --release            Run optimized release build (cargo run --release -p sail-core)
  --debug              Run debug build (default)
  --http, --no-tls     Disable TLS and bind plain HTTP (microphone restricted on LAN)
  --port <port>        Set listening port (default: 8088, or $SAIL_PORT)
  --host <host>        Set listening host (default: 0.0.0.0, or $SAIL_HOST)
  --cert <path>        Custom TLS certificate file (defaults to .local/tls/voice-cert.pem)
  --key <path>         Custom TLS private key file (defaults to .local/tls/voice-key.pem)
  --model <path>       Specify model GGUF path
  --backend <mode>     Compute backend (cpu, vulkan, hybrid; default: cpu)
  --cpu                Shorthand for --backend cpu
  --vulkan             Shorthand for --backend vulkan
  --hybrid             Shorthand for --backend hybrid (GPU + CPU hybrid offloading)
  --gpu-layers, -ngl N Number of layers to offload to GPU in vulkan/hybrid mode
  --cpu-moe <all|N|off> Keep MoE expert weights of all/N layers in RAM (use with -ngl 99)
  --threads, -t N      Number of CPU generation/batch threads (default: min(cores, 6))
  --device <dev>       Compute device identifier (e.g. Vulkan0)
  --cache-type-k, -ctk KV cache K quant type (q4_0, q8_0, f16; default: q4_0)
  --cache-type-v, -ctv KV cache V quant type (q4_0, q8_0, f16; default: q4_0)
  --no-flash-attn      Disable Flash Attention (enabled by default)
  --cache-reuse N      Chunk size for KV shifting/cache reuse (default: 256)
  --restart            Stop running SAIL before launching (a warm model stays loaded)
  --stop, --kill       Stop SAIL and the model server, freeing its memory, and exit
  --stop-model         Stop only the model server (SAIL reloads it on next use) and exit
  --no-persist         Stop the model server together with SAIL (old behaviour)
  --status             Display active SAIL processes and model server, then exit
  -h, --help           Show this help message

Examples:
  ./run.sh                  # Standard HTTPS start with auto-provisioned .pem certs
  ./run.sh --release        # Run in release mode
  ./run.sh --restart        # Terminate existing instance and start fresh
  ./run.sh --port 8090      # Run on custom port 8090
  ./run.sh --stop           # Stop SAIL and unload the model
EOF
      exit 0
      ;;
    --)
      shift
      EXTRA_ARGS=("$@")
      break
      ;;
    *)
      EXTRA_ARGS+=("$1")
      shift
      ;;
  esac
done

# 3. Process management helpers
# The model server (llama-server) is kept warm between runs: Ctrl+C stops SAIL but leaves
# the model loaded, and the next ./run.sh adopts it in seconds. It is recorded in
# .local/llama-server.<port>.json; SAIL replaces it by itself if the launch settings change.
MODEL_PORTS=(8081 8080)
model_server_pids() {
  pgrep -x "llama-server" 2>/dev/null || true
}
stop_sail() {
  echo "Stopping SAIL..."
  pkill -x "sail-core|sail-dialogue" 2>/dev/null || true
  if command -v fuser >/dev/null 2>&1; then
    fuser -k "${PORT}/tcp" 2>/dev/null || true
  fi
}
stop_model() {
  local pids
  pids="$(model_server_pids)"
  if [[ -z "$pids" ]]; then
    echo "Model server is not running."
  else
    echo "Stopping model server (pid $(echo $pids | tr ' ' ',')) and freeing its memory..."
    kill $pids 2>/dev/null || true
    for _ in $(seq 1 50); do
      [[ -z "$(model_server_pids)" ]] && break
      sleep 0.2
    done
    pids="$(model_server_pids)"
    [[ -n "$pids" ]] && kill -9 $pids 2>/dev/null || true
  fi
  if command -v fuser >/dev/null 2>&1; then
    for model_port in "${MODEL_PORTS[@]}"; do fuser -k "${model_port}/tcp" 2>/dev/null || true; done
  fi
  rm -f "$REPO_ROOT"/.local/llama-server.*.json
}
model_status() {
  local pids
  pids="$(model_server_pids)"
  if [[ -z "$pids" ]]; then
    echo "Model server: not running (loads on first use)"
  elif curl -fsS --max-time 2 "http://127.0.0.1:${MODEL_PORTS[0]}/health" >/dev/null 2>&1; then
    echo "Model server: warm and ready on port ${MODEL_PORTS[0]} (pid $(echo $pids | tr ' ' ',')) · stop with ./run.sh --stop-model"
  else
    echo "Model server: running, still loading (pid $(echo $pids | tr ' ' ','))"
  fi
}

if [[ "$ACTION" == "kill" ]]; then
  stop_sail
  stop_model
  sleep 0.5
  echo "Stopped."
  exit 0
fi

if [[ "$ACTION" == "stop-model" ]]; then
  stop_model
  exit 0
fi

if [[ "$ACTION" == "status" ]]; then
  echo "Checking active SAIL processes:"
  if pgrep -a -x "sail-core|sail-dialogue"; then
    echo "SAIL: Running"
  else
    echo "SAIL: not running"
  fi
  model_status
  exit 0
fi

if [[ "$KILL_FIRST" == "true" ]]; then
  stop_sail
  [[ "$SAIL_LLAMA_PERSIST" == "1" ]] || stop_model
  sleep 0.5
fi

# 4. Check for port conflicts
if command -v fuser >/dev/null 2>&1; then
  if fuser "${PORT}/tcp" 2>/dev/null | grep -q '[0-9]'; then
    echo "Error: Port ${PORT} is already in use by another process."
    echo "Run './run.sh --restart' to terminate it, or './run.sh --port <port>' to choose another port."
    exit 1
  fi
fi

# 5. Guard CARGO_TARGET_DIR against invalid or unwritable target directories, and keep
#    build artifacts on the runtime SSD by default. A 54 GB debug `target/` under the
#    repository once filled the 116 GB root filesystem (vision §13.1); `/srv/sail` exists
#    for exactly this. An explicit CARGO_TARGET_DIR always wins.
if [[ -n "${CARGO_TARGET_DIR:-}" ]]; then
  if ! mkdir -p "$CARGO_TARGET_DIR" 2>/dev/null || [[ ! -w "$CARGO_TARGET_DIR" ]]; then
    echo "Notice: Configured CARGO_TARGET_DIR ($CARGO_TARGET_DIR) is not writable."
    echo "Falling back to local $REPO_ROOT/target."
    export CARGO_TARGET_DIR="$REPO_ROOT/target"
  fi
else
  RUNTIME_TARGET="${SAIL_RUNTIME:-/srv/sail}/cargo-target"
  if mkdir -p "$RUNTIME_TARGET" 2>/dev/null && [[ -w "$RUNTIME_TARGET" ]]; then
    export CARGO_TARGET_DIR="$RUNTIME_TARGET"
  else
    export CARGO_TARGET_DIR="$REPO_ROOT/target"
  fi
fi
echo "Using CARGO_TARGET_DIR=$CARGO_TARGET_DIR"

# 6. Configure TLS and certificates
if [[ "$ENABLE_TLS" == "true" ]]; then
  CERT_PATH="${SAIL_TLS_CERT:-$REPO_ROOT/.local/tls/voice-cert.pem}"
  KEY_PATH="${SAIL_TLS_KEY:-$REPO_ROOT/.local/tls/voice-key.pem}"

  if [[ ! -f "$CERT_PATH" || ! -f "$KEY_PATH" ]]; then
    echo "TLS certificates not found. Generating self-signed certificate in $REPO_ROOT/.local/tls/ ..."
    mkdir -p "$(dirname "$CERT_PATH")"
    LAN_IP=$(hostname -I 2>/dev/null | awk '{print $1}' || echo "127.0.0.1")
    openssl req -x509 -newkey rsa:2048 -nodes -days 365 \
      -keyout "$KEY_PATH" -out "$CERT_PATH" \
      -subj "/CN=SAIL-local" \
      -addext "subjectAltName=DNS:localhost,IP:127.0.0.1,IP:${LAN_IP:-127.0.0.1}" \
      -addext 'basicConstraints=critical,CA:TRUE' \
      -addext 'keyUsage=critical,digitalSignature,keyEncipherment,keyCertSign' 2>/dev/null
    chmod 600 "$KEY_PATH"
    echo "Generated $CERT_PATH and $KEY_PATH"
  fi

  export SAIL_TLS_CERT="$CERT_PATH"
  export SAIL_TLS_KEY="$KEY_PATH"
  PROTOCOL="https"
else
  unset SAIL_TLS_CERT
  unset SAIL_TLS_KEY
  PROTOCOL="http"
fi

# 7. Bind address
export SAIL_VOICE_BIND="${HOST}:${PORT}"

# 8. The one resident model (see sail_common::models). No fallback to another model:
# SAIL keeps a single large model that voice, agents and Forge share. A default chosen in
# Settings > Models is saved in $SAIL_DATA/mind/default-model.json and resolved by SAIL
# itself, so this only pins the primary model when no default has been saved.
DEFAULT_MODEL_FILE="${SAIL_DATA:-/srv/sail-data}/mind/default-model.json"
if [[ -z "${SAIL_MODEL_PATH:-}" && -f "$DEFAULT_MODEL_FILE" ]]; then
  echo "Default model: saved choice in $DEFAULT_MODEL_FILE"
elif [[ -z "${SAIL_MODEL_PATH:-}" ]]; then
  for cand in \
    "$REPO_ROOT/models/Qwen3.6-35B-A3B-Q4_K_M.gguf" \
    "${SAIL_RUNTIME:-/srv/sail}/models/Qwen3.6-35B-A3B-Q4_K_M.gguf"; do
    if [[ -f "$cand" ]]; then
      export SAIL_MODEL_PATH="$cand"
      break
    fi
  done
fi

# 8b. Measured GPU offload. scripts/mind-gpu-setup.sh writes this file only when GPU
# offload beat CPU-only on this host. An explicit --cpu/--vulkan/--hybrid/--backend (or a
# SAIL_MODEL_BACKEND already in the environment) wins; delete the file to go back to CPU.
GPU_ENV="${SAIL_DATA:-/srv/sail-data}/mind/gpu.env"
if [[ -z "${SAIL_MODEL_BACKEND:-}" && -f "$GPU_ENV" ]]; then
  while IFS='=' read -r key value; do
    case "$key" in
      SAIL_MODEL_BACKEND|SAIL_VULKAN_DEVICE|SAIL_GPU_LAYERS|SAIL_CPU_MOE|SAIL_LLAMA_SERVER)
        [[ -z "${!key:-}" ]] && export "$key=$value" ;;
    esac
  done < <(grep -E '^[A-Z_]+=' "$GPU_ENV")
  GPU_ENV_LOADED=1
fi

# Set default context tokens (16k for Qwen 3.6 on 32GB systems)
export SAIL_CONTEXT_TOKENS="${SAIL_CONTEXT_TOKENS:-16384}"

# 9. Startup summary banner
LAN_IP=$(hostname -I 2>/dev/null | awk '{print $1}' || echo "127.0.0.1")
echo "=================================================="
echo " Starting SAIL Core Assistant"
echo "=================================================="
echo " Build mode:   $([ "$RELEASE" = "true" ] && echo "Release (--release)" || echo "Debug")"
echo " Bind address: $SAIL_VOICE_BIND"
echo " Protocol:     $PROTOCOL"
if [[ "$ENABLE_TLS" == "true" ]]; then
  echo " TLS Cert:     $SAIL_TLS_CERT"
  echo " TLS Key:      $SAIL_TLS_KEY"
fi
if [[ -n "${SAIL_MODEL_PATH:-}" ]]; then
  echo " LLM Model:    $SAIL_MODEL_PATH"
fi
echo " Backend:      ${SAIL_MODEL_BACKEND:-cpu}$([ "${SAIL_MODEL_BACKEND:-cpu}" != "cpu" ] && echo " (${SAIL_VULKAN_DEVICE:-Vulkan0}, GPU layers: ${SAIL_GPU_LAYERS:-8}, MoE experts on CPU: ${SAIL_CPU_MOE:-off})" || true)$([ "${GPU_ENV_LOADED:-0}" = 1 ] && echo " [from $GPU_ENV]" || true)"
echo " Acceleration: FlashAttention=$([ "${SAIL_FLASH_ATTN:-1}" != "0" ] && echo "on" || echo "off"), KV-Cache=${SAIL_CACHE_TYPE_K:-q4_0}/${SAIL_CACHE_TYPE_V:-q4_0}, CacheReuse=${SAIL_CACHE_REUSE:-256}"
if [[ -f "$REPO_ROOT/.local/sail_owner_token" ]]; then
  echo " Owner token:  Configured (.local/sail_owner_token)"
fi
echo " Web UI (loc): $PROTOCOL://localhost:${PORT}"
echo " Web UI (LAN): $PROTOCOL://${LAN_IP}:${PORT}"
if [[ "$SAIL_LLAMA_PERSIST" == "1" ]]; then
  model_status | sed 's/^Model server:/ Model:       /'
  echo " Stop SAIL:    Ctrl+C (the model stays loaded for a fast next start)"
  echo " Stop all:     ./run.sh --stop  (also unloads the model and frees its memory)"
else
  echo " Stop server:  Press Ctrl+C (or run ./run.sh --stop)"
fi
echo "=================================================="

# 10. Execute cargo run replacing shell process
CARGO_CMD=("cargo" "run")
if [[ "$RELEASE" == "true" ]]; then
  CARGO_CMD+=("--release")
fi
CARGO_CMD+=("-p" "sail-core")

if [[ ${#EXTRA_ARGS[@]} -gt 0 ]]; then
  CARGO_CMD+=("--" "${EXTRA_ARGS[@]}")
fi

exec "${CARGO_CMD[@]}"
