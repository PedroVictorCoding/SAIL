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
  --restart            Stop any running SAIL processes before launching
  --stop, --kill       Stop running SAIL and llama processes and exit
  --status             Display active SAIL processes and exit
  -h, --help           Show this help message

Examples:
  ./run.sh                  # Standard HTTPS start with auto-provisioned .pem certs
  ./run.sh --release        # Run in release mode
  ./run.sh --restart        # Terminate existing instance and start fresh
  ./run.sh --port 8090      # Run on custom port 8090
  ./run.sh --kill           # Stop all running instances
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
kill_sail_processes() {
  echo "Stopping active SAIL and llama processes..."
  pkill -f "sail-core|sail-dialogue|llama-server" 2>/dev/null || true
  if command -v fuser >/dev/null 2>&1; then
    fuser -k "${PORT}/tcp" 2>/dev/null || true
    fuser -k "8080/tcp" 2>/dev/null || true
    fuser -k "8081/tcp" 2>/dev/null || true
  fi
  sleep 0.5
  echo "Stopped."
}

if [[ "$ACTION" == "kill" ]]; then
  kill_sail_processes
  exit 0
fi

if [[ "$ACTION" == "status" ]]; then
  echo "Checking active SAIL processes:"
  if pgrep -la -f "sail-core|sail-dialogue|llama-server"; then
    echo "Status: Running"
  else
    echo "Status: No running SAIL processes found."
  fi
  exit 0
fi

if [[ "$KILL_FIRST" == "true" ]]; then
  kill_sail_processes
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
# SAIL keeps a single large model that voice, agents and Forge share.
if [[ -z "${SAIL_MODEL_PATH:-}" ]]; then
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
echo " Stop server:  Press Ctrl+C (or run ./run.sh --stop)"
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
