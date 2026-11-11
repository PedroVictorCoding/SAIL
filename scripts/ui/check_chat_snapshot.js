// GJS check of the conversation snapshot preview in crates/sail-dialogue/web/voice.js.
// Run from the repository root: gjs scripts/ui/check_chat_snapshot.js
const GLib=imports.gi.GLib;
const source=imports.byteArray.toString(GLib.file_get_contents('crates/sail-dialogue/web/voice.js')[1]);
const start=source.indexOf('// A camera or screen snapshot also belongs in the conversation');
const end=source.indexOf('window.appendConversationImage = appendConversationImage;');
if (start < 0 || end <= start) throw new Error('chat snapshot markers not found');

function element(tag){return {tag,children:[],className:'',textContent:'',src:'',alt:'',loading:'',append(...items){this.children.push(...items);}};}
const conversation=element('div');
globalThis.document={createElement:element,getElementById:id=>id==='conversation'?conversation:null};
let resumes=0;
const chatFollow={resume(){resumes++;}};
function check(ok,what){if(!ok)throw new Error('Chat snapshot assertion failed: '+what);}

eval(source.slice(start,end)+'window.appendConversationImage = appendConversationImage;'+`
const node=appendConversationImage('You','data:image/jpeg;base64,cGlj','Camera snapshot');
check(node.className==='message message-you message-image','image message uses the conversation classes');
check(node.children[0].textContent==='You','role label is shown');
check(node.children[1].children[0].src==='data:image/jpeg;base64,cGlj','image data URL is used as the source');
check(node.children[1].children[0].alt==='Camera snapshot','image alt text is the caption');
check(node.children[1].children[1].textContent==='Camera snapshot','caption is shown under the image');
check(conversation.children.includes(node),'image message is appended to the conversation');
check(resumes===1,'the conversation follows the new image');

const before=conversation.children.length;
check(appendConversationImage('You','javascript:alert(1)','Bad')===null,'non-image sources are refused');
check(conversation.children.length===before,'a refused source adds nothing to the conversation');
print('Chat snapshot preview checks passed');
`);
