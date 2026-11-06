// Browser fixtures only; run with Node and Playwright installed, or PLAYWRIGHT_MODULE pointing to it.
const {chromium}=require(process.env.PLAYWRIGHT_MODULE || 'playwright');
const fs=require('node:fs');const assert=require('node:assert/strict');
const root=require('node:path').join(__dirname, '../web/');
(async()=>{
const browser=await chromium.launch({headless:true,args:['--no-sandbox']});
const page=await browser.newPage({viewport:{width:1440,height:1000}});const errors=[];page.on('pageerror',e=>errors.push(e.message));
const position={percent:46.8,revision:1,anchor:{section:0,block:0,char:0},saved_at:'2026-11-06T11:40:00Z'};
let books=[{id:'one',title:'The Shape of Attention',authors:['Alex Reader'],format:'epub',sections:1,chars:1000,position},{id:'two',title:'A Field Guide to Quiet Places',authors:['Sam Rivers'],format:'txt',sections:1,chars:1000,position:null},{id:'three',title:'Patterns of the Natural World',authors:['Robin Lee'],format:'epub',sections:1,chars:1000,position:{...position,percent:100}}];
let fail=false, lookupFail=false, noMatches=false, coverFail=false, searches=0, coverRequests=0;
const coverJpeg=Buffer.from(await page.evaluate(() => {
 const canvas=document.createElement('canvas');canvas.width=300;canvas.height=450;
 const c=canvas.getContext('2d');const g=c.createLinearGradient(0,0,300,450);g.addColorStop(0,'#264862');g.addColorStop(1,'#142028');c.fillStyle=g;c.fillRect(0,0,300,450);
 c.fillStyle='#e4d6b7';c.font='28px Georgia';c.fillText('THE SHAPE',35,100);c.fillText('OF ATTENTION',35,140);c.font='16px sans-serif';c.fillText('ALEX READER',35,390);
 return canvas.toDataURL('image/jpeg').split(',')[1];
}), 'base64');
const match={work_key:'/works/OL123W',title:books[0].title,authors:books[0].authors,first_publish_year:2015,edition_count:3,subjects:['Reading','Attention']};
await page.route('**/*',async r=>{
const u=new URL(r.request().url());const path=u.pathname;assert.equal(u.origin,'https://sail.test');
if(path.startsWith('/api/')){
 if(path==='/api/guardian/token')return r.fulfill({json:{token:'t'.repeat(32)}});
 assert.equal(r.request().headers().authorization,'Bearer '+'t'.repeat(32));
 if(fail)return r.fulfill({status:503,json:{error:'Library unavailable. Try again.'}});
 if(path.endsWith('/metadata/search')) {
   searches++;
   assert.equal(r.request().method(),'POST');
   assert.deepEqual(r.request().postDataJSON(),{title:'The Shape of Attention',author:'Alex Reader'});
   if(lookupFail)return r.fulfill({status:502,json:{error:'Open Library unavailable. Try later.'}});
   return r.fulfill({json:{matches:noMatches?[]:[match]}});
 }
 if(path.endsWith('/metadata')) {
   const book=books.find(b=>path.includes(`/books/${b.id}/`));
   if(r.request().method()==='DELETE')book.metadata=null;
   else {assert.deepEqual(r.request().postDataJSON(),{work_key:match.work_key});book.metadata={...match,description:'<img src=x onerror=alert(1)> A book about the way we read.',cover_cached:true,retrieved_at:'2026-11-06T12:00:00Z'};}
   return r.fulfill({json:{book}});
 }
 if(path.endsWith('/cover')) {
   coverRequests++;
   return coverFail?r.fulfill({status:404,json:{error:'No cover'}}):r.fulfill({contentType:'image/jpeg',body:coverJpeg});
 }
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
await page.locator('#search').fill('Rivers');assert.equal(await page.locator('#shelf .book').count(),1);assert.equal(await page.locator('#book-list button').count(),1);
await page.locator('#search').fill('no matches');assert(await page.locator('#empty').isVisible());
await page.locator('#search').fill('');await page.locator('#search').focus();await page.keyboard.press('Tab');assert(await page.evaluate(()=>document.activeElement.classList.contains('book-list-entry')));
await page.locator('#book-list button').nth(1).click();
assert.equal(await page.locator('#continue h2').textContent(),'A Field Guide to Quiet Places');
assert.equal(await page.locator('#book-list button[aria-pressed=true]').count(),1);
await page.locator('#book-list button').first().click();
assert.equal(await page.locator('#continue h2').textContent(),'The Shape of Attention');
assert.equal(searches,0,'opening Library never sends an online request');
await page.getByRole('button',{name:'Find cover & details',exact:true}).click();
lookupFail=true;await page.locator('#metadata-search').click();await page.getByText('Open Library unavailable. Try later.').waitFor();
lookupFail=false;noMatches=true;await page.locator('#metadata-search').click();await page.getByText('No matches found.',{exact:false}).waitFor();
noMatches=false;await page.locator('#metadata-search').click();await page.getByRole('button',{name:'Use these details'}).click();
await page.locator('#metadata-dialog').waitFor({state:'hidden'});
await page.waitForFunction(()=>document.querySelector('#continue .cover-image')?.style.opacity==='1');
assert.equal(await page.locator('#continue .book-description img').count(),0);
assert((await page.locator('#continue .book-description').textContent()).startsWith('<img'));
assert.equal(coverRequests,1,'one authenticated local image fetch is shared by all book thumbnails');
for(const width of [1440,768,390,360]){
 await page.setViewportSize({width,height:1000});
 assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),`overflow ${width}`);
 await page.screenshot({path:`/tmp/library-${width}.png`,fullPage:true});
}
await page.getByRole('button',{name:'Continue reading →',exact:true}).click();await page.locator('#pages p').waitFor();assert(await page.locator('#library').isHidden());
await page.locator('#library-button').click();await page.locator('#shelf .book').first().waitFor();
page.once('dialog',d=>d.accept());await page.getByRole('button',{name:'Remove A Field Guide to Quiet Places from library',exact:true}).click();await page.waitForFunction(()=>document.querySelectorAll('#shelf .book').length===2);
await page.locator('#import').setInputFiles({name:'sample.txt',mimeType:'text/plain',buffer:Buffer.from('An imported book.')});
await page.waitForFunction(()=>document.querySelectorAll('#shelf .book').length===3);
await page.locator('#continue .book-about summary').click();
await page.getByRole('button',{name:'Change book details',exact:true}).click();
await page.locator('#metadata-close').click();
await page.getByRole('button',{name:'Remove online details',exact:true}).click();
await page.getByRole('button',{name:'Find cover & details',exact:true}).waitFor();
assert.equal(await page.locator('#continue .cover-image').count(),0);
// A missing cached cover keeps a usable typographic cover.
books[0].metadata={...match,cover_cached:true,retrieved_at:'2026-11-06T13:00:00Z'};coverFail=true;
await page.reload();await page.locator('#shelf .book').first().waitFor();
assert(await page.getByRole('button',{name:'Open The Shape of Attention',exact:true}).first().isVisible());
await page.emulateMedia({forcedColors:'active',reducedMotion:'reduce'});await page.screenshot({path:'/tmp/library-forced-colors.png',fullPage:true});
books=[];await page.reload();await page.locator('#empty').waitFor();assert.equal(await page.locator('#shelf .book').count(),0);
fail=true;await page.reload();await page.getByRole('status').filter({hasText:'Library unavailable'}).waitFor();
assert.deepEqual(errors,[]);console.log('PASS: 4 widths, live record projection, search/no results, keyboard, resume, metadata search/selection/cache/fallback/removal, import/removal, empty/error, forced colors/reduced motion; zero JS errors.');await browser.close();
})().catch(e=>{console.error(e);process.exit(1)});
