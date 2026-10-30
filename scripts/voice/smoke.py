"""Real offline inference smoke test: Kokoro -> Silero -> Whisper. No fake models."""
import base64
import json
import subprocess
import sys
import time
from pathlib import Path
import numpy as np

def start(mode):
    return subprocess.Popen([sys.executable, 'scripts/voice/worker.py', mode], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
def request(worker, data):
    worker.stdin.write(json.dumps(data) + '\n'); worker.stdin.flush()
    line = worker.stdout.readline()
    assert line, 'Worker exited'
    value = json.loads(line)
    assert value['ok'], value
    return value['result']
def encode(pcm):
    return base64.b64encode(pcm.astype('<i2').tobytes()).decode()
workers = []
try:
    tts = start('tts'); workers.append(tts)
    speech = request(tts, {'text': 'The blue bicycle is parked beside the library.', 'voice': 'af_heart'})
    audio = np.frombuffer(base64.b64decode(speech['pcm']), dtype='<i2')
    assert len(audio) > 24000 and speech['sample_rate'] == 24000
    # Keep a human-reviewable fixture out of source control.
    import wave
    Path('.local').mkdir(exist_ok=True)
    with wave.open('.local/voice-smoke.wav', 'wb') as f:
        f.setnchannels(1); f.setsampwidth(2); f.setframerate(24000); f.writeframes(audio.tobytes())
    pcm = np.interp(np.arange(0, len(audio), 1.5), np.arange(len(audio)), audio).astype('<i2')
    vad = start('vad'); workers.append(vad)
    confidence, latency = [], []
    for offset in range(0, len(pcm) - 512, 512):
        result = request(vad, {'session_id': 'smoke', 'pcm': encode(pcm[offset:offset+512])})
        confidence.append(result['confidence']); latency.append(result['inference_us'])
    assert max(confidence) > .7, max(confidence)
    # Independent recurrent state and explicit reset.
    silent = request(vad, {'session_id': 'other', 'pcm': encode(np.zeros(512))})
    assert silent['confidence'] < .5
    request(vad, {'op': 'reset', 'session_id': 'smoke'})
    stt = start('stt'); workers.append(stt)
    result = request(stt, {'pcm': encode(pcm)})
    assert 'bicycle' in result['text'].lower() and 'library' in result['text'].lower(), result
    partial = request(stt, {'pcm': encode(pcm[:len(pcm)//2]), 'partial': True})
    assert partial['tokens'] and all(t['is_partial'] for t in partial['tokens'])
    print(json.dumps({'transcript': result['text'], 'partial': partial['text'], 'vad_max_confidence': max(confidence), 'vad_p50_us': float(np.median(latency)), 'vad_p95_us': float(np.percentile(latency,95)), 'audio_seconds': len(audio)/24000}, indent=2))
finally:
    for worker in workers:
        worker.terminate()
        try: worker.wait(timeout=5)
        except subprocess.TimeoutExpired: worker.kill(); worker.wait()
