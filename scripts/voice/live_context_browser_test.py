"""Offline browser fixture: settings consent, acknowledged state and mobile layout."""
import threading
import os
from pathlib import Path
from http.server import ThreadingHTTPServer
from playwright.sync_api import sync_playwright, expect
from subsystem_pages_test import UIHandler

server = ThreadingHTTPServer(('127.0.0.1', 0), UIHandler)
threading.Thread(target=server.serve_forever, daemon=True).start()
try:
    with sync_playwright() as p:
        browser = p.chromium.launch(headless=True, executable_path=os.environ.get('SAIL_TEST_CHROMIUM'))
        page = browser.new_page(viewport={'width':1280,'height':900}, reduced_motion='reduce')
        errors=[]
        page.on('pageerror', lambda error: errors.append(str(error)))
        page.add_init_script('''
          window.sentActions=[];
          window.WebSocket=class {
            static OPEN=1;
            constructor(){this.readyState=1;setTimeout(()=>this.onopen?.(),30);}
            send(raw){if(typeof raw!=='string')return; const data=JSON.parse(raw);sentActions.push(data);
              if(data.action==='set_live_context') setTimeout(()=>this.onmessage({data:JSON.stringify({type:'live_context_state',enabled:data.enabled})}),20);
            }
            close(){this.readyState=3;}
          };
        ''')
        page.goto(f'http://127.0.0.1:{server.server_port}')
        page.wait_for_function("typeof sendAction === 'function'")
        page.evaluate("getOrFetchOwnerToken=async()=> 'fixture-owner-token-not-a-secret'")
        page.locator('#settingsRail').click()
        page.locator('[data-settings-tab="voice"]').click()
        button=page.locator('#liveContextBtn')
        expect(button).to_have_attribute('aria-pressed','false')
        button.focus();page.keyboard.press('Enter')
        expect(button).to_have_attribute('aria-pressed','true')
        assert page.evaluate("sentActions.find(a=>a.action==='set_live_context').owner_token") == 'fixture-owner-token-not-a-secret'
        assert page.evaluate('isRecording') is False, 'enabling must not acquire microphone'
        expect(page.locator('#liveContextStatus')).to_contain_text('type to ask')
        Path('.local').mkdir(exist_ok=True)
        page.locator('#liveContextSetting').screenshot(path='.local/live-context-desktop.png')
        page.set_viewport_size({'width':390,'height':844})
        page.locator('#liveContextSetting').scroll_into_view_if_needed()
        assert page.evaluate('document.documentElement.scrollWidth<=innerWidth')
        page.wait_for_function("document.getElementById('liveContextBtn').getBoundingClientRect().right <= innerWidth")
        box=button.bounding_box();assert box['x']>=0 and box['x']+box['width']<=390
        Path('.local').mkdir(exist_ok=True)
        page.screenshot(path='.local/live-context-mobile.png',full_page=True)
        # A delayed on-device transcript must not survive a consent/mute/reset change.
        page.evaluate('''async () => {
          clientSpeechActive.stt = true;
          let finish;
          SailClientSpeech.takeCapture = () => new Float32Array(512);
          SailClientSpeech.transcribe = () => new Promise(resolve => { finish = resolve; });
          const pending = commitClientCapture();
          discardListeningCapture(); finish({text:'late ambient private speech'}); await pending;
          if (sentActions.some(a => a.action === 'voice_text')) throw Error('revoked transcript sent');
        }''')
        button.click();expect(button).to_have_attribute('aria-pressed','false')
        assert not errors,errors
        browser.close()
finally:
    server.shutdown();server.server_close()
print('Live-context consent wiring, backend acknowledgement, keyboard and mobile layout passed (fixture transport)')
