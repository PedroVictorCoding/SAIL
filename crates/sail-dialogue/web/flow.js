/* Flow v1: a local text-DAG editor. Server receipts are the only execution state. */
'use strict';
(() => {
  const card=document.getElementById('flowCard');
  if(!card)return;
  let workflows=[],runs=[],draft=null,selected=null,dirty=false,busy=false,pending=null;
  const kinds=['trigger','trim','uppercase','prefix','contains','output'];
  const labels={
    trigger:'Manual trigger',trim:'Trim text',uppercase:'Uppercase',prefix:'Add prefix',contains:'Contains condition',output:'Result'
  };
  const $=s=>card.querySelector(s);
  function el(tag,attrs={},...children){
    const n=document.createElement(tag);
    for(const [k,v] of Object.entries(attrs)){
      if(k.startsWith('on'))n.addEventListener(k.slice(2),v);
      else if(k==='class')n.className=v;
      else if(v!==false&&v!=null)n.setAttribute(k,v);
    }
    for(const c of children.flat())if(c!=null)n.append(c instanceof Node?c:document.createTextNode(String(c)));
    return n;
  }

  function status(text,error=false){
    $('#flowStatus').textContent=text;
    $('#flowStatus').classList.toggle('flow-error',error);
  }

  async function api(path,body){
    const token=await getOrFetchOwnerToken();
    if(!token)throw Error('Owner authorization unavailable. Reconnect and try again.');
    const r=await fetch('/api/flow'+path,{
      method:body?'POST':'GET',cache:'no-store',headers:{
        Authorization:`Bearer ${token}`,...(body?{
          'Content-Type':'application/json'
        }:{})
      },...(body?{
        body:JSON.stringify(body)
      }:{})
    });
    const data=await r.json();
    if(!r.ok)throw Object.assign(Error(data.error||`Flow unavailable (${r.status})`),{
      status:r.status
    });
    return data;
  }  const copy=x=>JSON.parse(JSON.stringify(x));
  function fresh(){
    return {
      version:1,id:crypto.randomUUID(),revision:0,name:'Text workflow',nodes:[{
        id:'start',source:null,branch:'always',operation:{
          kind:'trigger'
        }
      },{
        id:'trim',source:'start',branch:'always',operation:{
          kind:'trim'
        }
      },{
        id:'result',source:'trim',branch:'always',operation:{
          kind:'output'
        }
      }]
    };
  }

  function choose(w){
    if(dirty&&!confirm('Discard unsaved workflow changes?'))return;
    draft=copy(w);
    selected=draft.nodes[0]?.id;
    dirty=false;
    render();
  }

  async function load(){
    try{
      const data=await api('/workflows');
      workflows=data.workflows;
      runs=data.runs;
      if(!draft){
        draft=copy(workflows[0]||fresh());
        selected=draft.nodes[0].id;
      }
      const resolved=pending&&runs.some(r=>r.id===pending.id);
      if(resolved){
        pending=null;
        $('#flowRun').textContent='Run saved workflow';
      }
      renderList();
      renderRuns();
      if(!$('#flowGraph').children.length||resolved)render();
      status(workflows.length?`${workflows.length} saved workflow${workflows.length===1?'':'s'}`:'No saved workflows. Edit this draft, then save and run.');
    }catch(e){
      status(e.message,true);
    }
  }

  function renderList(){
    const list=$('#flowSaved');
    list.replaceChildren(...workflows.map(w=>el('button',{
      type:'button',disabled:busy||!!pending,class:'tm-btn','aria-pressed':String(draft?.id===w.id),onclick:()=>choose(w)
    },w.name,el('small',{},`v${w.revision}`))));
    if(!workflows.length)list.append(el('p',{},'Your saved workflows will appear here.'));
  }

  function changed(){
    dirty=true;
    $('#flowRun').disabled=true;
    $('#flowSave').disabled=false;
    $('#flowDraftState').textContent='Unsaved changes';
  }

  function field(label,control){
    return el('label',{},el('span',{},label),control);
  }

  function render(){
    if(!draft)return;
    renderList();
    $('#flowName').value=draft.name;
    $('#flowDraftState').textContent=dirty?'Unsaved changes':draft.revision?`Saved version ${draft.revision}`:'Draft';
    $('#flowRun').disabled=busy||dirty||!draft.revision;
    $('#flowSave').disabled=busy||!!pending;
    $('#flowDelete').disabled=busy||!!pending||!draft.revision;
    $('#flowInput').disabled=busy||!!pending;
    for(const id of ['flowName','flowAdd','flowNew','flowRefresh'])$('#'+id).disabled=busy||(!!pending&&id!=='flowRefresh');
    const graph=$('#flowGraph');
    graph.replaceChildren();
    const depths=new Map();
    function depth(n,seen=new Set()){
      if(depths.has(n.id))return depths.get(n.id);
      if(seen.has(n.id))return 0;
      seen.add(n.id);
      const src=draft.nodes.find(s=>s.id===n.source);
      const d=src?Math.min(32,depth(src,seen)+1):0;
      depths.set(n.id,d);
      return d;
    }   for(const n of draft.nodes)depth(n);
    for(const d of [...new Set(depths.values())].sort((a,b)=>a-b)){
      const col=el('div',{
        class:'flow-column'
      });
      for(const n of draft.nodes.filter(n=>depths.get(n.id)===d))col.append(el('button',{
        type:'button',disabled:busy||!!pending,class:'flow-node','data-node':n.id,'aria-pressed':String(selected===n.id),onclick:()=>{
          selected=n.id;
          render();
        }
      },el('small',{},n.id),el('strong',{},labels[n.operation.kind]),el('span',{},n.source ? `From ${n.source} · ${n.branch}` : 'Manual input')));
      graph.append(col);
    }   const n=draft.nodes.find(n=>n.id===selected);
    const form=$('#flowNodeSettings');
    form.replaceChildren();
    if(n){
      form.append(el('h4',{},`Node: ${n.id}`));
      const kind=el('select',{
        disabled:busy||!!pending,'aria-label':'Operation',onchange:e=>{
          n.operation={
            kind:e.target.value,...(['prefix','contains'].includes(e.target.value)?{
              text:''
            }:{})
          };
          if(e.target.value==='trigger'){
            n.source=null;
            n.branch='always';
          }changed();
          render();
        }
      });
      for(const k of kinds)kind.append(el('option',{
        value:k,selected:n.operation.kind===k
      },labels[k]));
      form.append(field('Operation',kind));
      if(n.operation.kind!=='trigger'){
        const source=el('select',{
          disabled:busy||!!pending,'aria-label':'Input node',onchange:e=>{
            n.source=e.target.value||null;
            n.branch='always';
            changed();
            render();
          }
        });
        source.append(el('option',{
          value:''
        },'Choose an input'));
        for(const s of draft.nodes.filter(s=>s.id!==n.id&&s.operation.kind!=='output'))source.append(el('option',{
          value:s.id,selected:n.source===s.id
        },s.id));
        form.append(field('Input node',source));
        const branch=el('select',{
          disabled:busy||!!pending,'aria-label':'Run on branch',onchange:e=>{
            n.branch=e.target.value;
            changed();
          }
        });
        const src=draft.nodes.find(s=>s.id===n.source);
        for(const b of src?.operation.kind==='contains'?['always','true','false']:['always'])branch.append(el('option',{
          value:b,selected:n.branch===b
        },b));
        form.append(field('Run on branch',branch));
      }    if(['prefix','contains'].includes(n.operation.kind)){
        const text=el('input',{
          type:'text',disabled:busy||!!pending,maxlength:'1024','aria-label':'Node text',oninput:e=>{
            n.operation.text=e.target.value;
            changed();
          }
        });
        text.value=n.operation.text;
        form.append(field(n.operation.kind==='prefix'?'Prefix text':'Text to find (case sensitive)',text));
      }    form.append(el('button',{
        type:'button',disabled:busy||!!pending,class:'tm-btn',onclick:()=>{
          draft.nodes=draft.nodes.filter(v=>v.id!==n.id);
          selected=draft.nodes[0]?.id;
          changed();
          render();
        }
      },'Remove node'));
    }   requestAnimationFrame(drawEdges);
  }

  function drawEdges(){
    const svg=$('#flowEdges'),graph=$('#flowGraph');
    svg.replaceChildren();
    const base=graph.getBoundingClientRect();
    svg.setAttribute('width',graph.scrollWidth);
    svg.setAttribute('height',graph.scrollHeight);
    for(const n of draft?.nodes||[]){
      const a=[...graph.querySelectorAll('[data-node]')].find(x=>x.dataset.node===n.source),b=[...graph.querySelectorAll('[data-node]')].find(x=>x.dataset.node===n.id);
      if(!a||!b)continue;
      const ar=a.getBoundingClientRect(),br=b.getBoundingClientRect();
      const path=document.createElementNS('http://www.w3.org/2000/svg','path');
      const x=ar.right-base.left,y=ar.top+ar.height/2-base.top,u=br.left-base.left,v=br.top+br.height/2-base.top;
      path.setAttribute('d',`M${x},${y} C${x+22},${y} ${u-22},${v} ${u},${v}`);
      svg.append(path);
    }
  }

  function renderRuns(){
    const list=$('#flowRuns');
    const opened=new Set([...list.querySelectorAll('details[open]')].map(n=>n.dataset.run));
    list.replaceChildren();
    for(const r of [...runs].reverse()){
      const details=el('details',{
        'data-run':r.id,open:opened.has(r.id)||r.state==='running'
      },el('summary',{},`${r.workflow.name} · v${r.workflow.revision} · ${r.state} · ${new Date(r.created_at).toLocaleString()}`));
      details.append(el('small',{},`Run ${r.id}`));
      if(r.error)details.append(el('p',{
        class:'flow-error'
      },r.error));
      for(const p of r.receipts)details.append(el('div',{
        class:'flow-receipt'
      },el('strong',{},`${p.node_id}: ${p.skipped?'Skipped':p.decision!=null?String(p.decision):'Completed'}`),el('pre',{},p.output),el('small',{},new Date(p.completed_at).toLocaleString())));
      if(['running','paused'].includes(r.state)){
        for(const action of r.state==='paused'?['resume','cancel']:['cancel'])details.append(el('button',{
          class:'tm-btn',type:'button',onclick:async e=>{
            e.target.disabled=true;
            try{
              await api(`/runs/${r.id}/${action}`,{});
              await load();
            }catch(err){
              status(err.message,true);
              e.target.disabled=false;
            }
          }
        },action==='resume'?'Resume interrupted run':'Cancel run'));
      }else details.append(el('button',{
        class:'tm-btn',type:'button',onclick:async()=>{
          if(!confirm('Delete this execution and its saved results?'))return;
          try{
            await api(`/runs/${r.id}/delete`,{});
            await load();
          }catch(e){
            status(e.message,true);
          }
        }
      },'Delete execution'));
      list.append(details);
    }
    if(!runs.length)list.append(el('p',{},'No executions yet. Results and skipped branches appear here after a run.'));
  }  $('#flowDelete').onclick=async()=>{
    if(!confirm('Delete all saved versions of this workflow? Existing execution receipts are kept until you delete them separately.'))return;
    busy=true;
    render();
    try{
      await api(`/workflows/${draft.id}/delete`,{});
      draft=null;
      dirty=false;
      await load();
    }catch(e){
      status(e.message,true);
    }finally{
      busy=false;
      render();
    }
  };
  $('#flowNew').onclick=()=>choose(fresh());
  $('#flowRefresh').onclick=load;
  $('#flowName').oninput=e=>{
    draft.name=e.target.value;
    changed();
  };
  $('#flowAdd').onclick=()=>{
    if(draft.nodes.length>=32){
      status('Workflows support up to 32 nodes.',true);
      return;
    }
    let i=1;
    while(draft.nodes.some(n=>n.id===`node_${i}`))i++;
    const src=draft.nodes.find(n=>n.id===selected&&n.operation.kind!=='output')||draft.nodes.find(n=>n.operation.kind!=='output');
    const n={
      id:`node_${i}`,source:src?.id||null,branch:'always',operation:{
        kind:'trim'
      }
    };
    draft.nodes.push(n);
    selected=n.id;
    changed();
    render();
  };
  $('#flowSave').onclick=async()=>{
    busy=true;
    render();
    try{
      const saved=await api('/workflows',{
        expected_revision:draft.revision,workflow:{
          ...draft,revision:draft.revision+1
        }
      });
      draft=saved.workflow;
      dirty=false;
      await load();
      status(`Saved version ${draft.revision}`);
    }catch(e){
      status(e.message,true);
    }finally{
      busy=false;
      render();
    }
  };
  $('#flowRun').onclick=async()=>{
    busy=true;
    render();
    try{
      if(!pending)pending={
        id:crypto.randomUUID(),workflow_id:draft.id,revision:draft.revision,input:$('#flowInput').value
      };
      await api('/runs',pending);
      pending=null;
      await load();
      status('Run accepted. Results below come from saved server receipts.');
    }catch(e){
      if(e.status>=400&&e.status<500)pending=null;
      status(`${e.message}${pending?' Retry uses the same run ID.':''}`,true);
    }finally{
      busy=false;
      $('#flowRun').textContent=pending?'Retry run request':'Run saved workflow';
      render();
    }
  };
  // Flow is the Workflows section of SAIL Agents: it is shown when its card is on the page.
  const shown=()=>!!document.getElementById('subsystemContent')?.contains(card);
  for(const event of ['sail:page','sail:section'])window.addEventListener(event,()=>{
    if(shown())load();
  });
  new ResizeObserver(()=>requestAnimationFrame(drawEdges)).observe($('#flowGraph'));
  setInterval(()=>{
    if(shown()&&!busy&&runs.some(r=>r.state==='running'))load();
  },1500);
  if(shown())load();
})();
