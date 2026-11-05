"""Measure generated voice_prompt_cases JSON against one existing local model.

Usage: python prefill_bench.py cases.json [http://127.0.0.1:8081]
Runs a cold before/after arithmetic pair and candidate tool/confirmation probes.
Tool calls are inspected only, never executed. Results go to stdout as JSON lines.
"""
import json
import sys
import time
import urllib.request

cases = json.load(open(sys.argv[1]))
endpoint = sys.argv[2] if len(sys.argv) > 2 else "http://127.0.0.1:8081"
for case in cases:
    variants = ["before", "after"] if case["name"] == "short_answer" else ["after"]
    for variant in variants:
        body = {
            "model": "local", "messages": case[variant], "stream": False,
            "max_tokens": 160, "temperature": 0, "cache_prompt": False,
            "chat_template_kwargs": {"enable_thinking": False},
        }
        if case["tools"]:
            body.update(tools=case["tools"], tool_choice="auto")
        started = time.monotonic()
        request = urllib.request.Request(
            endpoint + "/v1/chat/completions", data=json.dumps(body).encode(),
            headers={"Content-Type": "application/json"},
        )
        with urllib.request.urlopen(request, timeout=300) as response:
            result = json.load(response)
        print(json.dumps({
            "case": case["name"], "variant": variant,
            "elapsed_s": round(time.monotonic() - started, 2),
            "usage": result.get("usage"), "timings": result.get("timings"),
            "answer": result["choices"][0]["message"],
            "finish_reason": result["choices"][0].get("finish_reason"),
        }), flush=True)
