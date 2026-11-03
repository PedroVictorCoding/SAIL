"""Optional Mind-owned local diarization worker. No profile storage or cloud API."""
import os
os.environ.update(HF_HUB_OFFLINE='1', TRANSFORMERS_OFFLINE='1', HF_HUB_DISABLE_TELEMETRY='1', PYANNOTE_METRICS_ENABLED='0', OMP_NUM_THREADS='2', MKL_NUM_THREADS='2', OPENBLAS_NUM_THREADS='2')
import base64
import contextlib
import hashlib
import json
from pathlib import Path
import sys

PROTOCOL=sys.stdout
MAX_BYTES=16000*60*2

def revision(root):
    import importlib.metadata
    digest=hashlib.sha256(('SAIL-speaker-v1|pyannote.audio/'+importlib.metadata.version('pyannote.audio')).encode())
    paths=sorted(p for p in root.rglob('*') if p.is_file() and p.suffix in {'.yaml','.yml','.bin','.safetensors','.ckpt','.pt','.npz'})
    if not paths: raise ValueError('No local model assets found')
    for path in paths:
        digest.update(str(path.relative_to(root)).encode())
        with path.open('rb') as stream:
            while chunk:=stream.read(1024*1024): digest.update(chunk)
    return digest.hexdigest()

def analyze(pipeline, pcm, model_revision):
    import numpy as np
    import torch
    waveform=torch.from_numpy(np.frombuffer(pcm,dtype='<i2').astype(np.float32)/32768).unsqueeze(0)
    with torch.inference_mode(): output=pipeline({'waveform':waveform,'sample_rate':16000},max_speakers=16)
    labels=output.speaker_diarization.labels()
    if not labels: return {'version':1,'model_revision':model_revision,'segments':[],'embeddings':[]}
    vectors=output.speaker_embeddings
    if vectors is None or len(vectors)!=len(labels): raise ValueError('Local pipeline did not return speaker embeddings')
    duration=len(pcm)//32
    segments=[{'speaker':speaker,'start_ms':max(0,round(turn.start*1000)),'end_ms':min(duration,round(turn.end*1000))} for turn,_,speaker in output.speaker_diarization.itertracks(yield_label=True)]
    segments=[s for s in segments if s['start_ms']<s['end_ms']]
    return {'version':1,'model_revision':model_revision,'segments':segments,'embeddings':[{'speaker':label,'values':vectors[i].tolist()} for i,label in enumerate(labels)]}

def main():
    line=sys.stdin.readline(3_000_001)
    if not line.endswith('\n') or len(line)>3_000_000: raise ValueError('Speaker request too large')
    request=json.loads(line)
    pcm=base64.b64decode(request['pcm'],validate=True)
    if not pcm or len(pcm)>MAX_BYTES or len(pcm)%2: raise ValueError('Expected at most 60 seconds of PCM16 mono at 16 kHz')
    root=Path(os.environ.get('SAIL_SPEAKER_MODEL_DIR','')).expanduser()
    if not os.environ.get('SAIL_SPEAKER_MODEL_DIR') or not root.is_dir(): raise ValueError('Set SAIL_SPEAKER_MODEL_DIR to an installed offline Community-1 pipeline')
    import torch
    torch.set_num_threads(2)
    torch.set_num_interop_threads(1)
    from pyannote.audio import Pipeline
    from pyannote.audio.pipelines import SpeakerDiarization
    pipeline=Pipeline.from_pretrained(str(root.resolve()))
    if not isinstance(pipeline,SpeakerDiarization): raise ValueError('Only the local SpeakerDiarization pipeline is supported')
    pipeline.to(torch.device('cpu'))
    return analyze(pipeline,pcm,revision(root))

if __name__=='__main__':
    try:
        with contextlib.redirect_stdout(sys.stderr): result=main()
        PROTOCOL.write(json.dumps({'ok':True,'result':result},allow_nan=False)+'\n');PROTOCOL.flush()
    except Exception:
        # Do not echo audio, vectors, tokens, or model exception internals into logs.
        PROTOCOL.write(json.dumps({'ok':False,'error':'Local speaker model unavailable or invalid input; check the offline model installation.'})+'\n');PROTOCOL.flush()
