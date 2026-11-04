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
  // Check if styles already injected
  if (document.getElementById('factory-styles')) return;
  const s = document.createElement('style');
  s.id = 'factory-styles';
  s.textContent = `
  .factory-mode {
    background: #17191D !important;
  }
  .factory-mode .workspace,
  .factory-mode aside {
    display: none !important;
  }
  .factory-mode .side-rail {
    background: #17191D;
  }
  .factory-floor {
    position: absolute;
    inset: 96px 0 0 138px;
    background: #17191D;
    overflow: hidden;
    z-index: 100;
  }
  .factory-canvas {
    width: 100%;
    height: 100%;
    display: block;
    cursor: pointer;
  }
  .factory-top-bar {
    position: absolute;
    top: 12px;
    left: 50%;
    transform: translateX(-50%);
    display: flex;
    gap: 24px;
    color: #C9CDD4;
    font: 11px monospace;
  }
  .factory-bottom-bar {
    position: absolute;
    bottom: 12px;
    left: 50%;
    transform: translateX(-50%);
    display: flex;
    align-items: center;
    gap: 12px;
    color: #C9CDD4;
    font: 11px monospace;
  }
  .scene-nav, .scene-btn {
    background: #22262A;
    border: 1px solid #3A3E45;
    color: #C9CDD4;
    border-radius: 6px;
    padding: 4px 10px;
    cursor: pointer;
  }
  .scene-btn.active {
    border-color: #F26B21;
    color: #F26B21;
  }
  .factory-inspector {
    position: absolute;
    right: 12px;
    top: 12px;
    width: 320px;
    background: #22262A;
    border: 1px solid #3A3E45;
    border-radius: 12px;
    padding: 14px;
    color: #C9CDD4;
  }
  .factory-live i {
    display: inline-block;
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: #5FD3A6;
    margin-right: 6px;
  }
  `;
  document.head.appendChild(s);
}

// Exit factory mode and restore normal UI
window.sailExitFactory = () => {
  const shell = document.getElementById('appShell');
  if (shell) shell.classList.remove('factory-mode');
  const floor = document.getElementById('factoryFloor');
  if (floor) floor.remove();
  document.querySelectorAll('.nav-item').forEach(el => el.classList.remove('selected'));
  document.getElementById('homeNav')?.classList.add('selected');
  location.hash = '#home';
  return false;
};

// Wire up home nav to exit factory mode
document.addEventListener('DOMContentLoaded', () => {
  const homeNav = document.getElementById('homeNav');
  if (homeNav) {
    homeNav.addEventListener('click', (e) => {
      if (document.getElementById('factoryFloor')) {
        e.preventDefault();
        window.sailExitFactory();
      }
    });
  }
});

function renderFactory() {
  const canvas = document.getElementById('factoryCanvas'); if (!canvas) return;
  const ctx = canvas.getContext('2d'); canvas.width = canvas.offsetWidth; canvas.height = canvas.offsetHeight;
  const grid = FACTORY.gridSize * 2; ctx.fillStyle = FACTORY.floorColor; ctx.fillRect(0,0,canvas.width,canvas.height);
  // Draw dotted grid
  ctx.fillStyle = FACTORY.gridDotColor; for(let x=0;x<canvas.width;x+=FACTORY.gridSize)for(let y=0;y<canvas.height;y+=FACTORY.gridSize)ctx.fillRect(x,y,1,1);
  // Draw pipe network
  ctx.strokeStyle = FACTORY.lineColor; ctx.lineWidth=3; ctx.lineCap='round';
  FACTORY.pipeRegistry.forEach(p=>{ctx.beginPath();ctx.moveTo(p.from.x*grid,p.from.y*grid);ctx.lineTo(p.to.x*grid,p.to.y*grid);ctx.stroke();});
  // Draw 3D isometric machines
  FACTORY.machineRegistry.forEach(m=>{
    const x=m.x*grid, y=m.y*grid, w=m.w*grid, h=m.h*grid;
    const selected = m === FACTORY.selectedMachine;
    // Machine base
    ctx.fillStyle='#22262A'; ctx.fillRect(x,y,w,h);
    // Selected orange outline
    ctx.strokeStyle = selected ? FACTORY.accent : FACTORY.lineColor;
    ctx.lineWidth = selected ? 2 : 1; ctx.strokeRect(x,y,w,h);
    // Vertical status indicator bars
    for(let i=0;i<4;i++){
      ctx.fillStyle = i<3 ? FACTORY.accent : '#333';
      ctx.fillRect(x + 6, y + 10 + i*9, 10, 7);
    }
    // Machine label uppercase condensed
    ctx.fillStyle = FACTORY.textColor; ctx.font = '11px "Consolas", monospace'; ctx.textAlign='left';
    ctx.fillText(m.name, x + 22, y + h - 10);
    // Floating anchored spec card when selected
    if (selected) {
      ctx.fillStyle = 'rgba(45, 48, 52, 0.95)';
      ctx.fillRect(x + w + 12, y, 220, 80);
      ctx.strokeStyle = FACTORY.accent; ctx.lineWidth = 1;
      ctx.strokeRect(x + w + 12, y, 220, 80);
      ctx.fillStyle = FACTORY.accent; ctx.font = 'bold 12px monospace';
      ctx.fillText(`MACHINE #${FACTORY.machineRegistry.indexOf(m)+1}`, x + w + 20, y + 22);
      ctx.fillStyle = FACTORY.textColor; ctx.font = '11px monospace';
      ctx.fillText(`${m.metric}: 84%`, x + w + 20, y + 45);
      ctx.fillText(`Temp: ${42 + Math.floor(Math.random()*80)}°C`, x + w + 20, y + 62);
    }
  });
}

// Canvas click/hover handler
function initCanvasEvents() {
  const canvas = document.getElementById('factoryCanvas'); if (!canvas) return;
  const grid = FACTORY.gridSize * 2;
  canvas.onclick = (e) => {
    const rect = canvas.getBoundingClientRect();
    const x = e.clientX - rect.left, y = e.clientY - rect.top;
    FACTORY.selectedMachine = null;
    for (const m of FACTORY.machineRegistry) {
      const mx = m.x*grid, my = m.y*grid, mw = m.w*grid, mh = m.h*grid;
      if (x >= mx && x <= mx+mw && y >= my && y <= my+mh) {
        FACTORY.selectedMachine = m; break;
      }
    }
    renderFactory();
  };
  // Re-render every 500ms for live status
  setInterval(renderFactory, 500);
}

// Add canvas event init to initFactory
const origInit = initFactory; initFactory = () => { origInit(); setTimeout(initCanvasEvents, 100); };

window.sailShowFactory = () => {
  closeSheet?.();
  // Deselect all nav items
  document.querySelectorAll('.nav-item').forEach(el => el.classList.remove('selected'));
  const navBtn = document.getElementById('factoryNav');
  if (navBtn) navBtn.classList.add('selected');
  initFactory();
  location.hash = '#factory';
  return false;
};

// Add factory to nav after DOM is ready
document.addEventListener('DOMContentLoaded', () => {
  // Add button to side rail
  const sideRail = document.querySelector('.side-rail');
  if (sideRail) {
    const navBtn = document.createElement('button');
    navBtn.id = 'factoryNav';
    navBtn.className = 'quiet-button nav-item';
    navBtn.type = 'button';
    navBtn.innerHTML = '<span class="nav-icon" aria-hidden="true"><svg><use href="/icons.svg#arena"></use></svg></span><span>Factory</span>';
    navBtn.onclick = window.sailShowFactory;
    // Insert right after Home nav
    const homeNav = document.getElementById('homeNav');
    if (homeNav && homeNav.nextSibling) {
      sideRail.insertBefore(navBtn, homeNav.nextSibling);
    } else {
      sideRail.insertBefore(navBtn, sideRail.firstChild);
    }
  }
  
  // Add to spaces list (once spaces is defined)
  if (typeof ui === 'undefined' && typeof spaces !== 'undefined') {
    const existing = spaces.find(s => s.id === 'factory');
    if (!existing) {
      spaces.unshift({
        id:'factory', name:'FACTORY FLOOR', subtitle:'Isometric room-as-index view', available:true,
        action:() => window.sailShowFactory()
      });
      if (typeof renderSpaces === 'function') renderSpaces();
    }
  }
  
  // Handle hash change for #factory
  window.addEventListener('hashchange', () => {
    if (location.hash === '#factory') {
      window.sailShowFactory();
    }
  });
});

// Resize listener for canvas
window.addEventListener('resize', () => {
  const canvas = document.getElementById('factoryCanvas');
  if (canvas && typeof renderFactory === 'function') {
    renderFactory();
  }
});

if (typeof module !== 'undefined') module.exports = {initFactory, FACTORY, sailShowFactory};
