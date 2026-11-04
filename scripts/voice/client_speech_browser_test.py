"""Browser test of on-device speech against a running SAIL server (requires Playwright and
`scripts/voice/download_web.py` on the server).

    .venv-voice/bin/python scripts/voice/client_speech_browser_test.py [port] [--full-turn]

Pass 1, default settings: the browser measures its own speed. If it is too slow it must
leave speech on the host and say so; otherwise it takes speech over.

Pass 2, speed limit lifted (localStorage override), so a slow test machine still exercises
the on-device path:
- the models load from /voice-assets only (any other network request fails the test);
- Kokoro speaks English and Whisper transcribes it back; Whisper identifies Portuguese;
- the microphone (Chromium's fake device playing a speech WAV) goes through the browser
  VAD and Whisper, and the server receives text, never PCM;
- with --full-turn, the reply comes back as `speech_text` and is spoken by the browser, with
  no `audio_chunk` from the host. This needs the server's model to answer.

Fixtures: `.local/voice-smoke.wav` from scripts/voice/smoke.py, and
`.local/voice-smoke-pt.wav` (host Kokoro, pf_dora: "O gato está a dormir no sofá da sala.").
"""
import base64
import sys
import wave
from pathlib import Path

import numpy as np
from playwright.sync_api import sync_playwright

args = [a for a in sys.argv[1:] if not a.startswith('--')]
PORT = int(args[0]) if args else 8088
FULL_TURN = '--full-turn' in sys.argv
ORIGIN = f'http://127.0.0.1:{PORT}'
FIXTURE = Path('.local/client-speech-mic.wav')

RECORD_SOCKET = """
  window.__sent = []; window.__received = [];
  const RealWS = WebSocket;
  window.WebSocket = class extends RealWS {
    constructor(...a) { super(...a); this.addEventListener('message', e => { try { window.__received.push(JSON.parse(e.data).type); } catch {} }); }
    send(d) { window.__sent.push(typeof d === 'string' ? JSON.parse(d).action : 'binary'); return super.send(d); }
  };"""
ROUND_TRIP = """async ({ text, voice }) => {
  const speech = await SailClientSpeech.speak(text, voice);
  const pcm = SailClientSpeech.resample(speech.audio, speech.sampling_rate);
  return SailClientSpeech.transcribe(pcm);
}"""
TRANSCRIBE_WAV = """async ({ b64, rate }) => {
  const bytes = Uint8Array.from(atob(b64), c => c.charCodeAt(0));
  const pcm16 = new Int16Array(bytes.buffer, 44, (bytes.length - 44) >> 1);
  const audio = Float32Array.from(pcm16, v => v / 32768);
  return SailClientSpeech.transcribe(SailClientSpeech.resample(audio, rate));
}"""


def read_wav(path):
    with wave.open(path, 'rb') as f:
        return f.getframerate(), np.frombuffer(f.readframes(f.getnframes()), dtype='<i2')


def mic_fixture():
    """`.local/voice-smoke.wav` (host Kokoro: "The blue bicycle is parked beside the
    library.") at 16 kHz, with silence around it so the VAD sees a start and an end."""
    rate, audio = read_wav('.local/voice-smoke.wav')
    pcm = np.interp(np.arange(0, len(audio), rate / 16000), np.arange(len(audio)), audio.astype(np.float32))
    silence = np.zeros(16000 * 2, np.float32)
    pcm = np.concatenate([silence, pcm, silence, silence]).astype('<i2')
    with wave.open(str(FIXTURE), 'wb') as f:
        f.setnchannels(1); f.setsampwidth(2); f.setframerate(16000); f.writeframes(pcm.tobytes())


def open_page(browser, max_rtf=None):
    page = browser.new_context().new_page()
    page.errors, page.offsite = [], []
    page.on('pageerror', lambda e: page.errors.append(str(e)))
    page.on('request', lambda r: None if r.url.startswith((ORIGIN, 'ws://127.0.0.1', 'data:', 'blob:')) else page.offsite.append(r.url))
    page.add_init_script(RECORD_SOCKET)
    if max_rtf:
        page.add_init_script(f"localStorage.setItem('sail.speechMaxRealTimeFactor', '{max_rtf}')")
    page.goto(ORIGIN)
    page.wait_for_function('ws && ws.readyState === WebSocket.OPEN')
    page.wait_for_function('SailClientSpeech.engine.settled', timeout=600_000)
    page.engine = page.evaluate('(({status, device, detail, stt, tts, measured}) => ({status, device, detail, stt, tts, measured}))(SailClientSpeech.engine)')
    page.wait_for_function('clientSpeechActive.stt === SailClientSpeech.sttOnDevice() && clientSpeechActive.tts === SailClientSpeech.ttsOnDevice(selectedVoice())', timeout=10_000)
    return page


def check_clean(page):
    assert not page.offsite, f'off-device requests: {page.offsite}'
    assert not page.errors, page.errors


mic_fixture()
with sync_playwright() as p:
    browser = p.chromium.launch(headless=True, args=[
        '--no-sandbox', '--use-fake-ui-for-media-stream', '--use-fake-device-for-media-stream',
        f'--use-file-for-fake-audio-capture={FIXTURE.resolve()}%noloop',
        '--autoplay-policy=no-user-gesture-required',
    ])

    # Pass 1: default speed gate.
    page = open_page(browser)
    status = page.locator('#speechPlacementStatus').text_content()
    print('Default:', page.engine, '|', status, flush=True)
    assert page.evaluate('crossOriginIsolated'), 'page is not cross-origin isolated (no WebAssembly threads)'
    if page.engine['status'] != 'ready':
        assert 'too slow' in page.engine['detail'] and 'host' in status, (page.engine, status)
        assert page.evaluate('!clientSpeechActive.stt && !clientSpeechActive.tts')
    check_clean(page)
    page.close()

    # Pass 2: on-device path, whatever this machine's speed.
    page = open_page(browser, max_rtf=100)
    engine = page.engine
    print('Forced:', engine, flush=True)
    assert engine['status'] == 'ready' and engine['stt'] and engine['tts'], engine

    english = page.evaluate(ROUND_TRIP, {'text': 'The quick brown fox jumps over the lazy dog.', 'voice': 'af_heart'})
    print('Kokoro -> Whisper (en):', english, flush=True)
    assert english['language'] == 'en', english
    assert all(w in english['text'].lower() for w in ('quick', 'brown', 'fox', 'lazy', 'dog')), english

    rate, _ = read_wav('.local/voice-smoke-pt.wav')
    portuguese = page.evaluate(TRANSCRIBE_WAV, {'b64': base64.b64encode(Path('.local/voice-smoke-pt.wav').read_bytes()).decode(), 'rate': rate})
    print('Whisper (pt):', portuguese, flush=True)
    assert portuguese['language'] == 'pt', portuguese
    assert 'gato' in portuguese['text'].lower(), portuguese

    page.get_by_role('button', name='Start Microphone').click()
    page.wait_for_function('window.__sent.includes("speech_start")', timeout=30_000)
    page.wait_for_function('window.__sent.includes("voice_text")', timeout=180_000)
    page.wait_for_function('window.__received.includes("turn_committed")', timeout=10_000)
    transcript = page.locator('#transcript').text_content()
    print('Server transcript:', transcript, flush=True)
    assert 'bicycle' in transcript.lower() and 'library' in transcript.lower(), transcript
    assert 'binary' not in page.evaluate('window.__sent'), 'PCM left the device while on-device speech was active'

    if FULL_TURN:
        page.wait_for_function('window.__received.includes("turn_finished")', timeout=180_000)
        received = page.evaluate('window.__received')
        assert 'speech_text' in received, received
        assert 'audio_chunk' not in received, 'the host synthesized speech the browser should have'
        page.wait_for_function('!playingSources.size && pendingAudio === 0', timeout=120_000)
        print('Reply spoken on this device:', received.count('speech_text'), 'fragments', flush=True)

    page.get_by_role('button', name='Stop Microphone').click()
    check_clean(page)
    browser.close()
    print('On-device speech browser test passed' + (' (full turn)' if FULL_TURN else ''))
