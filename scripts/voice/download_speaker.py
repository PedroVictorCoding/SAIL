#!/usr/bin/env python3
"""Explicit, authenticated setup download. Runtime speaker workers remain offline."""
import argparse
import getpass
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import shlex
import shutil
import sys

REPO = 'pyannote/speaker-diarization-community-1'
MODEL_URL = f'https://huggingface.co/{REPO}'
MANIFEST = '.sail-speaker-model.json'
STATE = '.sail-download-state.json'
RESERVE = 512 * 1024 * 1024


class SetupError(Exception):
    """Only our controlled messages are safe to display."""



def file_path(root, name):
    relative = PurePosixPath(name)
    if relative.is_absolute() or not relative.parts or '..' in relative.parts or '\\' in name:
        raise SetupError('Unsafe model file path')
    path = root.joinpath(*relative.parts)
    if not path.resolve().is_relative_to(root.resolve()):
        raise SetupError('Model file escapes its destination')
    current = path
    while current != root:
        if current.is_symlink():
            raise SetupError('Symlinked model files are not accepted')
        current = current.parent
    return path


def fingerprint(path):
    digest = hashlib.sha256()
    with path.open('rb') as source:
        first = source.read(1024 * 1024)
        if first.startswith(b'version https://git-lfs.github.com/spec/v1'):
            raise SetupError('Found a Git LFS pointer instead of model data')
        digest.update(first)
        while chunk := source.read(1024 * 1024):
            digest.update(chunk)
    return digest.hexdigest()


def write_json(path, value):
    temporary = path.with_name(path.name + '.tmp')
    # Our dedicated staging directory is private and never a symlink.
    with temporary.open('w', encoding='utf-8') as output:
        json.dump(value, output, indent=2)
        output.write('\n')
    temporary.replace(path)


def verify(destination, revision=None):
    if destination.is_symlink():
        raise SetupError('Destination cannot be a symlink')
    manifest_path = destination / MANIFEST
    if not manifest_path.is_file() or manifest_path.stat().st_size > 1024 * 1024:
        raise SetupError('Destination exists without a valid SAIL manifest; choose a new directory')
    manifest = json.loads(manifest_path.read_text())
    pinned = manifest.get('revision', '')
    if manifest.get('repo') != REPO or not re.fullmatch(r'[a-f0-9]{40}', pinned):
        raise SetupError('Invalid model manifest')
    if revision and revision != pinned:
        raise SetupError('Destination contains another revision; use a new destination')
    files = manifest.get('files', [])
    if not files or not any(f['path'] == 'config.yaml' for f in files):
        raise SetupError('Incomplete model manifest')
    for entry in files:
        path = file_path(destination, entry['path'])
        if not path.is_file() or path.stat().st_size != entry['size'] or fingerprint(path) != entry['sha256']:
            raise SetupError('Model verification failed; a file is missing or changed')
    return manifest


def download(destination, api, snapshot, token, revision=None):
    """Dependency injection lets tests verify the real flow without downloading weights."""
    if destination.is_symlink():
        raise SetupError('Destination cannot be a symlink')
    if destination.exists():
        return verify(destination, revision)
    # Check real authorization before making local directories or downloading weights.
    api.auth_check(repo_id=REPO, repo_type='model', token=token)
    info = api.model_info(REPO, revision=revision or 'main', files_metadata=True, token=token)
    pinned = info.sha
    if not re.fullmatch(r'[a-f0-9]{40}', pinned or ''):
        raise SetupError('Hub did not return an immutable commit revision')
    siblings = info.siblings or []
    if not siblings or not any(s.rfilename == 'config.yaml' for s in siblings):
        raise SetupError('Model repository has no pipeline config')
    if any(s.size is None or s.size < 0 for s in siblings):
        raise SetupError('Hub did not report complete file sizes; refusing an unbounded download')
    destination.parent.mkdir(parents=True, exist_ok=True)
    staging = destination.with_name(f'.{destination.name}.download-{pinned[:12]}')
    if staging.is_symlink():
        raise SetupError('Staging directory cannot be a symlink')
    for sibling in siblings:
        file_path(staging, sibling.rfilename)
        if sibling.rfilename in {STATE, MANIFEST}:
            raise SetupError('Repository conflicts with setup metadata')
    required = sum(s.size for s in siblings) + RESERVE
    if shutil.disk_usage(destination.parent).free < required:
        raise SetupError(f'Insufficient free space: need at least {required // (1024*1024)} MiB including headroom')
    identity = {'repo': REPO, 'revision': pinned}
    if staging.exists():
        if not (staging / STATE).is_file() or json.loads((staging / STATE).read_text()) != identity:
            raise SetupError('Staging directory is not this download; choose a new destination')
    else:
        staging.mkdir(mode=0o700)
        write_json(staging / STATE, identity)
    snapshot(repo_id=REPO, repo_type='model', revision=pinned, local_dir=str(staging), token=token, max_workers=2)
    files = []
    for sibling in siblings:
        path = file_path(staging, sibling.rfilename)
        if not path.is_file() or path.stat().st_size != sibling.size:
            raise SetupError('Downloaded model is incomplete; rerun to resume')
        digest = fingerprint(path)
        lfs = getattr(sibling, 'lfs', None)
        expected = lfs.get('sha256') if isinstance(lfs, dict) else getattr(lfs, 'sha256', None)
        if expected and digest != expected:
            raise SetupError('Model file does not match the Hub LFS checksum')
        files.append({'path': sibling.rfilename, 'size': sibling.size, 'sha256': digest})
    manifest = {**identity, 'files': files}
    write_json(staging / MANIFEST, manifest)
    verify(staging, pinned)
    # No incomplete directory is ever advertised as the active model.
    if destination.exists():
        raise SetupError('Destination appeared during download; staging was preserved')
    staging.rename(destination)
    return manifest


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--destination', type=Path, help='New writable model directory, preferably on runtime storage')
    parser.add_argument('--revision', help='Full immutable Hub commit SHA; default pins the current main once')
    parser.add_argument('--check-access', action='store_true', help='Check model access without downloading or writing files')
    parser.add_argument('--verify', action='store_true', help='Verify an existing download offline; no Hub dependency required')
    parser.add_argument('--prompt-token', action='store_true', help='Read a token with hidden terminal input; never save it')
    args = parser.parse_args(argv)
    if args.revision and not re.fullmatch(r'[a-f0-9]{40}', args.revision):
        parser.error('--revision must be a full 40-character lowercase commit SHA')
    if not args.check_access and args.destination is None:
        parser.error('--destination is required unless using --check-access')
    if args.verify and args.check_access:
        parser.error('--verify and --check-access are separate operations')
    destination = args.destination.expanduser().absolute() if args.destination else None
    try:
        if args.verify or destination and destination.exists() and not args.check_access:
            manifest = verify(destination, args.revision)
        else:
            try:
                from huggingface_hub import HfApi, snapshot_download, get_token
                from huggingface_hub.errors import GatedRepoError, HfHubHTTPError
            except ImportError:
                print('Install huggingface-hub in your setup environment first (the existing voice environment already includes it).', file=sys.stderr)
                return 2
            token = get_token()
            if args.prompt_token:
                if not sys.stdin.isatty():
                    raise SetupError('--prompt-token needs an interactive terminal; otherwise use hf auth login or HF_TOKEN')
                token = getpass.getpass('Hugging Face read token (not saved): ').strip()
            try:
                api = HfApi()
                if args.check_access:
                    api.auth_check(repo_id=REPO, repo_type='model', token=token)
                    print('Model access confirmed. No weights downloaded.')
                    return 0
                manifest = download(destination, api, snapshot_download, token, args.revision)
            except GatedRepoError:
                print(f'Access is gated. Sign in and accept/request access at {MODEL_URL}, then authenticate using hf auth login or --prompt-token. A download script cannot grant account access.', file=sys.stderr)
                return 3
            except HfHubHTTPError as error:
                code = getattr(getattr(error, 'response', None), 'status_code', None)
                print(f'Hub access/download failed (HTTP {code or "unknown"}). Check your read-token access and model terms at {MODEL_URL}. Tokens and raw server errors are not printed.', file=sys.stderr)
                return 3
        print(f'Offline model verified at commit {manifest["revision"]}.')
        print(f'export SAIL_SPEAKER_MODEL_DIR={shlex.quote(str(destination))}')
        print('The optional speaker Python environment is installed separately. SAIL was not restarted.')
        return 0
    except KeyboardInterrupt:
        print('Download interrupted. Rerun the same command to resume staging.', file=sys.stderr)
        return 130
    except SetupError as error:
        print(str(error), file=sys.stderr)
        return 2
    except Exception:
        print('Setup failed. Check network access, directory permissions, manifest integrity, and the installed huggingface-hub version. Staging is preserved; credentials and raw exception details are not printed.', file=sys.stderr)
        return 2



if __name__ == '__main__':
    raise SystemExit(main())
