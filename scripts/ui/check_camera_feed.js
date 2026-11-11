// GJS check of the live-feed governor in crates/sail-dialogue/web/ui.js: the owner's own
// camera must keep a steady rate, while the desktop capture stays CPU-adaptive.
// Run from the repository root: gjs scripts/ui/check_camera_feed.js
const GLib=imports.gi.GLib;
const source=imports.byteArray.toString(GLib.file_get_contents('crates/sail-dialogue/web/ui.js')[1]);
const start=source.indexOf('const AdaptiveFeedEngine = {');
const end=source.indexOf('window.AdaptiveFeedEngine = AdaptiveFeedEngine;');
if (start < 0 || end <= start) throw new Error('feed engine markers not found');

const nodes={
  cameraRateBadge:{textContent:''},
  cameraOverlayStats:{textContent:''},
  desktopRateBadge:{textContent:''},
  desktopOverlayStats:{textContent:''},
  governorCpuPercent:{textContent:''},
  governorCpuFill:{style:{}},
  governorFpsTarget:{textContent:''},
  governorFpsFill:{style:{}},
  governorStatusBadge:{textContent:'',className:''}
};
globalThis.document={getElementById:id=>nodes[id]??null};
const scheduled=[];
globalThis.setTimeout=(_fn,ms)=>{scheduled.push(ms);return 0;};
function check(ok,what){if(!ok)throw new Error('Camera feed assertion failed: '+what);}

eval(source.slice(start,end)+`
const engine=AdaptiveFeedEngine;
check(engine.cameraPreviewMs===250,'the camera preview interval is a fixed 250 ms');

engine.cpuUsage=5; engine.cameraActive=true; engine.updateAllBadges();
const calmBadge=nodes.cameraRateBadge.textContent;
engine.cpuUsage=95; engine.updateAllBadges();
const busyBadge=nodes.cameraRateBadge.textContent;
check(calmBadge===busyBadge,'the camera badge does not change with CPU load');
check(busyBadge.includes('4.0 FPS (250ms)'),'the camera badge shows its fixed rate: '+busyBadge);
check(!/adaptive|inverse|matched/i.test(busyBadge),'the camera badge is not CPU-matched: '+busyBadge);
check(nodes.cameraOverlayStats.textContent.includes('independent of CPU'),'the camera overlay says the rate is independent of CPU');

check(engine.calculateInterval()>2000,'the desktop interval still slows under high CPU');
scheduled.length=0;
engine.tickCamera();
check(scheduled.length===1&&scheduled[0]===250,'the camera timer uses the fixed 250 ms preview rate');
engine.cameraActive=false;
print('Camera steady-rate and desktop-adaptive checks passed');
`);
