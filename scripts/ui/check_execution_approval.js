// Verify that approving a scheduled operation never sends a second tool request.
const GLib=imports.gi.GLib;
const source=imports.byteArray.toString(GLib.file_get_contents('crates/sail-dialogue/web/voice.js')[1]);
const elements=new Map(), buttons=[];
function element(){return {children:[],textContent:'',className:'',value:'',addEventListener(type,fn){this[type]=fn;},append(...nodes){this.children.push(...nodes);},replaceChildren(){this.children=[];}};}
const document={getElementById(id){if(!elements.has(id))elements.set(id,element());return elements.get(id);},createElement(tag){const e=element();if(tag==='button')buttons.push(e);return e;}};
const window={addEventListener(){}};
const pendingApprovalActions=new Map([['request-1',{automatic:true}]]),lastToolCalls=new Map();
let approved=false,resolveCalls=0,toolCalls=0;
async function ownerApprovalRequest(path,body){if(path==='approvals')return {pending:approved?[]:[{session_id:'request-1',requester:'dialogue',risk_level:'high',capability:{tool:'proc:list'},expires_at:new Date().toISOString()}]};if(path==='resolve'){approved=true;resolveCalls++;return {token:{token_id:'grant'}};}throw new Error(path);}
function runKernelTool(){toolCalls++;} function showError(e){throw new Error(e);}
const loop=new GLib.MainLoop(null,false);
eval(source.slice(source.indexOf('async function refreshOwnerApprovals()'))+`
(async()=>{
 await refreshOwnerApprovals();
 const approve=buttons.find(b=>b.textContent==='Approve and continue');
 if(!approve)throw new Error('Scheduled operation must expose a continue button');
 await approve.click();
 if(resolveCalls!==1||toolCalls!==0)throw new Error('Approval must resume the server operation, not submit a second execution');
 if(pendingApprovalActions.has('request-1'))throw new Error('Resolved request must leave pending UI state');
 print('Owner approval resumes scheduled execution without duplicate submission');
})().catch(e=>{printerr(e.stack);imports.system.exit(1);}).finally(()=>loop.quit());`);
loop.run();
