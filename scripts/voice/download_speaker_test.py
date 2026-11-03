"""Downloader tests use fixture assets; no network, credentials or model weights."""
import hashlib
import json
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch
import download_speaker as download

REVISION='a'*40
FILES={'config.yaml':b'pipeline: local\n','embedding/model.bin':b'actual fixture weights'}
class API:
    def __init__(self,denied=False): self.denied=denied;self.access_checks=0
    def auth_check(self,**kwargs):
        self.access_checks+=1
        if self.denied: raise PermissionError('private server details must not be printed')
    def model_info(self,*args,**kwargs):
        return SimpleNamespace(sha=REVISION,siblings=[SimpleNamespace(rfilename=name,size=len(body),lfs={'sha256':hashlib.sha256(body).hexdigest()}) for name,body in FILES.items()])

def snapshot(**kwargs):
    assert kwargs['revision']==REVISION
    assert kwargs['max_workers']==2
    root=Path(kwargs['local_dir'])
    for name,body in FILES.items():
        path=root/name;path.parent.mkdir(parents=True,exist_ok=True);path.write_bytes(body)

class DownloadTests(unittest.TestCase):
    def setUp(self):self.tmp=tempfile.TemporaryDirectory();self.addCleanup(self.tmp.cleanup);self.dest=Path(self.tmp.name)/'model'
    def test_success_is_pinned_verified_and_repeated_runs_need_no_network(self):
        result=download.download(self.dest,API(),snapshot,'not-written-to-disk')
        self.assertEqual(result['revision'],REVISION)
        self.assertEqual(download.verify(self.dest)['repo'],download.REPO)
        self.assertNotIn('not-written-to-disk',json.dumps(result))
        download.download(self.dest,API(denied=True),lambda **_:self.fail('must not redownload'),None)
        (self.dest/'config.yaml').write_bytes(b'changed')
        with self.assertRaises(download.SetupError):download.verify(self.dest)
    def test_denied_access_creates_no_directory(self):
        with self.assertRaises(PermissionError):download.download(self.dest,API(denied=True),snapshot,None)
        self.assertFalse(self.dest.exists())
        self.assertEqual(list(self.dest.parent.iterdir()),[])
    def test_interrupted_snapshot_resumes_without_publishing_partial_model(self):
        def interrupted(**kwargs):
            (Path(kwargs['local_dir'])/'config.yaml').write_bytes(FILES['config.yaml'])
            raise KeyboardInterrupt()
        with self.assertRaises(KeyboardInterrupt):download.download(self.dest,API(),interrupted,None)
        self.assertFalse(self.dest.exists())
        self.assertTrue(list(self.dest.parent.glob('.model.download-*')))
        download.download(self.dest,API(),snapshot,None)
        self.assertTrue((self.dest/download.MANIFEST).is_file())
    def test_existing_user_files_and_symlinks_are_not_overwritten(self):
        self.dest.mkdir();(self.dest/'mine').write_text('keep')
        with self.assertRaises(download.SetupError):download.download(self.dest,API(),snapshot,None)
        self.assertEqual((self.dest/'mine').read_text(),'keep')
        with self.assertRaises(download.SetupError):download.file_path(self.dest,'../escape')
        (self.dest/'link').symlink_to(self.dest.parent)
        with self.assertRaises(download.SetupError):download.file_path(self.dest,'link/escape')
    def test_space_and_lfs_integrity_fail_closed(self):
        with patch.object(download.shutil,'disk_usage',return_value=SimpleNamespace(free=0)):
            with self.assertRaises(download.SetupError):download.download(self.dest,API(),snapshot,None)
        def corrupt(**kwargs):snapshot(**kwargs);(Path(kwargs['local_dir'])/'embedding/model.bin').write_bytes(b'x'*len(FILES['embedding/model.bin']))
        with self.assertRaises(download.SetupError):download.download(self.dest,API(),corrupt,None)
        self.assertFalse(self.dest.exists())
    def test_offline_cli_verification_works_and_revision_mismatch_refuses(self):
        download.download(self.dest,API(),snapshot,None)
        self.assertEqual(download.main(['--destination',str(self.dest),'--verify']),0)
        with self.assertRaises(download.SetupError):download.verify(self.dest,'b'*40)
if __name__=='__main__':unittest.main()
