// SAIL Factory Floor - Isometric Room-as-Index Interface
'use strict';

const FACTORY = {
  gridSize: 24, floorColor: '#17191D', gridDotColor: '#2A2D33', lineColor: '#3A3E45',
  accent: '#F26B21', okColor: '#5FD3A6', alertColor: '#FF4438', textColor: '#C9CDD4',
  selectedMachine: null, sceneIndex: 0, scenes: ['Overview', 'Ops', 'Body', 'Comms']
};

// Station registry - all subsystems fixed on factory floor
FACTORY.machineRegistry = [
  {id:'arena',name:'ARENA',type:'tank',x:4,y:4,w:2,h:2,metric:'Knowledge'},
  {id:'mind',name:'MIND',type:'tank',x:8,y:3,w:2,h:3,metric:'Model'},
  {id:'dialogue',name:'DIALOGUE',type:'ticker',x:2,y:2,w:4,h:1,metric:'Chat'},
  {id:'guardian',name:'GUARDIAN',type:'canister',x:12,y:5,w:1,h:1,metric:'Security'},
  {id:'forge',name:'FORGE',type:'junction',x:6,y:7,w:2,h:2,metric:'Coding'},
  {id:'pulse',name:'PULSE',type:'conveyor',x:10,y:8,w:4,h:1,metric:'Timeline'},
  {id:'canvas',name:'CANVAS',type:'monitor',x:3,y:8,w:2,h:2,metric:'View'},
  {id:'bridge',name:'BRIDGE',type:'pipe',x:14,y:2,w:3,h:1,metric:'External'},
  {id:'flow',name:'FLOW',type:'valve',x:5,y:5,w:1,h:1,metric:'Queue'},
  {id:'discovery',name:'DISCOVERY',type:'radar',x:1,y:6,w:1,h:1,metric:'Search'},
  {id:'capacity',name:'CAPACITY',type:'gastank',x:11,y:2,w:2,h:2,metric:'Storage'},
  {id:'anomaly',name:'ANOMALY',type:'pylons',x:0,y:9,w:1,h:1,metric:'Alerts'}
];

FACTORY.pipeRegistry = [
  {from:{x:5,y:5},to:{x:4,y:4}}, {from:{x:5,y:5},to:{x:8,y:3}},
  {from:{x:4,y:4},to:{x:2,y:2}}, {from:{x:8,y:3},to:{x:11,y:2}},
  {from:{x:11,y:2},to:{x:14,y:2}}, {from:{x:8,y:3},to:{x:12,y:5}},
  {from:{x:4,y:4},to:{x:6,y:7}}, {from:{x:6,y:7},to:{x:3,y:8}},
  {from:{x:6,y:7},to:{x:10,y:8}}, {from:{x:4,y:4},to:{x:3,y:8}},
  {from:{x:3,y:8},to:{x:1,y:6}}, {from:{x:3,y:8},to:{x:0,y:9}}
];


function initFactory() {
  const shell = document.getElementById('appShell');
  shell.classList.add('factory-mode'); shell.dataset.state = 'factory';
  if (document.getElementById('factoryFloor')) return;
  const floor = document.createElement('div'); floor.id = 'factoryFloor'; floor.className = 'factory-floor';
  floor.innerHTML = `<canvas id="factoryCanvas" class="factory-canvas"></canvas>
    <div class="factory-top-bar"><span class="factory-time">${new Date().toLocaleString()}</span>
    <span class="factory-live"><i></i> LIVE <span id="latencyReadout">0ms</span></span></div>
    <div class="factory-bottom-bar"><button class="scene-nav" id="scenePrev">◀</button>
    <span class="scene-label" id="sceneLabel">Overview</span><button class="scene-nav" id="sceneNext">▶</button>
    <div class="scene-list">${FACTORY.scenes.map((s,i)=>`<button class="scene-btn ${i===0?'active':''}" data-scene="${i}">${s}</button>`).join('')}</div></div>
    <div class="factory-inspector" id="factoryInspector" hidden><h3 id="inspectorTitle"></h3><div id="inspectorMetrics"></div></div>`;
  shell.appendChild(floor);
  document.getElementById('scenePrev').onclick = () => setScene(FACTORY.sceneIndex-1);
  document.getElementById('sceneNext').onclick = () => setScene(FACTORY.sceneIndex+1);
  document.querySelectorAll('.scene-btn').forEach(b=>b.onclick=()=>setScene(Number(b.dataset.scene)));
  injectFactoryStyles(); renderFactory();
}

function setScene(i) {
  FACTORY.sceneIndex = Math.max(0, Math.min(FACTORY.scenes.length-1, i));
  document.getElementById('sceneLabel').textContent = FACTORY.scenes[FACTORY.sceneIndex];
  document.querySelectorAll('.scene-btn').forEach((b,n)=>b.classList.toggle('active',n===FACTORY.sceneIndex));
}

function injectFactoryStyles() {
  const s = document.createElement('style'); s.textContent = `
  .factory-mode .workspace, .factory-mode aside, .factory-mode nav {display:none !important;}
  .factory-floor {position:absolute;inset:96px 0 0 138px;background:#17191D;overflow:hidden;}
  .factory-canvas {width:100%;height:100%;display:block;}
  .factory-top-bar {position:absolute;top:12px;left:50%;transform:translateX(-50%);display:flex;gap:24px;color:#C9CDD4;font:11px monospace;}
  .factory-bottom-bar {position:absolute;bottom:12px;left:50%;transform:translateX(-50%);display:flex;align-items:center;gap:12px;color:#C9CDD4;font:11px monospace;}
  .scene-nav, .scene-btn {background:#22262A;border:1px solid #3A3E45;color:#C9CDD4;border-radius:6px;padding:4px 10px;cursor:pointer;}
  .scene-btn.active {border-color:#F26B21;color:#F26B21;}
  .factory-inspector {position:absolute;right:12px;top:12px;width:320px;background:#22262A;border:1px solid #3A3E45;border-radius:12px;padding:14px;color:#C9CDD4;}
  `;
  document.head.appendChild(s);
}

function renderFactory() {
  const canvas = document.getElementById('factoryCanvas'); if (!canvas) return;
  const ctx = canvas.getContext('2d'); canvas.width = canvas.offsetWidth; canvas.height = canvas.offsetHeight;
  const grid = FACTORY.gridSize; ctx.fillStyle = FACTORY.floorColor; ctx.fillRect(0,0,canvas.width,canvas.height);
  ctx.fillStyle = FACTORY.gridDotColor; for(let x=0;x<canvas.width;x+=grid)for(let y=0;y<canvas.height;y+=grid)ctx.fillRect(x,y,1,1);
  ctx.strokeStyle = FACTORY.lineColor; ctx.lineWidth=3; FACTORY.pipeRegistry.forEach(p=>{ctx.beginPath();ctx.moveTo(p.from.x*grid*2,p.from.y*grid*2);ctx.lineTo(p.to.x*grid*2,p.to.y*grid*2);ctx.stroke();});
  FACTORY.machineRegistry.forEach(m=>{const x=m.x*grid*2,y=m.y*grid*2,w=m.w*grid*2,h=m.h*grid*2;
    ctx.fillStyle='#22262A'; ctx.strokeStyle=m===FACTORY.selectedMachine?FACTORY.accent:FACTORY.lineColor; ctx.lineWidth=m===FACTORY.selectedMachine?2:1;
    ctx.fillRect(x,y,w,h); ctx.strokeRect(x,y,w,h);
    for(let i=0;i<4;i++){ctx.fillStyle=i<3?FACTORY.accent:'#333';ctx.fillRect(x+4,y+8+i*8,8,6);}
    ctx.fillStyle=FACTORY.textColor; ctx.font='10px monospace'; ctx.fillText(m.name,x+12,y+h-8);
  });
}

if (typeof module !== 'undefined') module.exports = {initFactory};
