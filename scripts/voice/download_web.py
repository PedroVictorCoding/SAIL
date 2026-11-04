"""Download the in-browser speech chain: the same Silero VAD, Whisper base and Kokoro
models the host workers use, as ONNX for transformers.js, plus the pinned runtimes.

Setup-time only. The browser never downloads from the internet: SAIL serves these
files from its own origin under /voice-assets/ (see crates/sail-dialogue/src/web_assets.rs).
Every file is pinned to an exact revision and SHA-256 and written atomically; a file
already present with the right hash is kept.

    python3 scripts/voice/download_web.py [--dest DIR]

Default destination: $SAIL_VOICE_WEB_DIR, else $SAIL_RUNTIME/models/voice/web
(SAIL_RUNTIME defaults to /srv/sail). About 0.7 GB.
"""
import argparse
import hashlib
import json
import os
import sys
import urllib.request
from pathlib import Path

TRANSFORMERS = 'https://cdn.jsdelivr.net/npm/@huggingface/transformers@3.5.1/dist/'
KOKORO_JS = 'https://cdn.jsdelivr.net/npm/kokoro-js@1.2.1/dist/'
WHISPER = ('onnx-community/whisper-base', '27a07b4b52b3a4c5886134bb4320e93e1a4bbc7c')
KOKORO = ('onnx-community/Kokoro-82M-v1.0-ONNX', '1939ad2a8e416c0acfeecc08a694d14ef25f2231')
SILERO = ('onnx-community/silero-vad', 'e71cae966052b992a7eca6b17738916ce0eca4ec')

# (destination relative to the root, source URL, sha256)
LIB = [
    # transformers.js 3.5.1 bundles ONNX Runtime Web; kokoro-js 1.2.1 bundles the same
    # transformers.js 3.5.1, so one set of runtime files serves both.
    ('lib/transformers.min.js', TRANSFORMERS + 'transformers.min.js',
     '79fad8c1c2fd165bf1e77ace20ce023fab4dc13b6621f0e586c9b8c59c4d09d8'),
    ('lib/ort-wasm-simd-threaded.jsep.mjs', TRANSFORMERS + 'ort-wasm-simd-threaded.jsep.mjs',
     '08fb86ec433c78bfb032c5d84a68b8e8e5a8d81268fa39e24314179a5767a5b9'),
    ('lib/ort-wasm-simd-threaded.jsep.wasm', TRANSFORMERS + 'ort-wasm-simd-threaded.jsep.wasm',
     'c46655e8a94afc45338d4cb2b840475f88e5012d524509916e505079c00bfa39'),
    ('lib/kokoro.web.js', KOKORO_JS + 'kokoro.web.js',
     '6067712fe4c43cdb36c762f860a5ab8d96ea1d9c8882466438eb9f3d0d20be8f'),
]


def hf(repo_rev, path, sha256):
    repo, rev = repo_rev
    return (f'hf/{repo}/{path}', f'https://huggingface.co/{repo}/resolve/{rev}/{path}', sha256)


MODELS = [
    # Whisper base: fp32 encoder + q4 decoder on WebGPU, q8 for both on WASM.
    hf(WHISPER, 'config.json', 'f4d0608f7d918166da7edb3e188de5ef1bfe70d9802e785d271fd88111e9cf4b'),
    hf(WHISPER, 'generation_config.json', '61070cf8de25b1e9256e8e102ded49d8d24a8369ed36ef84fdf21549e68125a0'),
    hf(WHISPER, 'preprocessor_config.json', 'a6a76d28c93edb273669eb9e0b0636a2bddbb1272c3261e47b7ca6dfdbac1b8d'),
    hf(WHISPER, 'tokenizer.json', '27fc476bfe7f17299480be2273fc0608e4d5a99aba2ab5dec5374b4482d1a566'),
    hf(WHISPER, 'tokenizer_config.json', '2e036e4dbacfdeb7242c7d4ec4149f4a16e86026048f94d1637e3a8ee9c6a573'),
    hf(WHISPER, 'onnx/encoder_model.onnx', 'a9f3b752833b49e880dec91ee5b6d936112be7c3ea07c221024ba493439f46fe'),
    hf(WHISPER, 'onnx/decoder_model_merged_q4.onnx', '09f83b71ceedc97dab1d90b914715dc532a646a147abda11d83c64867b7c319c'),
    hf(WHISPER, 'onnx/encoder_model_quantized.onnx', '5862993336bf33acd23736071aae2b32261d3b1b2f37780194460d4ef974dd46'),
    hf(WHISPER, 'onnx/decoder_model_merged_quantized.onnx', 'fa3ef9902734ce5ae6f9ef2bdb2ba9a6c4b5785b09f4f420ce036573dc9d090b'),
    # Kokoro 82M v1.0: q8 on WASM (WebGPU has a vocoder ConvTranspose distortion bug).
    # kokoro-js 1.2.1 speaks English only, so only the host's English voices;
    # Portuguese and Spanish stay on the host worker.
    hf(KOKORO, 'config.json', 'df34b4f930b23447cd4dc410fabfb42eb3f24e803e6c3f97d618fb359380a36f'),
    hf(KOKORO, 'tokenizer.json', '77a02c8e164413299b4b4c403b14f8e0e1c1b727db4d46a09d6327b861060a34'),
    hf(KOKORO, 'tokenizer_config.json', 'be1cb066d6ef6b074b3f15e6a6dd21ac88ff3cdaedf325f0aaed686c70f75d20'),
    hf(KOKORO, 'onnx/model.onnx', '8fbea51ea711f2af382e88c833d9e288c6dc82ce5e98421ea61c058ce21a34cb'),
    hf(KOKORO, 'onnx/model_quantized.onnx', 'fbae9257e1e05ffc727e951ef9b9c98418e6d79f1c9b6b13bd59f5c9028a1478'),
    hf(KOKORO, 'voices/af_heart.bin', 'd583ccff3cdca2f7fae535cb998ac07e9fcb90f09737b9a41fa2734ec44a8f0b'),
    hf(KOKORO, 'voices/am_michael.bin', '1d1f21dd8da39c30705cd4c75d039d265e9bc4a2a93ed09bc9e1b1225eb95ba1'),
    # Silero VAD v5.
    hf(SILERO, 'onnx/model.onnx', 'a4a068cd6cf1ea8355b84327595838ca748ec29a25bc91fc82e6c299ccdc5808'),
]

FILES = LIB + MODELS


def default_dest() -> Path:
    if os.environ.get('SAIL_VOICE_WEB_DIR'):
        return Path(os.environ['SAIL_VOICE_WEB_DIR'])
    return Path(os.environ.get('SAIL_RUNTIME') or '/srv/sail') / 'models' / 'voice' / 'web'


def sha256_of(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open('rb') as f:
        for block in iter(lambda: f.read(1 << 20), b''):
            digest.update(block)
    return digest.hexdigest()


def fetch(dest: Path, url: str, expected: str) -> None:
    if dest.is_file() and sha256_of(dest) == expected:
        return
    dest.parent.mkdir(parents=True, exist_ok=True)
    tmp = dest.with_name(dest.name + '.download')
    print(f'downloading {url}', flush=True)
    with urllib.request.urlopen(url, timeout=60) as response, tmp.open('wb') as out:
        while block := response.read(1 << 20):
            out.write(block)
    actual = sha256_of(tmp)
    if actual != expected:
        tmp.unlink()
        raise SystemExit(f'sha256 mismatch for {url}: expected {expected}, got {actual}')
    tmp.replace(dest)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split('\n\n')[0])
    parser.add_argument('--dest', type=Path, default=default_dest())
    args = parser.parse_args()
    root = args.dest
    for rel, url, sha in FILES:
        fetch(root / rel, url, sha)
    # Written last: the server and browser treat the chain as installed only when the
    # manifest exists, so a partial download is never offered to clients.
    manifest = {
        'version': 1,
        # Sizes let the browser show real loading progress against the full download.
        'files': {rel: {'source': url, 'sha256': sha, 'bytes': (root / rel).stat().st_size} for rel, url, sha in FILES},
        'models': {'stt': WHISPER[0], 'tts': KOKORO[0], 'vad': SILERO[0]},
    }
    tmp = root / 'manifest.json.download'
    tmp.write_text(json.dumps(manifest, indent=2) + '\n')
    tmp.replace(root / 'manifest.json')
    print('Browser speech chain ready:', root.resolve())
    return 0


if __name__ == '__main__':
    sys.exit(main())
