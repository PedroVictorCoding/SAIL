#!/usr/bin/env python3
"""A stand-in for ggml-rpc-server: prints the real startup shape, then echoes bytes."""
import socket
import sys

args = sys.argv[1:]
port = 50052
if "-p" in args:
    port = int(args[args.index("-p") + 1])

print("Starting RPC server v9.9.9", flush=True)
print(f"  endpoint       : 127.0.0.1:{port}", flush=True)
print("  local cache    : n/a", flush=True)
print("Devices:", flush=True)
print("  Vulkan0: Fake GPU (16384 MiB, 16000 MiB free)", flush=True)
print("  CPU: Fake CPU (65536 MiB, 60000 MiB free)", flush=True)
print("  transport      : TCP", flush=True)

server = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
server.bind(("127.0.0.1", port))
server.listen(16)
while True:
    client, _ = server.accept()
    client.settimeout(0.2)
    try:
        while True:
            data = client.recv(65536)
            if not data:
                break
            client.sendall(data)
    except OSError:
        pass
    client.close()
