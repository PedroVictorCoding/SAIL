// Browser fixtures only; run with Node and Playwright installed, or PLAYWRIGHT_MODULE pointing to it.
const {chromium}=require(process.env.PLAYWRIGHT_MODULE || 'playwright');
const fs=require('node:fs');const assert=require('node:assert/strict');
const root=require('node:path').join(__dirname, '../web/');
(async()=>{
const browser=await chromium.launch({headless:true,args:['--no-sandbox']});
const page=await browser.newPage({viewport:{width:1440,height:1000}});const errors=[];page.on('pageerror',e=>errors.push(e.message));
const position={percent:46.8,revision:1,anchor:{section:0,block:0,char:0},saved_at:'2026-11-06T11:40:00Z'};
let books=[{id:'one',title:'The Shape of Attention',authors:['Alex Reader'],format:'epub',sections:1,chars:1000,position},{id:'two',title:'A Field Guide to Quiet Places',authors:['Sam Rivers'],format:'txt',sections:1,chars:1000,position:null},{id:'three',title:'Patterns of the Natural World',authors:['Robin Lee'],format:'epub',sections:1,chars:1000,position:{...position,percent:100}}];
let fail=false;
await page.route('**/*',async r=>{
const u=new URL(r.request().url());const path=u.pathname;
if(path.startsWith('/api/')){
 if(path==='/api/guardian/token')return r.fulfill({json:{token:'t'.repeat(32)}});
 assert.equal(r.request().headers().authorization,'Bearer '+'t'.repeat(32));
 if(fail)return r.fulfill({status:503,json:{error:'Library unavailable. Try again.'}});
 if(path.endsWith('/import')) {
   assert.equal(r.request().headers()['x-filename'], 'sample.txt');
   assert.equal(r.request().postData(), 'An imported book.');
   books.push({...books[0], id:'imported', title:'Imported book', position:null});
   return r.fulfill({json:{book:books[books.length-1]}});
 }
 if(path.endsWith('/library'))return r.fulfill({json:{books}});
 if(path.endsWith('/position'))return r.fulfill({json:{position}});
 if(path.includes('/sections/'))return r.fulfill({json:{index:0,blocks:[{kind:'p',text:'A quiet page. '.repeat(100)}]}});
 if(r.request().method()==='DELETE'){books=books.filter(b=>!path.endsWith(b.id));return r.fulfill({status:204});}
 return r.fulfill({json:{book:books[0],toc:[],sections:[{index:0,title:'Chapter one',chars:1000,blocks:1}]}});
}
const f=root+(path==='/reader'?'reader.html':path.slice(1));
if(!fs.existsSync(f))return r.fulfill({status:404});
return r.fulfill({contentType:path.endsWith('.css')?'text/css':path.endsWith('.js')?'text/javascript':'text/html',body:fs.readFileSync(f)});
});
await page.goto('https://sail.test/reader');await page.locator('#shelf .book').first().waitFor();
assert.equal(await page.locator('#shelf .book').count(),3);assert.equal(await page.locator('#reading-activity article').count(),2);
assert.equal(await page.locator('progress').getAttribute('aria-label'),'Reading progress for The Shape of Attention');
for(const width of [1440,768,390,360]){
 await page.setViewportSize({width,height:1000});
 assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),`overflow ${width}`);
 await page.screenshot({path:`/tmp/library-${width}.png`,fullPage:true});
}
await page.locator('#search').fill('Rivers');assert.equal(await page.locator('#shelf .book').count(),1);assert.equal(await page.locator('#book-list button').count(),1);
await page.locator('#search').fill('no matches');assert(await page.locator('#empty').isVisible());
await page.locator('#search').fill('');await page.locator('#search').focus();await page.keyboard.press('Tab');assert(await page.evaluate(()=>document.activeElement.classList.contains('book-list-entry')));
await page.getByRole('button',{name:'Continue reading →',exact:true}).click();await page.locator('#pages p').waitFor();assert(await page.locator('#library').isHidden());
await page.locator('#library-button').click();await page.locator('#shelf .book').first().waitFor();
page.once('dialog',d=>d.accept());await page.getByRole('button',{name:'Remove A Field Guide to Quiet Places from library',exact:true}).click();await page.waitForFunction(()=>document.querySelectorAll('#shelf .book').length===2);
await page.locator('#import').setInputFiles({name:'sample.txt',mimeType:'text/plain',buffer:Buffer.from('An imported book.')});
await page.waitForFunction(()=>document.querySelectorAll('#shelf .book').length===3);
await page.emulateMedia({forcedColors:'active',reducedMotion:'reduce'});await page.screenshot({path:'/tmp/library-forced-colors.png',fullPage:true});
books=[];await page.reload();await page.locator('#empty').waitFor();assert.equal(await page.locator('#shelf .book').count(),0);
fail=true;await page.reload();await page.getByRole('status').filter({hasText:'Library unavailable'}).waitFor();
assert.deepEqual(errors,[]);console.log('PASS: 4 widths, live record projection, search/no results, keyboard, resume, import/removal, empty/error, forced colors/reduced motion; zero JS errors.');await browser.close();
})().catch(e=>{console.error(e);process.exit(1)});
