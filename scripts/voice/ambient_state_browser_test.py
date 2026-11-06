"""Ambient state contract in a real browser with a fixture /ws/voice transport.

Checks that <html data-sail-state> and --sail-light-a follow real voice, tool and
approval events in order, that background Forge work does not light the shell, that
messages from a closed socket are ignored, and that consented listen-only capture
shows the recording state and a persistent REC indicator (desktop and 390px).

Run with: .venv-voice/bin/python scripts/voice/ambient_state_browser_test.py [screenshot-idle.png]
"""
import functools, http.server, threading, sys, json
from pathlib import Path
from playwright.sync_api import sync_playwright
ROOT = Path(__file__).resolve().parents[2]; WEB = ROOT/"crates/sail-dialogue/web"; ICONS = ROOT/"assets/source/icons/sail-icons.svg"
class H(http.server.SimpleHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/icons.svg":
            b = ICONS.read_bytes(); self.send_response(200); self.send_header("content-type","image/svg+xml"); self.send_header("content-length",str(len(b))); self.end_headers(); self.wfile.write(b); return
        if self.path.startswith("/vendor/") and "/xterm/" not in self.path:
            self.path = "/vendor/xterm/" + self.path.split("/")[-1]
        if self.path.startswith("/api/"): self.send_response(404); self.end_headers(); return
        super().do_GET()
    def log_message(self,*a): pass
SHOT = sys.argv[1] if len(sys.argv) > 1 else "/tmp/ambient-idle.png"
srv = http.server.ThreadingHTTPServer(("127.0.0.1",0), functools.partial(H, directory=str(WEB)))
threading.Thread(target=srv.serve_forever, daemon=True).start()
STUB = """
window.__sockets=[];
class FakeWS { constructor(u){ this.url=u; this.readyState=0; this.sent=[]; __sockets.push(this); setTimeout(()=>{this.readyState=1; this.onopen&&this.onopen();},10);} send(d){this.sent.push(d);} close(){ this.readyState=3; this.onclose&&this.onclose(); } }
FakeWS.OPEN=1; window.WebSocket=FakeWS;
window.__push=(m)=>{ const s=__sockets.at(-1); s.onmessage({data:JSON.stringify(m)}); };
"""
errors=[]
with sync_playwright() as p:
    b = p.chromium.launch(args=["--no-sandbox","--use-fake-device-for-media-stream","--use-fake-ui-for-media-stream"]); pg = b.new_page(viewport={"width":1440,"height":900})
    pg.on("pageerror", lambda e: errors.append(str(e)))
    pg.add_init_script(STUB)
    pg.goto(f"http://127.0.0.1:{srv.server_port}/")
    st = lambda: pg.evaluate("document.documentElement.dataset.sailState")
    light = lambda: pg.evaluate("getComputedStyle(document.documentElement).getPropertyValue('--sail-light-a').trim()")
    pg.wait_for_function("document.documentElement.dataset.sailState==='idle'")
    pg.wait_for_timeout(700); assert light() in ("#496f9d","rgb(73, 111, 157)"), light()
    pg.evaluate("__push({type:'session',session_id:'s1',learning:true,control:{is_muted:false,max_recording_ms:180000}})")
    seq = []
    pg.evaluate("__push({type:'turn_started',turn_id:1,epoch:1,transcript:'hi',state:'thinking'})"); seq.append(st())
    pg.evaluate("__push({type:'tool_event',epoch:1,event:{tool:'fs_read',state:'executing'}})"); seq.append(st())
    pg.evaluate("__push({type:'tool_event',epoch:1,event:{tool:'fs_read',state:'completed'}})"); seq.append(st())
    pg.evaluate("__push({type:'response_finished',epoch:1})"); seq.append(st())
    pg.evaluate("__push({type:'turn_finished',epoch:1,state:'idle'})"); seq.append(st())
    pg.evaluate("__push({type:'approval_required',epoch:1,session_id:'ap1',tool:'fs:write'})"); seq.append(st())
    assert pg.evaluate("document.documentElement.dataset.sailApproval")=="1"
    pg.evaluate("__push({type:'approval_resolved',session_id:'ap1',approved:true})"); seq.append(st())
    # Background Forge work must not light the shell.
    pg.evaluate("__push({type:'forge_event',job_id:'j1',state:'running'})"); seq.append(st())
    print("sequence", seq)
    assert seq == ['thinking','executing','thinking','speaking','idle','approval_required','idle','idle'], seq
    pg.wait_for_timeout(500)
    assert pg.is_hidden("#recIndicator")
    pg.evaluate("__sockets.at(-1).close()"); assert st()=="disconnected", st()
    pg.wait_for_function("document.documentElement.dataset.sailState==='idle'", timeout=5000)
    # Late message from the closed socket is ignored.
    pg.evaluate("__sockets[0].onmessage({data:JSON.stringify({type:'response_finished'})})"); assert st()=="idle", st()
    pg.wait_for_timeout(600)
    pg.screenshot(path=SHOT)
    # Consented listen-only capture: recording + persistent REC.
    pg.evaluate("__push({type:'session',session_id:'s2',learning:true,control:{is_muted:false,max_recording_ms:180000}})")
    pg.evaluate("__push({type:'live_context_state',enabled:true,segments:0})")
    pg.evaluate("startMic()")
    pg.wait_for_function("document.documentElement.dataset.sailState==='recording'", timeout=8000)
    assert pg.is_visible("#recIndicator"), "REC must be visible"
    pg.evaluate("__push({type:'response_finished'})"); assert st()=="speaking", st()
    assert pg.is_visible("#recIndicator"), "REC stays visible while speaking"
    pg.evaluate("__push({type:'turn_finished',state:'idle'})")
    pg.wait_for_timeout(700)
    pg.screenshot(path=SHOT.replace('idle','recording'))
    pg.evaluate("stopMic()"); assert st()=="idle", st(); assert pg.is_hidden("#recIndicator")
    pg.set_viewport_size({"width":390,"height":844}); pg.evaluate("startMic()")
    pg.wait_for_function("document.documentElement.dataset.sailState==='recording'", timeout=8000)
    pg.wait_for_timeout(600); pg.screenshot(path=SHOT.replace('idle','recording-390'))
    b.close()
assert not errors, errors
print("Ambient state contract: ordered transitions, background isolation, stale socket, recording/REC at 1440 and 390px passed (fixture transport)")
