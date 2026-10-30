"""Mind-owned offline workers. One JSON request/response per line; stdout is protocol only."""
import os
os.environ.update(HF_HUB_OFFLINE='1', TRANSFORMERS_OFFLINE='1', HF_HUB_DISABLE_TELEMETRY='1')
import base64
import contextlib
import json
import sys
import time
from pathlib import Path

PROTOCOL = sys.stdout
ROOT = Path(os.environ.get('SAIL_VOICE_MODELS', 'models/voice')).resolve()

def respond(value):
    PROTOCOL.write(json.dumps(value) + '\n')
    PROTOCOL.flush()

def main():
    import numpy as np
    mode = sys.argv[1]
    model = None
    threads = 4
    states = {}
    if mode == 'vad':
        import onnxruntime as ort
        opts = ort.SessionOptions()
        opts.intra_op_num_threads = opts.inter_op_num_threads = 1
        model = ort.InferenceSession(str(ROOT / 'silero_vad.onnx'), opts, providers=['CPUExecutionProvider'])
    elif mode == 'stt':
        from faster_whisper import WhisperModel
        model = WhisperModel(str(ROOT / 'whisper-base'), device='cpu', compute_type='int8', cpu_threads=4, local_files_only=True)
    elif mode == 'tts':
        from kokoro_onnx import Kokoro
        import onnxruntime as ort
        def make_tts(threads):
            opts = ort.SessionOptions()
            opts.intra_op_num_threads = threads
            opts.inter_op_num_threads = 1
            session = ort.InferenceSession(str(ROOT / 'kokoro-v1.0.onnx'), opts, providers=['CPUExecutionProvider'])
            return Kokoro.from_session(session, str(ROOT / 'voices-v1.0.bin'))
        model = make_tts(threads)
    else:
        raise ValueError('Unknown worker')
    for line in sys.stdin:
        try:
            req = json.loads(line)
            requested_threads = int(req.get('threads', threads))
            if requested_threads not in (2, 4, 8):
                raise ValueError('Invalid hardware tier thread count')
            if requested_threads != threads and mode != 'vad':
                if mode == 'stt':
                    model = WhisperModel(str(ROOT / 'whisper-base'), device='cpu', compute_type='int8', cpu_threads=requested_threads, local_files_only=True)
                else:
                    model = make_tts(requested_threads)
                threads = requested_threads
            if req.get('op') == 'health':
                result = {'ready': True, 'engine': mode}
            elif mode == 'vad':
                session = req['session_id']
                if req.get('op') == 'reset':
                    states.pop(session, None)
                    result = {}
                else:
                    pcm = np.frombuffer(base64.b64decode(req['pcm']), dtype='<i2').astype(np.float32) / 32768
                    if pcm.size != 512:
                        raise ValueError('Silero requires exactly 512 samples at 16000 Hz')
                    state, context = states.get(session, (np.zeros((2, 1, 128), np.float32), np.zeros((1, 64), np.float32)))
                    x = np.concatenate((context, pcm[None, :]), axis=1)
                    start = time.perf_counter_ns()
                    out, state = model.run(None, {'input': x, 'state': state, 'sr': np.array(16000, dtype=np.int64)})
                    elapsed = (time.perf_counter_ns() - start) // 1000
                    states[session] = (state, x[:, -64:])
                    result = {'confidence': float(out[0][0]), 'inference_us': elapsed}
            elif mode == 'stt':
                pcm = np.frombuffer(base64.b64decode(req['pcm']), dtype='<i2').astype(np.float32) / 32768
                segments, info = model.transcribe(pcm, language=req.get('language'), beam_size=1 if req.get('partial') else 5, vad_filter=False, condition_on_previous_text=False, word_timestamps=True)
                tokens = []
                text = ''
                for seg in segments:
                    text += seg.text
                    for word in seg.words or []:
                        tokens.append({'token': word.word, 'confidence': word.probability, 'timestamp_ms': int(word.start * 1000), 'is_partial': bool(req.get('partial'))})
                result = {'text': text.strip(), 'tokens': tokens, 'language': info.language}
            else:
                samples, sr = model.create(req['text'], voice=req.get('voice', 'af_heart'), speed=float(req.get('speed', 1)), lang=req.get('language', 'en-us'))
                pcm = (np.clip(samples, -1, 1) * 32767).astype('<i2')
                result = {'pcm': base64.b64encode(pcm.tobytes()).decode(), 'sample_rate': sr}
            respond({'ok': True, 'result': result})
        except Exception as exc:
            respond({'ok': False, 'error': str(exc)})

if __name__ == '__main__':
    # Inference libraries may print diagnostics; stdout is exclusively NDJSON.
    with contextlib.redirect_stdout(sys.stderr):
        main()
