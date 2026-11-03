"""Protocol failures must not echo audio/secrets or require optional model imports."""
import base64
import json
import os
from pathlib import Path
import subprocess
import sys
import unittest
WORKER=Path(__file__).with_name('speaker_worker.py')
class WorkerTests(unittest.TestCase):
    def test_absent_model_and_malformed_audio_fail_without_echoing_input(self):
        env={**os.environ,'SAIL_SPEAKER_MODEL_DIR':'/not/an/installed/model'}
        for pcm in ['private-invalid-audio!',base64.b64encode(b'\x00'*32000).decode()]:
            result=subprocess.run([sys.executable,str(WORKER)],input=json.dumps({'pcm':pcm})+'\n',text=True,capture_output=True,env=env,timeout=10,check=True)
            reply=json.loads(result.stdout)
            self.assertFalse(reply['ok']);self.assertNotIn(pcm,reply['error']);self.assertEqual(result.stderr,'')
if __name__=='__main__': unittest.main()
