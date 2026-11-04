"""Terminal browser regressions with a simulated WebSocket; run with Playwright installed.

Set SAIL_TEST_CHROMIUM to use an existing Chromium executable.
Backend PTY/WebSocket tests: cargo test -p sail-dialogue --test terminal_test.
"""
import functools, http.server, threading, re, json, os
from pathlib import Path
from playwright.sync_api import sync_playwright
root=Path(__file__).resolve().parents[1] / 'crates/sail-dialogue/web'
class Handler(http.server.SimpleHTTPRequestHandler):
 def do_GET(self):
  if self.path.startswith('/vendor/'):
   self.path='/vendor/xterm/'+self.path.split('/')[-1]
  if self.path=='/':
   html=(root/'index.html').read_text()
   html=re.sub(r'<script.*?</script>',lambda m:m.group(0) if any(x in m.group(0) for x in ['/terminal.js','/vendor/']) else '',html,flags=re.S)
   self.send_response(200); self.end_headers();self.wfile.write(html.encode()); return
  super().do_GET()
 def log_message(self,*args):pass
server=http.server.ThreadingHTTPServer(('127.0.0.1',0),functools.partial(Handler,directory=str(root)))
threading.Thread(target=server.serve_forever,daemon=True).start()
with sync_playwright() as p:
 browser=p.chromium.launch(executable_path=os.environ.get('SAIL_TEST_CHROMIUM'),headless=True,args=['--no-sandbox'])
 page=browser.new_page(viewport={'width':1440,'height':1000}); errors=[]; sent=[]
 page.on('pageerror',lambda e:errors.append(str(e)))
 def socket(ws):
  ws.on_message(lambda m: sent.append(m))
  ws.send(json.dumps({'type':'connected','shell':'/bin/bash','cwd':'/tmp'}))
 page.route_web_socket('**/ws/terminal',socket)
 page.goto(f'http://127.0.0.1:{server.server_port}/');page.wait_for_function('window.sailTerminal?.term?.cols > 0')
 page.locator('#terminalCard').scroll_into_view_if_needed(); page.evaluate('sailTerminal.term.focus()')
 page.keyboard.type('abc'); page.keyboard.press('Tab');page.keyboard.press('ArrowUp');page.keyboard.press('Control+c');page.keyboard.press('Escape')
 page.wait_for_timeout(100)
 inputs=[json.loads(x)['data'] for x in sent if isinstance(x,str) and json.loads(x)['type']=='input']
 assert ''.join(inputs)=='abc\t\x1b[A\x03\x1b',repr(inputs)
 page.evaluate("sailTerminal.term.write('progress 10%\\rprogress 90%\\x1b[K\\r\\n\\x1b[31mred\\x1b[0m')")
 page.wait_for_timeout(100)
 text=page.evaluate("Array.from({length:sailTerminal.term.buffer.active.length},(_,i)=>sailTerminal.term.buffer.active.getLine(i).translateToString(true)).join('\\n')")
 assert 'progress 90%' in text and 'progress 10%' not in text,text
 page.evaluate("sailTerminal.term.write('\\x1b[?1049hALT')");page.wait_for_timeout(50)
 assert page.evaluate("sailTerminal.term.buffer.active.type")=='alternate'
 page.evaluate("sailTerminal.term.write('\\x1b[?1049l')");page.wait_for_timeout(50)
 assert page.evaluate("sailTerminal.term.buffer.active.type")=='normal'
 page.locator('#terminalCard').screenshot(path='/tmp/sail-terminal-desktop.png')
 desktop_cols=page.evaluate('sailTerminal.term.cols')
 page.set_viewport_size({'width':390,'height':844});page.wait_for_timeout(200)
 mobile_cols=page.evaluate('sailTerminal.term.cols');assert 0<mobile_cols<desktop_cols,(desktop_cols,mobile_cols)
 page.locator('#terminalCard').screenshot(path='/tmp/sail-terminal-mobile.png')
 page.emulate_media(reduced_motion='reduce');page.reload();page.wait_for_function('window.sailTerminal?.term?.cols > 0')
 assert page.evaluate('sailTerminal.term.options.cursorBlink') is False
 assert not errors,errors
 print(f'PASS: keyboard bytes, carriage return, alternate screen, resize ({desktop_cols} -> {mobile_cols} cols), reduced motion; no browser errors')
 browser.close()
server.shutdown()
