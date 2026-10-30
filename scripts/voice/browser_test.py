"""Browser regression test against a running local voice server (requires Playwright)."""
import sys
from pathlib import Path
from playwright.sync_api import sync_playwright
port=int(sys.argv[1]) if len(sys.argv)>1 else 8088
with sync_playwright() as p:
    browser=p.chromium.launch(headless=True,args=['--no-sandbox','--no-proxy-server','--use-fake-device-for-media-stream','--use-fake-ui-for-media-stream','--host-resolver-rules=MAP sail-insecure.test 127.0.0.1'])
    page=browser.new_page(viewport={'width':1280,'height':1000})
    errors=[];page.on('pageerror',lambda e:errors.append(str(e)))
    page.goto(f'http://127.0.0.1:{port}')
    page.wait_for_function('ws && ws.readyState === WebSocket.OPEN')
    page.get_by_role('button',name='Kernel: os:status').click()
    page.wait_for_function('document.getElementById("canvasPanel").textContent.includes("memory_used_gb")')
    page.get_by_role('button',name='Start Microphone').click()
    page.wait_for_function('isRecording',timeout=20000)
    page.wait_for_timeout(1200)
    page.get_by_role('button',name='Stop Microphone').click()
    assert page.evaluate('!isRecording && !micStream && !processor')
    # Restart must not register a duplicate worklet or leak the old media graph.
    page.get_by_role('button',name='Start Microphone').click()
    page.wait_for_function('isRecording',timeout=20000)
    page.get_by_role('button',name='Stop Microphone').click()
    page.evaluate("renderArtifact({artifact_type:'mermaid',content:'graph LR; Silero-->STT; STT-->Session; Session-->Gemma; Gemma-->Kokoro; Kokoro-->Orb'})")
    page.wait_for_selector('#canvasPanel svg')
    page.evaluate("renderArtifact({artifact_type:'text',content:'<img src=x onerror=alert(1)>'})")
    assert page.locator('#canvasPanel img').count()==0
    Path('.local').mkdir(exist_ok=True)
    page.screenshot(path='.local/voice-ui.png',full_page=True)
    assert not errors,errors
    # Reproduce the original LAN HTTP failure: it must give an inline explanation, not throw.
    page.goto(f'http://sail-insecure.test:{port}')
    page.wait_for_function('ws && ws.readyState === WebSocket.OPEN')
    page.get_by_role('button',name='Start Microphone').click()
    page.wait_for_function('document.getElementById("error").textContent.includes("secure connection")')
    assert not page.evaluate('isRecording')
    assert not errors,errors
    browser.close()
    print('Browser capture/restart/cleanup, tool cards, Mermaid, text escaping and insecure-context handling passed')
