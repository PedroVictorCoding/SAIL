// Run with: gjs scripts/ui/check_svg_isolation.js
// Verify that untrusted SVG reaches an image URL, never active page markup.
const GLib = imports.gi.GLib;
const [, bytes] = GLib.file_get_contents('crates/sail-dialogue/web/card-modal.js');
const source = new TextDecoder().decode(bytes);
const start = source.indexOf('async function renderArtifact(');
const end = source.length;
const panel = { children:[], append(node) { this.children.push(node); } };
function element(tag) {
  return { tag, children:[], style:{}, setAttribute(){}, addEventListener(name,fn){this[name]=fn;}, replaceChildren(){this.children=[];}, showModal(){this.open=true;}, append(...nodes) { this.children.push(...nodes); },
    set innerHTML(value) { throw new Error('Untrusted markup reached the page DOM'); } };
}
const modalNodes=new Map(['cardModal','cardModalIcon','cardModalTitle','cardModalBadge','cardModalBody','cardModalFooter'].map(id=>[id,element('div')]));
const document = { createElement:element, getElementById(id) { return id === 'canvasPanel' ? panel : modalNodes.get(id)||null; } };
const render = new Function('document','window',`${source}; return renderArtifact;`)(document,{});
const malicious = '<svg xmlns="http://www.w3.org/2000/svg" onload="alert(document.cookie)"><script>alert(1)</script><rect width="10" height="10"/></svg>';
render({artifact_type:'svg',content:malicious}).then(async () => {
  const image = panel.children[0].children.find(node => node.tag === 'img');
  if (!image || !image.src.startsWith('data:image/svg+xml;charset=utf-8,')) throw new Error('SVG must use image-document isolation');
  panel.children[0].click();
  const full=modalNodes.get('cardModalBody').children[0];
  if(!full||full.tag!=='img'||full.src!==image.src)throw new Error('Modal SVG must retain image-document isolation');
  if (decodeURIComponent(image.src.split(',')[1]) !== malicious) throw new Error('Visual source was not preserved');
  await render({artifact_type:'html',content:'<img onerror="alert(1)">'});
  await render({artifact_type:'svg',content:null});
  if (panel.children.length !== 1) throw new Error('Nonvisual or malformed content was accepted');
  print('SVG isolation and malformed-artifact checks passed');
}).catch(error => { printerr(error.message); imports.system.exit(1); });
// GJS jobs above contain no asynchronous I/O; run the promise queue before exit.
