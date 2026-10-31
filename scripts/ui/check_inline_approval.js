const GLib=imports.gi.GLib;
const source=imports.byteArray.toString(GLib.file_get_contents('crates/sail-dialogue/web/voice.js')[1]);
const nodes=new Map(),buttons=[];
function element(){return {children:[],style:{},dataset:{},textContent:'',className:'',disabled:false,append(...items){this.children.push(...items);},prepend(item){this.children.unshift(item);if(item.id)nodes.set(item.id,item);},replaceChildren(){this.children=[];},scrollIntoView(){},setAttribute(){},addEventListener(name,fn){this[name]=fn;},set innerHTML(text){if(text.includes('<img'))throw new Error('Untrusted markup inserted');}};}
nodes.set('canvasPanel',element());nodes.set('forgeActiveJob',element());
const document={getElementById:id=>nodes.get(id)||null,createElement(tag){const e=element();if(tag==='button')buttons.push(e);return e;},querySelectorAll:()=>[]};
let requests=[],resolved=0,lastModal;
function openCardModal(options){lastModal=options;}
async function ownerApprovalRequest(path,body){requests.push(path);return path==='approvals'?{pending:['pending','modal-pending'].map(session_id=>({session_id,requester:'dialogue',capability:{arguments:'exact owner-reviewed input'},expires_at:new Date().toISOString()}))}:{token:{token_id:'grant'}};}
function handleApprovalResolved(){resolved++;}function showError(error){throw new Error(error);}function sendAction(){throw new Error('Approval must never use unauthenticated WebSocket resolution');}
function updateState(){}const forgeJobStatus=new Map();
const loop=new GLib.MainLoop(null,false);
const card=source.slice(source.indexOf('function renderApprovalCard('),source.indexOf('function handleApprovalResolved('));
eval(card+`
(async()=>{
 renderApprovalCard({session_id:'pending',tool:'forge:enqueue'});
 const button=buttons.find(b=>b.textContent==='Review exact request');
 if(!button)throw new Error('Inline approval must require exact review');
 await button.click({stopPropagation(){}});
 if(requests.join(',')!=='approvals')throw new Error('First click must only retrieve exact scope');
 await button.click({stopPropagation(){}});
 if(requests.join(',')!=='approvals,resolve'||resolved!==1)throw new Error('Second click must authenticate once without resubmission');
 renderApprovalCard({session_id:'modal-pending',tool:'forge:enqueue'});
 const expand=buttons.filter(b=>b.className==='card-expand-btn').at(-1);
 expand.click({stopPropagation(){}});
 const modal=lastModal.footerNode.children.find(b=>b.textContent==='Review exact request');
 await modal.click();
 if(requests.join(',')!=='approvals,resolve,approvals')throw new Error('Modal must retrieve its own exact scope before approval');
 if(!lastModal.bodyNode.children.some(n=>String(n.textContent).includes('exact owner-reviewed input')))throw new Error('Exact scope must be visible in modal');
 await modal.click();
 if(requests.join(',')!=='approvals,resolve,approvals,resolve'||resolved!==2)throw new Error('Modal must use authenticated resumption without duplicate execution');
 print('Inline and modal approvals require authenticated exact-scope review before continuing');
})().catch(e=>{printerr(e.stack);imports.system.exit(1);}).finally(()=>loop.quit());`);
loop.run();
