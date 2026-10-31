const GLib=imports.gi.GLib;
const source=imports.byteArray.toString(GLib.file_get_contents('crates/sail-dialogue/web/voice.js')[1]);
function element(){return {children:[],style:{},textContent:'',className:'',append(...nodes){this.children.push(...nodes);},setAttribute(){},addEventListener(type,fn){this[type]=fn;},close(){},set innerHTML(text){if(text.includes('<img'))throw new Error('Issue text inserted as HTML');}};}
const panel=element(),dialog=element();
const document={createElement:element,getElementById:id=>id==='canvasPanel'?panel:id==='cardModal'?dialog:null};
let modal,sent;
function openCardModal(options){modal=options;}function sendAction(action,fields){sent={action,...fields};}
const render=new Function('document','openCardModal','sendAction',source.slice(source.indexOf('function renderToolResult('),source.indexOf('function updateSystemCard('))+';return renderToolResult;')(document,openCardModal,sendAction);
render({tool:'github:issues',success:true,output:{count:2,issues:[{number:43,title:'<img src=x onerror=alert(1)>',priority:'P0'},{number:60,title:'Secondary work',priority:'P1'}]}});
panel.children[0].click();
function flatten(node){return [node,...(node.children||[]).flatMap(flatten)];}
const nodes=flatten(modal.bodyNode),pills=nodes.filter(n=>n.className.startsWith('forge-priority-pill'));
if(pills.map(n=>n.textContent).join(',')!=='P0,P1')throw new Error('Priority ordering/badges were lost in modal');
const solve=nodes.find(n=>n.textContent==='Solve with Forge');solve.click({stopPropagation(){}});
if(sent.action!=='forge:enqueue'||sent.issue_number!==43||sent.task!=='<img src=x onerror=alert(1)>'||'priority' in sent)throw new Error('Forge selection must preserve the schema-compatible request');
print('Priority badges, issue ordering, safe titles and Forge selection pass');
