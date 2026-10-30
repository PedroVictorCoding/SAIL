"""Explicit setup-time downloads; runtime workers never download weights."""
import hashlib
import json
import os
import urllib.request
from pathlib import Path
from huggingface_hub import snapshot_download
root = Path(os.environ.get('SAIL_VOICE_MODELS', 'models/voice'))
root.mkdir(parents=True, exist_ok=True)
urls = {
    'silero_vad.onnx': 'https://raw.githubusercontent.com/snakers4/silero-vad/5cd7945676eb32225748052e2e6a0580e4686a08/src/silero_vad/data/silero_vad.onnx',
    'kokoro-v1.0.onnx': 'https://github.com/thewh1teagle/kokoro-onnx/releases/download/model-files-v1.0/kokoro-v1.0.onnx',
    'mmproj-gemma-4-12B-it-Q8_0.gguf': 'https://huggingface.co/ggml-org/gemma-4-12B-it-GGUF/resolve/202658970530b31541cd0b645a9c198bed2a615e/mmproj-gemma-4-12B-it-Q8_0.gguf',
    'voices-v1.0.bin': 'https://github.com/thewh1teagle/kokoro-onnx/releases/download/model-files-v1.0/voices-v1.0.bin',
}
for name, url in urls.items():
    dest = root / name
    if not dest.exists():
        tmp = dest.with_suffix('.download')
        urllib.request.urlretrieve(url, tmp)
        tmp.replace(dest)
snapshot_download('Systran/faster-whisper-base', revision='ebe41f70d5b6dfa9166e2c581c45c9c0cfc57b66', local_dir=root / 'whisper-base', allow_patterns=['*.json', '*.bin', '*.txt'])
manifest = {p.name: {'source': urls[p.name], 'sha256': hashlib.sha256(p.read_bytes()).hexdigest()} for p in root.iterdir() if p.name in urls}
(root / 'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
print('Voice models ready:', root.resolve())
