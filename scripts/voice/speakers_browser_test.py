"""Offline speaker UI test; model responses are explicit fixtures, not real accuracy evidence."""
from http.server import ThreadingHTTPServer
from pathlib import Path
import io
import json
import threading
import wave
from playwright.sync_api import sync_playwright,expect
from subsystem_pages_test import UIHandler
profiles=[];calls=[]
class Handler(UIHandler):
    def reply(self,value,status=200):
        body=json.dumps(value).encode();self.send_response(status);self.send_header('Content-Type','application/json');self.send_header('Content-Length',str(len(body)));self.end_headers();self.wfile.write(body)
    def do_GET(self):
        if self.path=='/api/guardian/token':self.reply({'token':'speaker-browser-fixture-owner-token-at-least-32'});return
        super().do_GET()
    def do_POST(self):
        data=json.loads(self.rfile.read(int(self.headers['Content-Length'])));calls.append(data['action'])
        if data['action']=='list':self.reply({'profiles':profiles})
        elif data['action']=='enroll':profiles.append({'id':data['request_id'],'name':data['name']});self.reply({'enrolled':True,'name':data['name']})
        elif data['action']=='delete':profiles.clear();self.reply({'deleted':True})
        else:
            known=data['action']=='verify'
            self.reply({'version':1,'advisory_only':True,'segments':[{'speaker':'SPEAKER_00','start_ms':0,'end_ms':4000}],'matches':[{'speaker':'SPEAKER_00','name':profiles[0]['name'] if known else None,'similarity':0.91 if known else None,'decision':'matched' if known else 'unknown'}]})

server=ThreadingHTTPServer(('127.0.0.1',0),Handler);threading.Thread(target=server.serve_forever,daemon=True).start()
try:
    audio=io.BytesIO()
    with wave.open(audio,'wb') as f:f.setnchannels(1);f.setsampwidth(2);f.setframerate(16000);f.writeframes(bytes(128000))
    with sync_playwright() as p:
        browser=p.chromium.launch(headless=True);page=browser.new_page(viewport={'width':1440,'height':1000},reduced_motion='reduce');errors=[];page.on('pageerror',lambda e:errors.append(str(e)));page.on('dialog',lambda dialog:dialog.accept())
        page.goto(f'http://127.0.0.1:{server.server_port}/');page.locator('#settingsOpen').click();page.locator('[data-settings-tab="voice"]').click();page.locator('#speakerControls summary').click()
        page.locator('#speakerEnroll').click();expect(page.locator('#speakerStatus')).to_contain_text('explicit enrollment consent');assert 'enroll' not in calls
        page.locator('#speakerAudio').set_input_files({'name':'clip.wav','mimeType':'audio/wav','buffer':audio.getvalue()});page.locator('#speakerAnalyze').click();expect(page.locator('#speakerResults')).to_contain_text('SPEAKER_00 · Unknown')
        page.locator('#speakerName').fill('Alice <img src=x>');page.locator('#speakerConsent').check();page.locator('#speakerEnroll').click();expect(page.locator('#speakerStatus')).to_contain_text('Enrolled Alice');assert page.locator('#speakerProfiles img').count()==0
        page.get_by_role('button',name='Verify selected clip').click();expect(page.locator('#speakerResults')).to_contain_text('Alice <img src=x> · matched')
        page.get_by_role('button',name='Delete voice profile').click();expect(page.locator('#speakerStatus')).to_contain_text('Voice profile deleted');assert not profiles
        page.locator('#speakerAudio').set_input_files({'name':'broken.wav','mimeType':'audio/wav','buffer':b'not a wave'});before=calls.count('analyze');page.locator('#speakerAnalyze').click();expect(page.locator('#speakerStatus')).to_contain_text('complete PCM WAV');assert calls.count('analyze')==before
        page.set_viewport_size({'width':390,'height':844});assert page.evaluate('document.documentElement.scrollWidth<=innerWidth');Path('.local').mkdir(exist_ok=True);page.locator('#speakerControls').scroll_into_view_if_needed(); assert page.locator('#speakerControls').bounding_box()['width']>280; page.screenshot(path='.local/speaker-settings-mobile.png')
        assert not errors,errors;browser.close()
finally:server.shutdown();server.server_close()
print('Speaker consent, WAV validation, timeline, enrollment/verification/delete controls, safe text and mobile layout passed (fixture backend)')
