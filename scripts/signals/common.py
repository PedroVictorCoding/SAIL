"""Shared plumbing for SAIL Capital model-signal helpers (round 12).

Protocol (see crates/sail-capital/src/models/mod.rs): one JSON request on stdin, one JSON
reply on stdout. Helpers only ever use bars up to each origin.
"""

import json
import sys


def read_request():
    return json.load(sys.stdin)


def reply(obj):
    sys.stdout.write(json.dumps(obj, separators=(",", ":")))
    sys.stdout.flush()


def fail(message):
    reply({"error": message})
    return 1


def cut(bars, origin):
    """Bars up to and including `origin` (bars are oldest first)."""
    out = []
    for bar in bars:
        if bar[0] > origin:
            break
        out.append(bar)
    return out
