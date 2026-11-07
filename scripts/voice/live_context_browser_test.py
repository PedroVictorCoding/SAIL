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
        page.locator('#toolsMap .tm-top button[aria-label="Settings"]').click()
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
        # Enabling Listen without reply while on Arena must never hijack the Arena Learn/Courses panel.
        page.set_viewport_size({'width':1280,'height':900})
        page.evaluate("sailNavigate('arena')")
        expect(page.locator('#subsystemTitle')).to_have_text('Arena')
        expect(page.locator('#learnPanelMain')).to_be_visible()
        page.evaluate("window.setLiveContext(false)")
        expect(button).to_have_attribute('aria-pressed','false')
        page.evaluate("window.setLiveContext(true)")
        expect(button).to_have_attribute('aria-pressed','true')
        expect(page.locator('#learnPanelMain')).to_be_visible()
        expect(page.locator('#memoryPanelTranscript')).to_be_hidden()
        # Open the Live Transcript tab inside Memory from the Settings shortcut.
        page.locator('#toolsMap .tm-top button[aria-label="Settings"]').click()
        page.locator('[data-settings-tab="voice"]').click()
        page.locator('#openLiveTranscriptBtn').click()
        expect(page.locator('#subsystemTitle')).to_have_text('Memory')
        expect(page.locator('#transcriptTab')).to_have_attribute('aria-selected','true')
        expect(page.locator('#memoryPanelTranscript')).to_be_visible()
        expect(page.locator('#ltEnableBtn')).to_have_attribute('aria-pressed','true')
        expect(page.locator('#ltStatusText')).to_have_text('Ready')
        expect(page.locator('#ltPauseLabel')).to_have_text('Record')
        # Partial and observed live transcript segments render chronologically inside the panel.
        page.evaluate("handleVoiceMessage({type:'transcript', text:'Aujourd’hui, nous voyons…', is_final:false})")
        expect(page.locator('#ltPartial')).to_be_visible()
        expect(page.locator('#ltPartial')).to_have_text('Aujourd’hui, nous voyons…')
        page.evaluate("handleVoiceMessage({type:'live_context_observed', text:'Aujourd’hui, nous voyons pourquoi la croissance a ralenti en Europe cette année.'})")
        page.evaluate("handleVoiceMessage({type:'live_context_observed', text:'La première raison, c’est le prix de l’énergie.'})")
        expect(page.locator('#ltPartial')).to_be_hidden()
        expect(page.locator('#ltEmpty')).to_be_hidden()
        expect(page.locator('#ltSegments .lt-segment')).to_have_count(2)
        expect(page.locator('#ltSegments .lt-segment').first).to_contain_text('Aujourd’hui, nous voyons pourquoi la croissance a ralenti')
        # Lines arriving while recording is stopped show capture clock time (not 00:00) and keep Ready status before recording ever starts.
        assert page.locator('#ltSegments .lt-seg-time').first.inner_text() != '00:00'
        expect(page.locator('#ltStatusText')).to_have_text('Ready')
        expect(page.locator('#ltCopyBtn')).to_be_visible()
        expect(page.locator('#ltExportBtn')).to_be_visible()
        # Asking SAIL from the Live Transcript footer sends the panel's full transcript with the question and renders the answer inline.
        page.fill('#ltAskInput', 'Summarize the two reasons from this lecture.')
        page.locator('#ltAskForm button[type="submit"]').click()
        assert page.evaluate("""sentActions.some(a =>
          a.action === 'text' &&
          a.text.includes('Aujourd’hui, nous voyons pourquoi la croissance a ralenti') &&
          a.text.includes('La première raison, c’est le prix de l’énergie.') &&
          a.text.includes('Question: Summarize the two reasons from this lecture.')
        )""")
        page.evaluate("handleVoiceMessage({type:'turn_started', turn_id:1, transcript:'full-payload', state:'thinking'})")
        page.evaluate("handleVoiceMessage({type:'response_delta', text:'1. Energy prices slowed European growth.'})")
        page.evaluate("handleVoiceMessage({type:'turn_finished', state:'idle'})")
        expect(page.locator('#ltAnswer')).to_be_visible()
        expect(page.locator('#ltAnswerQuestion')).to_have_text('Summarize the two reasons from this lecture.')
        expect(page.locator('#ltAnswerBody')).to_have_text('1. Energy prices slowed European growth.')
        page.screenshot(path='.local/live-transcript-tab-desktop.png')
        # Mobile layout check on the Live Transcript tab inside Memory (including inline answer visibility).
        page.set_viewport_size({'width':390,'height':844})
        assert page.evaluate('document.documentElement.scrollWidth<=innerWidth')
        expect(page.locator('#ltAnswer')).to_be_visible()
        page.screenshot(path='.local/live-transcript-tab-mobile.png',full_page=True)
        # Starting a New conversation (resetSession) and reloading the page must preserve the recorded lesson transcript.
        page.evaluate("resetSession()")
        expect(page.locator('#ltSegments .lt-segment')).to_have_count(2)
        page.reload()
        page.wait_for_function("typeof window.sailNavigate === 'function'")
        page.evaluate("sailNavigate('memory', 'memoryPanelTranscript')")
        expect(page.locator('#ltSegments .lt-segment')).to_have_count(2)
        # Stopping and clearing from the Live Transcript tab resets the state and storage.
        page.evaluate("window.setLiveContext(true)")
        expect(page.locator('#ltEnableBtn')).to_have_attribute('aria-pressed','true')
        page.locator('#ltStopBtn').click()
        expect(page.locator('#ltEnableBtn')).to_have_attribute('aria-pressed','false')
        expect(page.locator('#ltStatusText')).to_have_text('Off')
        expect(button).to_have_attribute('aria-pressed','false')
        page.locator('#ltClearBtn').click()
        expect(page.locator('#ltSegments .lt-segment')).to_have_count(0)
        assert not errors,errors
        browser.close()
finally:
    server.shutdown();server.server_close()
print('Live-context consent wiring, backend acknowledgement, keyboard and mobile layout passed (fixture transport)')
