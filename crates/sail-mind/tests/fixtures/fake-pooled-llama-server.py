#!/usr/bin/env python3
"""Loopback-only fixture for the pooled llama-server lifecycle."""
import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
import socket
import sys

args = sys.argv[1:]
if "--list-devices" in args:
    print("Available devices:")
    if "--rpc" in args:
        host, port = args[args.index("--rpc") + 1].rsplit(":", 1)
        assert host == "127.0.0.1"
        with socket.create_connection((host, int(port)), timeout=2) as peer:
            peer.sendall(b"RPC")
            assert peer.recv(3, socket.MSG_WAITALL) == b"RPC"
        print("  RPC0: Fixture CPU (8192 MiB, 8192 MiB free)")
    sys.exit(0)

model = Path(args[args.index("-m") + 1])
with model.with_suffix(".launches").open("a", encoding="utf-8") as log:
    log.write("launch\n")
if model.stem == "crash":
    sys.exit(3)


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/health":
            self.reply({"status": "ok"})
        elif self.path == "/props":
            self.reply({"model_path": str(model)})
        elif self.path == "/metrics" and "--metrics" in args:
            self.send_body(b"llamacpp:predicted_tokens_seconds 7.4\n", "text/plain")
        else:
            self.send_error(404)

    def do_POST(self):
        if self.path != "/v1/chat/completions":
            self.send_error(404)
            return
        self.rfile.read(int(self.headers.get("Content-Length", "0")))
        self.reply({
            "choices": [{"message": {"content": "fixture answer"}}],
            "timings": {"predicted_per_second": 7.4},
        })

    def reply(self, value):
        self.send_body(json.dumps(value).encode(), "application/json")

    def send_body(self, body, content_type):
        self.send_response(200)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_):
        pass


with ThreadingHTTPServer(("127.0.0.1", int(args[args.index("--port") + 1])), Handler) as server:
    server.serve_forever()
