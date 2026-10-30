"""Exercise a running SAIL voice server with real models. Usage: python e2e.py [port]."""
import asyncio
import base64
import json
import struct
import sys
import wave
import zlib
import websockets

PORT=int(sys.argv[1]) if len(sys.argv)>1 else 8088
async def receive(ws, kind, timeout=180):
    async with asyncio.timeout(timeout):
        while True:
            value=json.loads(await ws.recv())
            if value['type']=='error': raise AssertionError(value)
            if value['type']==kind: return value
async def collect_turn(ws):
    kinds={}; reply=''
    async with asyncio.timeout(180):
        while True:
            value=json.loads(await ws.recv()); kind=value['type']; kinds[kind]=kinds.get(kind,0)+1
            if kind=='error': raise AssertionError(value)
            if kind=='response_delta':reply+=value['text']
            if kind=='audio_chunk':assert base64.b64decode(value['audio_base64'])[:4]==b'RIFF'
            if kind=='turn_finished':return reply,kinds
async def send(ws, action, **fields):await ws.send(json.dumps({'action':action,**fields}))
def red_png():
    def chunk(kind,data):return struct.pack('>I',len(data))+kind+data+struct.pack('>I',zlib.crc32(kind+data)&0xffffffff)
    width=height=32
    return b'\x89PNG\r\n\x1a\n'+chunk(b'IHDR',struct.pack('>IIBBBBB',width,height,8,2,0,0,0))+chunk(b'IDAT',zlib.compress((b'\0'+b'\xff\0\0'*width)*height))+chunk(b'IEND',b'')
async def main():
    uri=f'ws://127.0.0.1:{PORT}/ws/voice'
    async with websockets.connect(uri,origin=f'http://127.0.0.1:{PORT}',max_size=8_000_000) as a,websockets.connect(uri,origin=f'http://127.0.0.1:{PORT}',max_size=8_000_000) as b:
        hello_a=await receive(a,'session'); hello_b=await receive(b,'session');assert hello_a['session_id']!=hello_b['session_id']
        await send(a,'toggle_mute'); assert (await receive(a,'control'))['control']['is_muted']
        await send(b,'set_tier',profile='LowPower');assert not (await receive(b,'control'))['control']['is_muted']
        await send(a,'toggle_mute');await receive(a,'control')
        await send(a,'kernel_tool',tool='fs:query',params={'path':'/etc/passwd'})
        assert not (await receive(a,'tool_result'))['result']['success']
        await send(a,'text',text='Reply with one short sentence saying the blue bicycle is ready.')
        reply,kinds=await collect_turn(a);assert 'bicycle' in reply.lower(),reply;assert kinds.get('audio_chunk',0)>0 and kinds.get('canvas',0)>0
        print('Text -> Gemma -> Kokoro -> Canvas:',reply, kinds,flush=True)
        # Input is a real synthesized fixture from smoke.py, resampled to 16kHz.
        import numpy as np
        with wave.open('.local/voice-smoke.wav','rb') as f:audio=np.frombuffer(f.readframes(f.getnframes()),dtype='<i2')
        pcm=np.interp(np.arange(0,len(audio),1.5),np.arange(len(audio)),audio).astype('<i2').tobytes()+bytes(16000*2)
        collector=asyncio.create_task(collect_turn(a))
        for offset in range(0,len(pcm),1024):
            frame=pcm[offset:offset+1024]
            if len(frame)<1024:frame+=bytes(1024-len(frame))
            await a.send(frame);await asyncio.sleep(.032)
        reply,kinds=await collector;assert kinds.get('transcript',0)>0 and kinds.get('audio_chunk',0)>0
        print('PCM -> Silero -> Whisper -> Session -> Gemma -> Kokoro:',reply,kinds,flush=True)
        await send(a,'text',text='Describe only the dominant color of this image in one short sentence.',vision={'format':'image/png','data_base64':base64.b64encode(red_png()).decode()})
        reply,kinds=await collect_turn(a);assert 'red' in reply.lower(),reply
        print('Vision:',reply,flush=True)
        await send(a,'text',text='Write a very long detailed story about a bicycle.')
        await receive(a,'response_delta');await send(a,'interrupt');await receive(a,'interruption')
        await send(a,'set_tier',profile='Balanced');await receive(a,'control',timeout=3)
        # No output from the cancelled turn may follow its interruption acknowledgement.
        try:
            async with asyncio.timeout(.4):
                while True:
                    event=json.loads(await a.recv());assert event['type'] not in ('response_delta','audio_chunk'),event
        except TimeoutError:pass
        print('Session isolation, tool scope, responsive controls and cancellation: passed',flush=True)
asyncio.run(main())
