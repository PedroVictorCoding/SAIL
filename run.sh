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

# 5. Guard CARGO_TARGET_DIR against invalid or unwritable target directories
if [[ -n "${CARGO_TARGET_DIR:-}" ]]; then
  if ! mkdir -p "$CARGO_TARGET_DIR" 2>/dev/null || [[ ! -w "$CARGO_TARGET_DIR" ]]; then
    echo "Notice: Configured CARGO_TARGET_DIR ($CARGO_TARGET_DIR) is not writable."
    echo "Falling back to local $REPO_ROOT/target."
    export CARGO_TARGET_DIR="$REPO_ROOT/target"
  fi
else
  export CARGO_TARGET_DIR="$REPO_ROOT/target"
fi

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

# 8. Model auto-discovery if not explicitly set
if [[ -z "${SAIL_MODEL_PATH:-}" ]]; then
  for cand in \
    "$REPO_ROOT/models/Qwen3.6-35B-A3B-Q4_K_M.gguf" \
    "/srv/sail/models/Qwen3.6-35B-A3B-Q4_K_M.gguf" \
    "$REPO_ROOT/models/Qwen3-Coder-30B-A3B-Instruct-Q4_K_M.gguf" \
    "/srv/sail/models/Qwen3-Coder-30B-A3B-Instruct-Q4_K_M.gguf" \
    "$REPO_ROOT/models/gemma4-v2-Q4_K_M.gguf"; do
    if [[ -f "$cand" ]]; then
      export SAIL_MODEL_PATH="$cand"
      break
    fi
  done
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
