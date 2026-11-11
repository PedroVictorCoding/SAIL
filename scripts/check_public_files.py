"""Check Git-tracked publication content; report filenames, never secret values."""
import re
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
FORBIDDEN = re.compile(r"(?:^|/)(?:\.env(?:\..*)?|id_rsa|id_ed25519|credentials\.json|service-account[^/]*\.json)$|\.(?:pem|key|p12|pfx|keystore|jks|db|sqlite|sqlite3)(?:-(?:shm|wal))?$|(?:^|/)(?:secrets|\.secrets|\.aws|\.ssh|\.local|models|logs|\.venv|node_modules)/", re.I)
# Exact synthetic fixture used to verify Guardian token isolation; no path-wide exemption.
TEST_TOKEN = b"ghp_abcdefghijklmnopqrstuvwxyz0123456789"
SECRETS = [
    re.compile(rb"-----BEGIN (?:RSA |EC |DSA |OPENSSH |ENCRYPTED )?PRIVATE KEY-----"),
    re.compile(rb"(?:gh[pousr]_[A-Za-z0-9]{30,}|github_pat_[A-Za-z0-9_]{40,})"),
    re.compile(rb"AKIA[A-Z0-9]{16}"),
    re.compile(rb"sk-(?:proj-)?[A-Za-z0-9_-]{40,}"),
]

def main():
    names = subprocess.check_output(["git", "ls-files", "-z"], cwd=ROOT).decode().split("\0")
    problems = []
    for name in filter(None, names):
        path = ROOT / name
        if FORBIDDEN.search(name) and not name.endswith((".example", ".sample")):
            problems.append((name, "private/local file path"))
        if path.is_file() and any(pattern.search(path.read_bytes().replace(TEST_TOKEN, b"SYNTHETIC_TOKEN")) for pattern in SECRETS):
            problems.append((name, "possible credential or private key"))
    for name, reason in problems:
        print(f"BLOCKED: {name}: {reason}")
    print(f"Publication check: {len(problems)} finding(s).")
    return bool(problems)

if __name__ == "__main__":
    raise SystemExit(main())
