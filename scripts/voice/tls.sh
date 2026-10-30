#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."
VOICE_IP=${1:-127.0.0.1}
[[ "$VOICE_IP" =~ ^[0-9a-fA-F:.]+$ ]] || { echo 'Pass a valid IP address'; exit 1; }
mkdir -p .local/tls
openssl req -x509 -newkey rsa:2048 -nodes -days 365 \
  -keyout .local/tls/voice-key.pem -out .local/tls/voice-cert.pem \
  -subj /CN=SAIL-local -addext "subjectAltName=DNS:localhost,IP:127.0.0.1,IP:$VOICE_IP" \
  -addext 'basicConstraints=critical,CA:TRUE' \
  -addext 'keyUsage=critical,digitalSignature,keyEncipherment,keyCertSign'
chmod 600 .local/tls/voice-key.pem
printf 'Trust .local/tls/voice-cert.pem on each client, then run:\nSAIL_VOICE_BIND=0.0.0.0:8088 SAIL_TLS_CERT=.local/tls/voice-cert.pem SAIL_TLS_KEY=.local/tls/voice-key.pem cargo run -p sail-core\n'
