"""Offline memory UI regression; the Rust memory_http test covers real authorization."""
import json
import threading
from http.server import ThreadingHTTPServer
from playwright.sync_api import sync_playwright, expect
from subsystem_pages_test import UIHandler

changes=[]
class MemoryHandler(UIHandler):
    def do_GET(self):
        if self.path == '/api/guardian/token':
            self.reply({'token':'offline-browser-test-owner-token-32-bytes'});return
        super().do_GET()
    def reply(self,data):
        body=json.dumps(data).encode();self.send_response(200);self.send_header('Content-Type','application/json');self.send_header('Content-Length',str(len(body)));self.end_headers();self.wfile.write(body)
    def do_POST(self):
        data=json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        entity={'id':1,'name':'SAIL','kind':'software','scope':'voice','sensitivity':'private','aliases':['Assistant'],'merged_into':None}
        claim={'id':10,'predicate':'theme','object':{'type':'value','value':'<img src=x onerror=alert(1)>'},'status':'user_reported','observed_at':1}
        if self.path.endswith('/change'):changes.append(data);self.reply({'ok':True,'generation':0});return
        if data['action']=='overview':self.reply({'entities':[[entity,True]],'history':[{'at':1,'op':'assert','target_kind':'claim','target_id':10}],'generation':0})
        elif data['action']=='entity':self.reply({'entity':entity,'claims':[claim],'why':[{'claim_id':10,'evidence':[{'stance':'supports','evidence':{'source_kind':'user_statement','source_ref':'archive:1','receipt':'Owner preference'}}]}],'graph':[],'procedures':[],'project_items':[],'generation':0})
        elif data['action']=='search':self.reply({'generation':0,'hits':[{'entity_id':1,'entity_name':'SAIL','predicate':'theme','value':'orange','status':'user_reported'}]})
        else:self.reply({'version':1,'entities':[entity]})

server=ThreadingHTTPServer(('127.0.0.1',0),MemoryHandler)
threading.Thread(target=server.serve_forever,daemon=True).start()
try:
    with sync_playwright() as p:
        browser=p.chromium.launch(headless=True)
        page=browser.new_page(viewport={'width':1440,'height':1000},reduced_motion='reduce')
        errors=[];page.on('pageerror',lambda e:errors.append(str(e)));page.on('dialog',lambda dialog:dialog.accept())
        page.goto(f'http://127.0.0.1:{server.server_port}/#arena')
        page.get_by_role('button',name='Pinned · SAIL').click()
        expect(page.locator('#memoryDetail h3')).to_have_text('SAIL #1')
        expect(page.locator('#memoryDetail h4')).to_contain_text('<img src=x')
        assert page.locator('#memoryDetail img').count()==0
        page.get_by_text('Why remembered',exact=True).click();expect(page.get_by_text('supports · user_statement',exact=False)).to_be_visible()
        page.get_by_role('button',name='Pin',exact=True).click();page.wait_for_function("document.getElementById('memoryStatus').textContent.includes('entities shown')")
        page.get_by_label('Correct this value').fill('orange');page.get_by_role('button',name='Save correction').click()
        page.wait_for_timeout(200)
        assert any(change['action']['action']=='correct' and change['action']['value']=='orange' for change in changes)
        page.set_viewport_size({'width':390,'height':844});assert page.evaluate('document.documentElement.scrollWidth<=innerWidth')
        page.screenshot(path='.local/memory-mobile.png',full_page=True)
        assert not errors,errors
        browser.close()
finally:
    server.shutdown();server.server_close()
print('Memory views, safe text, source receipts, owner action wiring and mobile layout passed')
