'use strict';
const $=id=>document.getElementById(id),esc=v=>String(v).replace(/[&<>"']/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
const num=(v,d=0)=>Number(v).toLocaleString('en-US',{minimumFractionDigits:d,maximumFractionDigits:d});
const time=ms=>`${String(Math.floor(ms/60000)).padStart(2,'0')}:${(ms/1000%60).toFixed(1).padStart(4,'0')}`;
const colors=['#7f9d61','#b7865a','#79a4a7','#aa84aa','#bf927b','#738fbd','#aaa055','#849585'];
const color=id=>colors[id%colors.length];
const choice=c=>c?.place?`Node ${String.fromCharCode(65+c.place.node)}`:({node_a:'Node A',node_b:'Node B',node_c:'Node C',node_d:'Node D',defer:'Defer'}[c]||'No recommendation');
const priorityName=p=>({normal:'Normal',high:'High',critical:'Critical'}[p]||'Normal');
const jobPriority=j=>state.client_priorities[j.client]||'normal';
const policyName=p=>({jev:'Jev',first_fit:'First Fit',best_fit:'Best Fit'}[p]);
let state=null,busy=false,connected=false,polling=false,revision=0,signature='',clientIds='',selected={kind:'client',id:0},mapIds='',particles=[],seenJobs=null,lastFrame=0;
const reducedMotion=matchMedia('(prefers-reduced-motion: reduce)').matches;
const nodeYs=[100,250,400,550];
const sourceY=(i,n)=>n===1?330:100+i*450/(n-1);
function showError(message){$('error').hidden=!message;$('error').textContent=message||'';}
function clients(force=false){
 if(selected.kind==='client'&&!state.clients.some(c=>c.id===selected.id))selected={kind:'client',id:state.clients[0].id};
 const ids=state.clients.map(c=>c.id).join(',')+':'+selected.id;
 if(ids!==clientIds||force){clientIds=ids;$('clients').innerHTML=state.clients.filter(c=>c.id===selected.id).map(c=>`<div class="client ${c.config.enabled?'':'disabled'}" data-client="${c.id}" style="--client:${color(c.id)}"><div class="client-head"><i class="client-dot"></i><strong>${esc(c.name)}</strong><button class="remove" data-remove="${c.id}" aria-label="Remove ${esc(c.name)}" title="Existing jobs keep running">×</button></div><label class="priority-control">Live priority<select aria-label="${esc(c.name)} priority" data-priority="${c.id}">${['normal','high','critical'].map(p=>`<option value="${p}" ${c.priority===p?'selected':''}>${priorityName(p)}</option>`).join('')}</select></label><p class="helper">Applies immediately to queued and future requests. Running jobs continue.</p><div class="client-stats" data-stats="${c.id}"></div><form data-form="${c.id}"><div class="client-grid"><label>Jobs / sec<input aria-label="${esc(c.name)} jobs per second" name="rate" type="number" min="0" step="1" value="${c.config.rate}" required></label><label>CPU / job<input aria-label="${esc(c.name)} CPU per job" name="cpu" type="number" min="1" max="32" step="1" value="${c.config.cpu}" required></label><label>GiB / job<input aria-label="${esc(c.name)} memory per job" name="memory_gib" type="number" min="1" max="64" step="1" value="${c.config.memory_gib}" required></label><label>Duration (s)<input aria-label="${esc(c.name)} duration in seconds" name="seconds" type="number" min="1" max="30" step="1" value="${c.config.duration_ms/1000}" required></label></div><div class="client-foot"><span class="client-summary">${c.config.enabled&&c.config.rate>0?'Sending '+num(c.config.rate)+' jobs/s':'Source paused'}</span><button type="submit">Apply changes</button></div></form></div>`).join('');}
 $('client-count').textContent=`${state.clients.length} / 8`;
 $('add-client').disabled=busy||!connected||state.clients.length>=8;
 document.querySelectorAll('.client button').forEach(b=>b.disabled=busy||!connected||(b.hasAttribute('data-remove')&&state.clients.length===1));
 document.querySelectorAll('[data-priority]').forEach(el=>{const c=state.clients.find(c=>c.id===Number(el.dataset.priority));el.value=c.priority;el.disabled=busy||!connected;});
 document.querySelectorAll('[data-stats]').forEach(el=>{const s=state.client_stats.find(s=>s.client===Number(el.dataset.stats));el.innerHTML=s?`<span><b>${s.queued}</b> queued</span><span><b>${s.completed}</b> completed · ${num(s.completed_per_second,2)}/s</span><span><b>${s.mean_wait_ms==null?'—':num(s.mean_wait_ms/1000,1)+'s'}</b> mean wait</span><span><b>${s.p95_wait_ms==null?'—':num(s.p95_wait_ms/1000,1)+'s'}</b> p95 wait</span><small>Waits: ${s.started} started jobs · oldest queued ${num(s.oldest_wait_ms/1000,1)}s · completion rate since run start</small>`:'';});
 document.querySelectorAll('.client[data-client]').forEach(el=>{const c=state.clients.find(c=>c.id===Number(el.dataset.client));if(!c)return;el.classList.toggle('disabled',!c.config.enabled);el.querySelector('.client-summary').textContent=c.config.enabled&&c.config.rate>0?'Sending '+num(c.config.rate)+' jobs/s':'Source paused';});
 // Do not replace editable forms on polling; unsubmitted user input stays intact.
}
function render(){
 $('connection').textContent=connected?'● Engine connected':'Engine disconnected';
 $('connection').dataset.connected=String(connected);

 if(!state)return;

 $('scenario').value=state.scenario;$('scenario').disabled=busy||!connected;
 $('scenario-description').textContent=state.scenario_description;
 $('run-scenario').disabled=busy||!connected||state.scenario==='sandbox';
 const ended=state.at_ms>=state.horizon_ms;
 $('evidence-source').hidden=state.evidence_source!=='datadog';$('evidence-source').textContent=`Evidence: Datadog · Run: ${state.simulation_run||'—'}`;
 $('clock').textContent=time(state.at_ms);if($('duration'))$('duration').textContent=`/ ${String(Math.floor(state.horizon_ms/60000)).padStart(2,'0')}:00`;$('run-status').textContent=ended?'COMPLETE':state.paused?'PAUSED':'RUNNING';
 $('play').textContent=ended?'Session complete':state.paused?(state.at_ms?'▶ Resume traffic':'▶ Start traffic'):'Ⅱ Pause traffic';
 $('play').disabled=busy||!connected||ended;$('step').disabled=busy||!connected||ended||state.evidence_source==='datadog';$('reset').disabled=busy||!connected;
 $('policy').value=state.policy;$('policy').disabled=busy||!connected||state.evidence_source==='datadog';$('policy').querySelector('[value="jev"]').disabled=!state.available;
 $('policy').title='Changing policy resets jobs and keeps client settings';
 document.querySelectorAll('[data-speed]').forEach(b=>{b.setAttribute('aria-pressed',Number(b.dataset.speed)===state.speed);b.disabled=busy||!connected||(state.evidence_source==='datadog'&&Number(b.dataset.speed)!==1);});
 $('status').textContent=state.paused&&state.pending_job?'Paused · pending judgment applies on resume':state.status;if(state.evidence_source==='datadog')$('status').textContent+=' · '+state.telemetry_status;$('calls').textContent=state.policy==='jev'?`${state.calls} evaluations`:'Deterministic policy · no inference';
 for(const key of ['completed','running','queued','rejected'])$(key).textContent=num(state[key]);
 $('wait').textContent=num(state.mean_wait_ms/1000,1)+'s';
 const c=state.cost,partial=c.missing_usage_calls+c.unpriced_calls;
 $('cost').textContent=c.priced_calls===0&&partial?'—':`$${num(c.estimated_usd,6)}${partial?'*':''}`;
 $('cost-details').textContent=`${num(c.calls)} evaluations · ${num(c.input_tokens)} input tokens · ${num(c.output_tokens)} output tokens. ${c.priced_calls} priced responses; ${c.missing_usage_calls} calls without usage; ${c.unpriced_calls} responses with an unknown rate. ${partial?'The displayed estimate is partial.':''}`;
 clients();renderTopology();renderQueue();renderNodes();renderInspector();renderDecisions();renderTrend();renderClientLag();

}
function renderQueue(){
 const queue=state.jobs.filter(j=>j.phase==='queued');$('queue-count').textContent=`${state.queued} waiting`;
 $('queue-state').textContent=state.queued?`Oldest waiting ${num(state.oldest_wait_ms/1000,1)}s · capacity for 128 jobs`:'New arrivals wait here until a node is selected.';
 $('queue').innerHTML=queue.length?queue.slice(0,8).map((j,i)=>`<div class="job ${state.eligible_jobs.includes(j.id)?'candidate':''}" style="--client:${color(j.client)}"><div class="job-head"><strong>#${j.id}</strong><span>${state.pending_job===j.id?(state.paused?'Pending result':'Jev evaluating…'):state.eligible_jobs.includes(j.id)?(state.at_ms-j.arrived_at>=state.aging_ms?'AGING · NEXT':'CLIENT HEAD'):'QUEUED'}</span></div><small><span class="priority-badge ${jobPriority(j)}">${priorityName(jobPriority(j))}</span> Client ${j.client+1} · waiting ${num((state.at_ms-j.arrived_at)/1000,1)}s</small><span class="job-size">${j.cpu} CPU · ${j.memory_gib} GiB</span><small>~${num(j.duration_ms/1000)}s to run</small></div>`).join(''):'<div class="empty">Room to breathe.<br>No jobs waiting.</div>';
 $('queue-more').textContent=queue.length>8?`+ ${queue.length-8} more jobs waiting`:'';
}
function renderNodes(){
 const jobs=selected.kind==='node'?state.jobs.filter(j=>j.phase==='running'&&j.node===selected.id):[];
 $('node-details').innerHTML=jobs.length?`<div class="running-jobs">${jobs.map(j=>`<div class="running-job" style="--client:${color(j.client)}" title="Client ${j.client+1} · ${j.cpu} CPU · ${j.memory_gib} GiB"><strong>#${j.id}</strong><small>${num((state.at_ms-j.started_at)/1000,1)}s / ~${num(j.duration_ms/1000)}s</small></div>`).join('')}</div>`:'';
}
function serverArt(){return `<svg class="server-art" viewBox="0 0 150 120" aria-hidden="true"><ellipse cx="77" cy="103" rx="56" ry="10" fill="#42633b" opacity=".06"/><path class="platform" d="M6 83 L70 48 L144 87 L78 120 Z" fill="#dbe5cf"/><path d="M31 29 L72 6 L118 32 L75 57 Z" fill="#c6d7b1"/><path d="M31 29 L75 57 V99 L31 73 Z" fill="#94ad7d"/><path d="M75 57 L118 32 V75 L75 99 Z" fill="#708d60"/><path d="M40 48 L64 62 V69 L40 55 Z M40 64 L64 78 V85 L40 71 Z" fill="#e3edcf"/><path d="M85 63 L107 51 V58 L85 71 Z M85 78 L107 66 V72 L85 85 Z" fill="#b1c896"/><circle cx="105" cy="44" r="2" fill="#e5efa5"/></svg>`;}
function clientArt(){return `<svg class="source-art" viewBox="0 0 120 90" aria-hidden="true"><path d="M6 58 L54 33 L112 64 L63 89 Z" fill="#dce5d3"/><path d="M24 20 L62 7 L96 27 L58 44 Z" fill="#cdddbd"/><path d="M24 20 L58 40 V67 L24 47 Z" fill="#91aa7d"/><path d="M58 40 L96 21 V49 L58 67 Z" fill="#718f62"/><path d="M31 29 L50 40 V51 L31 40 Z" fill="var(--client)"/><circle cx="82" cy="43" r="2" fill="#e8f2d6"/></svg>`;}
function renderTopology(){
 const ids=state.clients.map(c=>c.id).join(',');
 if(ids!==mapIds){mapIds=ids;
  $('map-clients').innerHTML=state.clients.map((c,i)=>`<button class="map-source" data-select-client="${c.id}" style="left:14%;top:${sourceY(i,state.clients.length)/6.6}%;--client:${color(c.id)}" aria-label="Configure ${esc(c.name)}">${clientArt()}<strong>${esc(c.name)}</strong><span class="source-rate"></span><span class="priority-badge source-priority"></span></button>`).join('');
  $('flow-paths').innerHTML=[...state.clients.map((c,i)=>`M140 ${sourceY(i,state.clients.length)} C240 ${sourceY(i,state.clients.length)} 260 330 320 330`),'M320 330 H500',...nodeYs.map(y=>`M500 330 C635 330 655 ${y} 815 ${y}`)].map(d=>`<path d="${d}" class="track-shadow"/><path d="${d}" class="track"/><path d="${d}" class="track-center"/>`).join('');
 }
 $('topology').classList.toggle('many-clients',state.clients.length>5);
 if(!$('map-nodes').children.length)$('map-nodes').innerHTML=state.nodes.map((n,i)=>`<button class="map-node" data-select-node="${i}" style="left:81.5%;top:${nodeYs[i]/6.6}%" aria-label="Inspect ${esc(n.name)}"><span class="node-pile" aria-hidden="true"></span>${serverArt()}<strong>${esc(n.name)}</strong><span class="map-node-count"></span><span class="mini-resource"><span>CPU</span><i><b class="cpu-fill"></b></i><small class="cpu-text"></small></span><span class="mini-resource"><span>MEM</span><i><b class="mem-fill"></b></i><small class="mem-text"></small></span></button>`).join('');
 state.clients.forEach(c=>{const b=document.querySelector(`#map-clients [data-select-client="${c.id}"]`);b.querySelector('.source-rate').textContent=c.config.enabled&&c.config.rate>0?`${num(c.config.rate)} jobs/s · ${c.config.cpu} CPU`:'Paused';b.querySelector('.source-priority').textContent=priorityName(c.priority);b.querySelector('.source-priority').className='priority-badge source-priority '+c.priority;b.classList.toggle('source-paused',!c.config.enabled||!c.config.rate);});
 document.querySelectorAll('[data-select-client]').forEach(b=>b.setAttribute('aria-pressed',selected.kind==='client'&&selected.id===Number(b.dataset.selectClient)));
 state.nodes.forEach((n,i)=>{const b=document.querySelector(`[data-select-node="${i}"]`),jobs=state.jobs.filter(j=>j.phase==='running'&&j.node===i);b.setAttribute('aria-pressed',selected.kind==='node'&&selected.id===i);b.querySelector('.map-node-count').textContent=`${jobs.length} running`;b.querySelector('.cpu-fill').style.width=`${n.used_cpu/n.cpu*100}%`;b.querySelector('.mem-fill').style.width=`${n.used_memory_gib/n.memory_gib*100}%`;b.querySelector('.cpu-text').textContent=`${n.used_cpu}/${n.cpu}`;b.querySelector('.mem-text').textContent=`${n.used_memory_gib}/${n.memory_gib}`;b.querySelector('.node-pile').innerHTML=jobs.slice(0,16).map(j=>`<i style="background:${color(j.client)}"></i>`).join('');});
 $('queue-stack').innerHTML=state.jobs.filter(j=>j.phase==='queued').slice(0,24).map(j=>`<i style="background:${color(j.client)}"></i>`).join('')||'<span class="empty-queue">···</span>';
 $('queue-hub').setAttribute('aria-pressed',selected.kind==='queue');
 $('scheduler-name').textContent=policyName(state.policy).toUpperCase()+' + REFLEX';
 $('scheduler-status').textContent=state.pending_job?`${state.paused?'Pending':'Judging'} client heads`:state.queued?'Waiting to place':'Ready for work';
 $('scheduler-light').classList.toggle('thinking',state.pending_job!=null);
}
function renderInspector(){
 $('clients').hidden=selected.kind!=='client';$('node-details').hidden=selected.kind!=='node';$('queue-details').hidden=selected.kind!=='queue';
 const c=state.clients.find(c=>c.id===selected.id);
 $('inspector-title').textContent=selected.kind==='client'?c.name:selected.kind==='node'?state.nodes[selected.id].name:'Shared queue';
 $('inspector-help').textContent=selected.kind==='client'?'Change priority live for queued and future jobs; workload edits affect future arrivals.':selected.kind==='node'?'Live reservations and running jobs. Completed work releases its capacity.':'FIFO within each client. Jev chooses among fitting client heads; after 30s the oldest fitting head takes precedence. Cards are shown in arrival order.';
}
function collectParticles(next){
 const previous=seenJobs;
 seenJobs=new Map(next.jobs.map(j=>[j.id,{phase:j.phase,started_at:j.started_at}]));
 if(!previous||!state||next.at_ms<state.at_ms||next.at_ms===0){particles=[];return;}
 if(reducedMotion)return;
 for(const j of next.jobs){
  const old=previous.get(j.id),i=next.clients.findIndex(c=>c.id===j.client);
  if(!old&&j.arrived_at>=state.at_ms&&i>=0)particles.push({kind:'arrival',client:j.client,cpu:j.cpu,y:sourceY(i,next.clients.length),rejected:j.phase==='rejected',age:0});
  if(j.started_at!=null&&j.started_at>=state.at_ms&&(!old||old.started_at==null))particles.push({kind:'placement',client:j.client,cpu:j.cpu,node:j.node,age:0});
 }
 particles=particles.slice(-90);
}
function bezier(a,b,c,d,t){const u=1-t;return a*u*u*u+3*b*u*u*t+3*c*u*t*t+d*t*t*t;}
function animate(now){
 const delta=Math.min(100,now-lastFrame||0);lastFrame=now;
 if(state&&!state.paused&&connected)particles.forEach(p=>p.age+=delta*state.speed);
 particles=particles.filter(p=>p.age<1100);
 $('flow-particles').innerHTML=particles.map(p=>{const t=Math.min(1,p.age/850);let x,y;
  if(p.kind==='arrival'){x=bezier(140,240,260,320,t);y=bezier(p.y,p.y,330,330,t);}else{x=bezier(500,635,655,815,t);y=bezier(330,330,nodeYs[p.node],nodeYs[p.node],t);}
  return `<circle cx="${x}" cy="${y}" r="${3+Math.min(6,Math.sqrt(p.cpu)*1.4)}" fill="${p.rejected?'#c2634a':color(p.client)}" stroke="#fffef8" stroke-width="1" opacity="${p.age>850?1-(p.age-850)/250:.9}"/>`;
 }).join('');requestAnimationFrame(animate);
}
function renderDecisions(){
 const latest=state.decisions[0];$('inspect-latest').hidden=!latest;
 const picked=latest&&state.jobs.find(j=>j.id===latest.job),bypassed=picked&&state.jobs.some(j=>j.phase==='queued'&&j.arrived_at<picked.arrived_at);
 $('decision-title').textContent=latest?`Job #${latest.job} → ${choice(latest.choice)}`:'Waiting for work';
 $('decision-body').textContent=latest?`${time(latest.at_ms)} · ${latest.status}. ${latest.result?.error||latest.reason}${latest.status==='placed'&&bypassed?' · Selected ahead of older requests from another client.':''}`:'Start the clients to generate the first jobs.';
 $('decisions').innerHTML=state.decisions.length?state.decisions.map(d=>`<tr><td>${time(d.at_ms)}</td><td>#${d.job}</td><td>${policyName(d.policy)}</td><td>${choice(d.choice)}</td><td class="${d.status==='rejected'||d.status==='evaluation_error'?'error':''}">${esc(d.status)}</td><td>${d.result?.confidence==null?'—':num(d.result.confidence*100,1)+'%'}</td><td><button data-inspect="${d.job}:${d.at_ms}" aria-label="Inspect job ${d.job} decision">Inspect ↗</button></td></tr>`).join(''):'<tr><td colspan="7" class="empty">No placement decisions yet.</td></tr>';
}
function inspect(d){if(!d)return;$('inspection').textContent=JSON.stringify(d,null,2);$('inspect-dialog').showModal();}
function renderTrend(){
 const points=state.history,left=Math.max(0,state.at_ms-60000),right=Math.max(60000,state.at_ms),max=Math.max(5,...points.flatMap(p=>[p.running,p.queued]));
 const x=t=>35+570*(t-left)/(right-left),y=n=>140-120*n/max;
 let svg='';for(let i=0;i<=4;i++){const yy=y(max*i/4);svg+=`<line x1="35" x2="610" y1="${yy}" y2="${yy}" stroke="#e2e9d8" stroke-dasharray="3 4"/><text x="27" y="${yy+3}" text-anchor="end">${num(max*i/4)}</text>`;}
 for(let i=0;i<=3;i++){const at=left+(right-left)*i/3;svg+=`<text x="${x(at)}" y="163" text-anchor="middle">${num(at/1000)}s</text>`;}
 for(const [key,color] of [['running','#769662'],['queued','#81a5ab']]){const d=points.map((p,i)=>`${i?'L':'M'}${x(p.at_ms)},${y(p[key])}`).join(' ');svg+=`<path d="${d}" fill="none" stroke="${color}" stroke-width="2"/>`;}
 $('trend').innerHTML=svg;
}
const hiddenLagClients=new Set();
let lagLegendSignature='';
function renderClientLag(){
 const live=state.live_lag||[], ids=[...new Set([...state.history.flatMap(p=>(p.clients||[]).map(c=>c.client)),...live.map(c=>c.client)])].sort((a,b)=>a-b);
 const left=Math.max(0,state.at_ms-60000),right=Math.max(60000,state.at_ms);
 const samples=[...state.history.filter(p=>p.at_ms>=left&&p.at_ms<state.at_ms),{at_ms:state.at_ms,clients:live}];
 const signature=ids.map(id=>`${id}:${state.client_priorities[id]||'normal'}:${hiddenLagClients.has(id)}`).join('|');
 if(signature!==lagLegendSignature){lagLegendSignature=signature;$('lag-legend').innerHTML=ids.map(id=>`<button data-lag-client="${id}" aria-pressed="${!hiddenLagClients.has(id)}" style="--client:${color(id)}"><i></i>Client ${id+1} <span class="priority-badge ${state.client_priorities[id]||'normal'}">${priorityName(state.client_priorities[id])}</span></button>`).join('');}
 const changes=(state.priority_changes||[]).filter(e=>!hiddenLagClients.has(e.client)&&e.at_ms>=left);
 $('lag-events').textContent=changes.length?changes.map(e=>`${time(e.at_ms)} · Client ${e.client+1} → ${priorityName(e.priority)}`).join('  |  '):'Change a client’s live priority to mark it on this timeline.';
 $('lag-values').innerHTML=live.filter(c=>!hiddenLagClients.has(c.client)).map(c=>`<tr><td><i style="background:${color(c.client)}"></i>Client ${c.client+1}</td><td>${priorityName(c.priority)}</td><td>${c.queued}</td><td>${num(c.oldest_wait_ms/1000,1)}s</td><td>${c.recent_mean_start_lag_ms==null?'—':num(c.recent_mean_start_lag_ms/1000,1)+'s'}</td><td>${c.recent_starts}</td></tr>`).join('');
}
function ingest(next){const sig=JSON.stringify(next);if(sig===signature&&connected)return;signature=sig;collectParticles(next);state=next;connected=true;showError(next.error);render();}
async function send(cmd,refreshClients=false){
 if(busy||!connected)return false;busy=true;revision++;render();
 try{const response=await fetch('/api/scheduler/command',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(cmd),signal:AbortSignal.timeout(10000)});const data=await response.json();if(!response.ok)throw new Error(data.error||'Command failed');ingest(data);if(refreshClients)clients(true);return true;}catch(e){showError(e.message);return false;}finally{busy=false;render();}
}
async function poll(){if(!busy&&!polling){polling=true;const version=revision;try{const r=await fetch('/api/scheduler/state',{signal:AbortSignal.timeout(5000)});if(!r.ok)throw new Error();const next=await r.json();if(!busy&&version===revision)ingest(next);}catch(e){connected=false;showError('Engine disconnected. Keep the playground CLI running.');render();}finally{polling=false;}}setTimeout(poll,200);}
$('play').addEventListener('click',()=>send({type:state.paused?'play':'pause'}));$('step').addEventListener('click',()=>send({type:'step'}));$('reset').addEventListener('click',()=>send({type:'reset'},true));$('policy').addEventListener('change',e=>send({type:'policy',policy:e.target.value},true));
document.querySelectorAll('[data-speed]').forEach(b=>b.addEventListener('click',()=>send({type:'speed',value:Number(b.dataset.speed)})));
$('add-client').addEventListener('click',()=>send({type:'add_client'},true));
$('clients').addEventListener('submit',e=>{e.preventDefault();const f=e.target,id=Number(f.dataset.form),c=state.clients.find(c=>c.id===id);const data=new FormData(f);send({type:'client',id,config:{rate:Number(data.get('rate')),cpu:Number(data.get('cpu')),memory_gib:Number(data.get('memory_gib')),duration_ms:Number(data.get('seconds'))*1000,enabled:c.config.enabled}},true);});
$('clients').addEventListener('click',e=>{const remove=e.target.closest('[data-remove]');if(remove)send({type:'remove_client',id:Number(remove.dataset.remove)},true);});
$('inspect-latest').addEventListener('click',()=>inspect(state.decisions[0]));$('decisions').addEventListener('click',e=>{const b=e.target.closest('[data-inspect]');if(b)inspect(state.decisions.find(d=>`${d.job}:${d.at_ms}`===b.dataset.inspect));});
$('cost-info').addEventListener('click',()=>$('cost-dialog').showModal());document.querySelectorAll('.close').forEach(b=>b.addEventListener('click',()=>b.closest('dialog').close()));
document.querySelectorAll('a[href="/"],a[href="/recovery"]').forEach(a=>a.addEventListener('click',async e=>{e.preventDefault();if(await send({type:'pause'}))location.href=a.getAttribute('href');}));
document.querySelectorAll('#map-clients').forEach(el=>el.addEventListener('click',e=>{const b=e.target.closest('[data-select-client]');if(b){selected={kind:'client',id:Number(b.dataset.selectClient)};clients(true);render();}}));
$('map-nodes').addEventListener('click',e=>{const b=e.target.closest('[data-select-node]');if(b){selected={kind:'node',id:Number(b.dataset.selectNode)};render();}});
$('queue-hub').addEventListener('click',()=>{selected={kind:'queue',id:0};render();});
const renderLegacyScenario=render;
render=function(){renderLegacyScenario();if(state)window.renderScenarioUI?.({state,busy,connected:connected,selected:selected,send,hiddenLagClients:[...hiddenLagClients],onSelect:selection=>{selected=selection;clients(true); render();}});};
poll();requestAnimationFrame(animate);


$('scenario').addEventListener('change',e=>send({type:'scenario',scenario:e.target.value}));
$('run-scenario').addEventListener('click',async()=>{const scenario=state.scenario;if(await send({type:'scenario',scenario}))await send({type:'play'});});

$('clients').addEventListener('change', e=>{if(e.target.matches('[data-priority]'))send({type:'priority',id:Number(e.target.dataset.priority),priority:e.target.value});});

$('lag-legend').addEventListener('click',e=>{const b=e.target.closest('[data-lag-client]');if(!b)return;const id=Number(b.dataset.lagClient);hiddenLagClients.has(id)?hiddenLagClients.delete(id):hiddenLagClients.add(id);render();});
