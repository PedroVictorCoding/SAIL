const GLib=imports.gi.GLib;
const source=imports.byteArray.toString(GLib.file_get_contents('crates/sail-dialogue/web/voice.js')[1]);
const nodes=new Map(),buttons=[];
function element(){return {children:[],dataset:{},textContent:'',className:'',disabled:false,append(...items){this.children.push(...items);},prepend(item){this.children.unshift(item);if(item.id)nodes.set(item.id,item);},replaceChildren(){this.children=[];},scrollIntoView(){},addEventListener(name,fn){this[name]=fn;},set innerHTML(text){if(text.includes('<img'))throw new Error('Untrusted markup inserted');}};}
nodes.set('canvasPanel',element());nodes.set('forgeActiveJob',element());
const document={getElementById:id=>nodes.get(id)||null,createElement(tag){const e=element();if(tag==='button')buttons.push(e);return e;},querySelectorAll:()=>[]};
let requests=[],resolved=0;
async function ownerApprovalRequest(path,body){requests.push(path);return path==='approvals'?{pending:[{session_id:'pending',requester:'dialogue',capability:{arguments:'exact owner-reviewed input'},expires_at:new Date().toISOString()}]}:{token:{token_id:'grant'}};}
function handleApprovalResolved(){resolved++;}function showError(error){throw new Error(error);}function sendAction(){throw new Error('Approval must never use unauthenticated WebSocket resolution');}
function updateState(){}const forgeJobStatus=new Map();
const loop=new GLib.MainLoop(null,false);
const card=source.slice(source.indexOf('function renderApprovalCard('),source.indexOf('function handleApprovalResolved('));
eval(card+`
(async()=>{
 renderApprovalCard({session_id:'pending',tool:'forge:enqueue'});
 const button=buttons.find(b=>b.textContent==='Review exact request');
 if(!button)throw new Error('Inline approval must require exact review');
 await button.click();
 if(requests.join(',')!=='approvals')throw new Error('First click must only retrieve exact scope');
 await button.click();
 if(requests.join(',')!=='approvals,resolve'||resolved!==1)throw new Error('Second click must authenticate once without resubmission');
 print('Inline approval requires authenticated exact-scope review before continuing');
})().catch(e=>{printerr(e.stack);imports.system.exit(1);}).finally(()=>loop.quit());`);
loop.run();
