'use strict';
const $ = id => document.getElementById(id);
const esc = value => String(value).replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
const format = (value, digits=0) => Number(value).toLocaleString('en-US', {maximumFractionDigits:digits, minimumFractionDigits:digits});
const time = ms => `${String(Math.floor(ms/60000)).padStart(2,'0')}:${(ms/1000%60).toFixed(1).padStart(4,'0')}`;
const phaseName = p => ({closed:'Closed',open:'Open',half_open:'Probe',bypassed:'Bypassed'}[p] || p);
const outcomeName = p => ({success:'Success',error:'Error',timeout:'Timed out',shed:'Circuit-shed'}[p] || 'Pending');
const positions = [130,300,470];
// The same cubic curve as the SVG roads keeps traffic and status dots on the track.
const circuitPosition = .54;
function routePoint(service,t) {
 const u=1-t,y=positions[service];
 return {x:330*u*u*u+3*480*u*u*t+3*490*u*t*t+695*t*t*t,
         y:300*u*u*u+3*300*u*u*t+3*y*u*t*t+y*t*t*t};
}
const serviceDescriptions = ['The catalog service looks up products and availability.','The payment service processes transactions.','The search service finds and ranks matching products.'];
let state = null, selected = 1, busy = false, connected = false, seenId = -1, lastTime = 0, revision = 0, lastSignature = '';
let particles = [], lastFrame = 0, polling = false;
const reducedMotion = matchMedia('(prefers-reduced-motion: reduce)').matches;

function nodeArt() {
 return `<svg class="node-art" viewBox="0 0 150 115" aria-hidden="true"><ellipse cx="76" cy="100" rx="50" ry="10" fill="#42633b" opacity=".07"/><path class="node-platform" d="M6 79 L70 45 L143 84 L78 119 Z" fill="#e1e8d6"/><path d="M30 29 L72 6 L117 31 L74 56 Z" fill="#c6d4b6"/><path d="M30 29 L74 56 V97 L30 71 Z" fill="#94ab82"/><path d="M74 56 L117 31 V74 L74 97 Z" fill="#6f8b61"/><path d="M39 48 L64 62 V69 L39 55 Z M39 63 L64 77 V84 L39 70 Z" fill="#e0eacb"/><path d="M85 63 L107 50 V57 L85 70 Z M85 77 L107 64 V70 L85 84 Z" fill="#a8c58e"/><path d="M53 29 L72 19 L93 31 L74 41 Z" fill="#e9eedc"/><circle class="server-led" cx="104" cy="44" r="2" fill="#dceb99"/></svg>`;
}
function buildNodes() {
 $('nodes').innerHTML = state.services.map((s,i) => `<button class="node" data-service="${i}" aria-label="Inspect ${esc(s.definition.name)}" aria-pressed="${i===selected}">${nodeArt()}<div class="fault-bubbles"></div><div class="node-queue" aria-hidden="true"></div><div class="node-name"><i class="selection-dot"></i>${esc(s.definition.name)}<span class="phase closed">Closed</span></div><div class="node-meta"></div><div class="node-workers"></div></button>`).join('');
 $('circuit-signals').innerHTML=state.services.map((s,i)=>{
  const point=routePoint(i,circuitPosition);
  return `<button class="circuit-signal closed" data-circuit="${i}" style="left:${point.x/9}%;top:${point.y/6}%"><span class="signal-dot" aria-hidden="true"></span><span class="signal-label">Closed</span></button>`;
 }).join('');
 $('pressure-halos').innerHTML = positions.map((y,i)=>`<ellipse id="halo-${i}" cx="694" cy="${y}" rx="108" ry="65" fill="#e3b278" opacity="0"/>`).join('');
}
const choiceName=c=>({open:'Open circuit',probe:'Permit probe',no_change:'No change'}[c]||'Evaluation error');
function phaseReason(t) {
 if(state.policy==='jev' && t.trigger==='Jev recommendation accepted by Reflex')return 'Jev recommended this transition; Reflex checked legality, freshness, and resource limits.';
 if(t.to==='open') return t.from==='half_open' ? 'The recovery probe failed. The circuit reopened for a new cooldown.' : 'The recent error and timeout ratio met the opening threshold.';
 if(t.to==='half_open') return 'The cooldown elapsed. One recovery probe was reserved; other arrivals remain blocked.';
 if(t.to==='closed') return 'The recovery probe succeeded. Normal admission resumed.';
 return t.trigger;
}
function render() {
 if(!state) return;
 renderForecast(state.forecasts?.[selected], state.services[selected].definition.name, state.policy, busy || !connected || state.replaying);
 $('scenario').value=state.scenario;$('scenario').disabled=busy||!connected;
 $('scenario-description').textContent=state.scenario_description;
 $('run-scenario').disabled=busy||!connected||state.scenario==='sandbox';
 const ended=state.at_ms>=state.horizon_ms;
 const datadog=state.inference.evidence_source==='datadog';
 $('policy-select').value=state.policy;$('policy-select').disabled=datadog||busy||!connected;
 $('policy-select').querySelector('[value="jev"]').disabled=!state.inference.available;
 $('policy-select').title=state.inference.available?'Changing policy starts a fresh incident':'Set TYPESAFE_API_KEY on the server to enable Jev';
 $('policy-label').textContent=state.policy==='jev'?`${state.inference.model} · ${state.replaying?'recorded replay':(datadog?'Datadog evidence':'live inference')}`:'Threshold policy · no AI inference';
 $('inference-strip').hidden=state.policy!=='jev';
 const pending=state.inference.pending;
 $('inference-status').textContent=state.replaying?'Replaying recorded judgments · no API calls':pending?`${pending.response_ready?'Response ready':'Evaluating'} · ${state.services[pending.service].definition.name}${state.paused?' · resumes with the clock':''}`:state.inference.budget_exhausted?'Call budget exhausted · no new judgments':state.paused?'Jev ready · start or step traffic':'Jev observing client responses';
 if(datadog) $('inference-status').textContent=`Datadog · ${pending?'Querying / evaluating':state.inference.telemetry_status}${state.inference.evidence_age_seconds==null?'':` · data ${format(state.inference.evidence_age_seconds)}s old`}${state.paused?' · paused':''}`;
 document.querySelector('.duration').textContent=`/ ${String(Math.floor(state.horizon_ms/60000)).padStart(2,'0')}:00`;
 $('inference-budget').textContent=`${state.inference.calls} / ${state.inference.limit} live evaluations`;
 renderCost();
 $('judgment-panel').hidden=state.policy!=='jev';
 if(!$('nodes').children.length) buildNodes();
 renderCircuits();
 $('clock').textContent=time(state.at_ms);
 $('seed').textContent=`SEED ${state.seed}`;
 $('play').textContent=ended?'Session complete':state.paused?(state.at_ms===0?'▶ Start traffic':'▶ Resume traffic'):'Ⅱ Pause traffic';
 $('play').disabled=ended || busy || !connected;
 $('step').disabled=datadog || ended || busy || !connected;
 $('run-status').textContent=ended?'COMPLETE':state.replaying?'REPLAY':state.paused?(state.at_ms===0?'READY':'PAUSED'):'RUNNING';
 $('replay').disabled=datadog || state.at_ms===0 || busy || !connected;
 $('repair').disabled=state.replaying || ended || busy || !connected;
 $('reset').disabled=busy || !connected;
 document.querySelectorAll('[data-speed]').forEach(b=>{b.setAttribute('aria-pressed',Number(b.dataset.speed)===state.speed);b.disabled=(datadog&&Number(b.dataset.speed)!==1)||busy||!connected;});
 const counts=state.counts;
 $('success-count').textContent=format(counts.success);
 $('failed-count').textContent=format(counts.error+counts.timeout);
 $('shed-count').textContent=format(counts.shed);
 $('pending-count').textContent=format(counts.offered-counts.success-counts.error-counts.timeout-counts.shed);
 $('offered-rate').textContent=`${format(state.services.reduce((n,s)=>n+s.state.offered_rate,0))} requests / sec`;
 state.services.forEach((s,i)=>{
   const node=document.querySelector(`[data-service="${i}"]`), phase=s.state.phase;
   node.classList.toggle('selected',i===selected);node.setAttribute('aria-pressed',i===selected);
   const pill=node.querySelector('.phase');pill.textContent=phaseName(phase);pill.className=`phase ${phase}`;
   node.querySelector('.node-queue').innerHTML=Array.from({length:Math.min(24,s.state.queued)},()=>'<i></i>').join('');
   node.querySelector('.node-queue').title=`${s.state.queued} queued requests (up to 24 blocks shown)`;
   node.querySelector('.node-meta').textContent=`${s.state.active}/${s.definition.workers} busy · ${s.state.queued} queued`;
   node.querySelector('.node-workers').innerHTML=Array.from({length:s.definition.workers},(_,j)=>`<i class="${j<s.state.active?'busy':''}"></i>`).join('');
   node.querySelector('.fault-bubbles').innerHTML=[s.faults.slow?'6×':null,s.faults.errors?'ϟ':null,s.faults.surge?'↗':null].filter(Boolean).map(t=>`<span>${t}</span>`).join('');
   $('halo-'+i).setAttribute('opacity',Math.min(.26,s.state.stress*.26));
 });
 const service=state.services[selected], s=service.state;
 $('selected-index').textContent=`0${selected+1} / 03`;
 $('service-name').textContent=service.definition.name;
 $('service-phase').textContent=phaseName(s.phase);$('service-phase').className=`phase ${s.phase}`;
 $('service-description').textContent=serviceDescriptions[selected];
 $('pressure-value').textContent=`${format(s.stress*100)}%`;
 $('pressure-fill').style.width=`${s.stress*100}%`;
 $('pressure-fill').style.background=s.stress>.65?'#c28b62':s.stress>.3?'#c6ac6d':'#8fac6c';
 $('workers').textContent=`${s.active} / ${service.definition.workers}`;
 $('queued').textContent=format(s.queued);
 $('zombie-work').className=`zombie-note ${service.timed_out_work?'active':''}`;
 $('zombie-work').textContent=service.timed_out_work?`${service.timed_out_work} timed-out requests are still consuming downstream capacity.`:'No timed-out work downstream.';
 document.querySelectorAll('[data-fault]').forEach(b=>{b.setAttribute('aria-pressed',service.faults[b.dataset.fault]);b.disabled=state.replaying||ended||busy||!connected;});
 const t=state.transitions.find(t=>t.service===selected);
 $('decision-title').textContent=t?`${phaseName(t.from)} → ${phaseName(t.to)}`:'Watching the traffic';
 $('decision-body').textContent=t?`${time(t.at_ms)} · ${phaseReason(t)} Reflex accepted the guarded transition.`:'A 5-second response window controls this circuit. Reflex validates every transition before applying it.';
 renderJudgments(); renderEvents(); renderTrend(); renderRequests();
 $('connection').textContent=connected?'● ENGINE CONNECTED':'ENGINE DISCONNECTED';
}
function renderCost() {
 const c=state.inference.cost;
 if(!c){$('cost-total').textContent='Unavailable';$('cost-summary').textContent='Restart the server to enable cost tracking';return;}
 const excluded=c.missing_usage_calls+c.unpriced_calls;
 $('cost-total').textContent=c.priced_calls===0&&excluded?'—':`$${format(c.estimated_usd,6)}`;
 $('cost-summary').textContent=`${format(c.input_tokens)} input tokens${excluded?' · partial estimate':''}`;
 $('cost-usage').textContent=`${format(c.calls)} live evaluations since server start · ${format(c.input_tokens)} input tokens · ${format(c.output_tokens)} output tokens reported.`;
 $('cost-coverage').textContent=`${format(c.priced_calls)} priced responses. ${format(c.missing_usage_calls)} calls without reported usage; ${format(c.unpriced_calls)} responses without a known model rate.`;
}
function renderCircuits() {
 state.services.forEach((service,i)=>{
  const phase=service.state.phase,signal=document.querySelector(`[data-circuit="${i}"]`);
  const behavior={closed:'Traffic flows normally',open:'New requests are blocked',half_open:'One recovery request only'}[phase];
  signal.className=`circuit-signal ${phase}`;
  signal.querySelector('.signal-label').textContent=phaseName(phase);
  signal.setAttribute('aria-label',`Inspect ${service.definition.name} circuit: ${phaseName(phase)}. ${behavior}`);
  signal.setAttribute('aria-pressed',i===selected);
  signal.title=`${service.definition.name}: ${phaseName(phase)} — ${behavior}`;
 });
}
function renderJudgments() {
 const jev=state.policy==='jev';$('judgment-details').hidden=!jev;
 if(!jev)return;
 const latest=state.decisions.find(d=>d.service===selected);
 if(latest){
  const r=latest.inference,score=r.action?r.probabilities[r.action]:null;
  $('decision-title').textContent=`${choiceName(r.action)} · ${latest.guard.status}`;
  $('decision-body').textContent=`${time(latest.completed_at_ms)} · ${r.error ? r.error.code + ': ' + r.error.message + '. ' : ''}${latest.guard.reason}`;
  $('judgment-details').innerHTML=`<span>Choice score <b>${score==null?'Not supplied':format(score*100,1)+'%'}</b></span><span>Confidence <b>${r.confidence==null?'Not supplied':format(r.confidence*100,1)+'%'}</b></span><span>Provider latency <b>${format(r.wall_latency_ms)} ms</b></span><button data-judgment="${latest.id}">Inspect evidence ↗</button>`;
 }else{
  $('decision-title').textContent='Waiting for a Jev judgment';
  $('decision-body').textContent='Traffic continues while Jev evaluates recent client responses. Reflex guards the resulting recommendation.';
  $('judgment-details').innerHTML='<span>No model scores have been received.</span>';
 }
 $('judgments').innerHTML=state.decisions.length?state.decisions.slice(0,10).map(d=>{
  const r=d.inference,score=r.action?r.probabilities[r.action]:null;
  return `<tr><td>${time(d.completed_at_ms)}</td><td>${esc(state.services[d.service].definition.name)}</td><td>${esc(choiceName(r.action))}</td><td>${score==null?'—':format(score*100,1)+'%'}</td><td>${format(r.wall_latency_ms)} ms</td><td class="guard-${d.guard.status}">${esc(d.guard.status)}</td><td><button data-judgment="${d.id}" aria-label="Inspect judgment ${d.id}">Inspect ↗</button></td></tr>`;
 }).join(''):'<tr><td colspan="7"><div class="empty">No judgments yet. Start traffic to gather evidence.</div></td></tr>';
}
function inspectJudgment(id){
 const d=state.decisions.find(d=>d.id===id);if(!d)return;
 const r=d.inference;
 $('judgment-title').textContent=`${state.services[d.service].definition.name} · ${choiceName(r.action)}`;
 $('judgment-detail').innerHTML=`<p><strong>${esc(d.guard.status)}</strong> · ${esc(d.guard.reason)}</p><p>Observed ${time(d.evidence.observed_at_ms)} · applied/checked ${time(d.completed_at_ms)} · provider ${format(r.wall_latency_ms)} ms</p><h3>${r.error?.code==='datadog_evidence'?'Telemetry unavailable · Jev was not called':'Evidence sent to Jev'}</h3><pre>${esc(JSON.stringify(d.model_input||d.evidence,null,2))}</pre><h3>Provider result</h3><pre>${esc(JSON.stringify(r,null,2))}</pre>`;
 $('judgment-dialog').showModal();
}
function renderEvents() {
 const modelEvents=state.decisions.map(d=>({at:d.completed_at_ms,kind:d.inference.error?'fault':'transition',title:`${state.services[d.service].definition.name}: Jev ${choiceName(d.inference.action)}`,detail:`${d.guard.status} · ${d.guard.reason}`}));
 const events=[...modelEvents,...state.transitions.map(t=>({at:t.at_ms,kind:'transition',title:`${state.services[t.service].definition.name}: ${phaseName(t.from)} → ${phaseName(t.to)}`,detail:phaseReason(t)})),...state.injections.map(t=>({at:t.at_ms,kind:'fault',title:`${state.services[t.service].definition.name}: ${Object.values(t.faults).some(Boolean)?'faults updated':'faults repaired'}`,detail:[t.faults.slow?'6× slowdown':null,t.faults.errors?'85% injected errors':null,t.faults.surge?'4× traffic':null].filter(Boolean).join(' · ')||'Normal conditions restored. Existing work continues.'}))].sort((a,b)=>b.at-a.at).slice(0,16);
 $('event-count').textContent=`${events.length}${events.length===16?'+':''} RECENT`;
 $('event-log').innerHTML=events.length?events.map(e=>`<div class="event ${e.kind}"><time>${time(e.at)}</time><i class="event-dot"></i><div><strong>${esc(e.title)}</strong><small>${esc(e.detail)}</small></div></div>`).join(''):'<div class="empty">A quiet system.<br>Your first fault starts the story.</div>';
}
function renderTrend() {
 const values=state.history.filter(v=>v.service===selected), left=Math.max(0,state.at_ms-30000),right=Math.max(30000,state.at_ms),max=Math.max(5,...values.map(v=>v.queued));
 const x=at=>34+(at-left)/(right-left)*576,y=(n,limit)=>132-n/limit*114;
 let svg='';
 for(let i=0;i<=4;i++){const yy=132-i/4*114;svg+=`<line x1="34" x2="610" y1="${yy}" y2="${yy}" stroke="#e7eddf" stroke-dasharray="3 4"/><text x="25" y="${yy+3}" text-anchor="end">${format(max*i/4)}</text><text x="620" y="${yy+3}">${i*25}%</text>`;}
 for(let i=0;i<=3;i++){const at=left+(right-left)*i/3;svg+=`<text x="${x(at)}" y="154" text-anchor="middle">${format(at/1000)}s</text>`;}
 for(const [field,limit,color] of [['queued',max,'#76996a'],['stress',1,'#c8a165']]){const d=values.map((v,i)=>`${i?'L':'M'}${x(v.at_ms).toFixed(2)},${y(v[field],limit).toFixed(2)}`).join(' ');svg+=`<path d="${d}" fill="none" stroke="${color}" stroke-width="2" stroke-linejoin="round"/>`;}
 $('trend').innerHTML=svg;$('trend-label').textContent=`${state.services[selected].definition.name} · last 30 simulated seconds`;
}
function serverStatus(r) {return r.outcome==='shed'?'Never admitted':r.downstream_finished_ms!=null?'Finished':r.started_ms!=null?'Still running':'Still queued';}
function renderRequests() {
 const rows=state.requests.filter(r=>r.service===selected).slice(0,8);
 $('requests').innerHTML=rows.length?rows.map(r=>`<tr><td>#${r.id}${r.probe?' · probe':''}</td><td>${(r.arrived_ms/1000).toFixed(3)}s</td><td class="outcome-${r.outcome||'pending'}">${outcomeName(r.outcome)}</td><td>${serverStatus(r)}</td><td>${format(r.work_ms)} ms</td><td><button data-request="${r.id}" aria-label="Inspect request ${r.id}">Inspect ↗</button></td></tr>`).join(''):'<tr><td colspan="6"><div class="empty">Start traffic to follow the first request.</div></td></tr>';
}
function inspectRequest(id) {
 const r=state.requests.find(r=>r.id===id);if(!r)return;
 $('request-title').textContent=`Request #${r.id}`;
 const pairs=[['Service',state.services[r.service].definition.name],['Arrived',`${(r.arrived_ms/1000).toFixed(3)}s`],['Client outcome',outcomeName(r.outcome)],['Client latency',`${format((r.client_finished_ms??state.at_ms)-r.arrived_ms)} ms`],['Downstream',serverStatus(r)],['Worker time',`${format(r.work_ms)} ms`],['Work after timeout',`${format(r.post_timeout_work_ms)} ms`],['Recovery probe',r.probe?'Yes':'No']];
 $('request-detail').innerHTML=`<dl>${pairs.map(([key,value])=>`<dt>${esc(key)}</dt><dd>${esc(value)}</dd>`).join('')}</dl><p>${esc(r.cause||'The client is still waiting for a response.')}</p>`;
 $('request-dialog').showModal();
}
function ingest(next) {
 const signature=JSON.stringify(next);
 if(signature===lastSignature && connected)return;
 lastSignature=signature;
 if(next.at_ms<lastTime || (next.at_ms===0 && lastTime===0)){particles=[];seenId=-1;}
 if(!reducedMotion)for(const r of next.requests.slice().reverse())if(r.id>seenId && r.id%3===0)particles.push({id:r.id,service:r.service,age:0,outcome:r.outcome});
 seenId=Math.max(seenId,...next.requests.map(r=>r.id));lastTime=next.at_ms;
 const latest=new Map(next.requests.map(r=>[r.id,r]));for(const p of particles){if(latest.has(p.id))p.outcome=latest.get(p.id).outcome;}
 state=next;connected=true;showError(next.error);render();
}
function showError(message){$('error-banner').hidden=!message;$('error-banner').textContent=message||'';}
async function send(command) {
 if(busy || !connected)return false;
 busy=true;revision++;render();
 try {
  const response=await fetch('/api/command',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(command),signal:AbortSignal.timeout(10000)});
  const data=await response.json();if(!response.ok)throw new Error(data.error||'The command was rejected.');
  ingest(data);return true;
 }catch(error){showError(error.message);return false;}
 finally{busy=false;render();}
}
async function poll() {
 if(!busy && !polling){polling=true;const version=revision;try{const response=await fetch('/api/state',{signal:AbortSignal.timeout(5000)});if(!response.ok)throw new Error('The simulation server is unavailable.');const data=await response.json();if(!busy && version===revision)ingest(data);}catch(error){connected=false;showError('Engine disconnected. Keep the playground CLI running, then this page will reconnect.');render();}finally{polling=false;}}
 setTimeout(poll,200);
}
function animate(now) {
 const dt=Math.min(100,now-lastFrame||0);lastFrame=now;
 if(state&&!state.paused&&connected){for(const p of particles)p.age+=dt*state.speed;}
 particles=particles.filter(p=>p.age<1200).slice(-130);
 $('particles').innerHTML=particles.map(p=>{
  const t=Math.min(1,p.age/900);
  let x,y;
  if(t<.35){x=130+200*t/.35;y=300;}else{const u=(t-.35)/.65,point=routePoint(p.service,u*(p.outcome==='shed'?circuitPosition:1));x=point.x;y=point.y;}
  const color=p.outcome==='shed'?'#c1a273':p.outcome==='timeout'||p.outcome==='error'?'#c78b6a':'#7c9c64';
  return `<circle cx="${x.toFixed(1)}" cy="${y.toFixed(1)}" r="${p.outcome==='shed'?3:3.3}" fill="${color}" opacity="${p.age>900?1-(p.age-900)/300:.8}"/>`;
 }).join('');requestAnimationFrame(animate);
}
$('policy-select').addEventListener('change',e=>send({type:'policy',policy:e.target.value}));
for(const id of ['judgments','judgment-details'])$(id).addEventListener('click',e=>{const b=e.target.closest('[data-judgment]');if(b)inspectJudgment(Number(b.dataset.judgment));});
$('play').addEventListener('click',()=>{$('intro').hidden=true;send({type:state.paused?'play':'pause'});});
$('step').addEventListener('click',()=>send({type:'step'}));
$('reset').addEventListener('click',()=>send({type:'reset'}));
$('repair').addEventListener('click',()=>send({type:'repair'}));
$('replay').addEventListener('click',()=>send({type:'replay'}));
document.querySelectorAll('[data-speed]').forEach(b=>b.addEventListener('click',()=>send({type:'speed',value:Number(b.dataset.speed)})));
$('nodes').addEventListener('click',e=>{const b=e.target.closest('[data-service]');if(b){selected=Number(b.dataset.service);render();}});
document.querySelectorAll('[data-fault]').forEach(b=>b.addEventListener('click',()=>{const faults={...state.services[selected].faults};faults[b.dataset.fault]=!faults[b.dataset.fault];$('intro').hidden=true;send({type:'fault',service:selected,faults});}));
$('requests').addEventListener('click',e=>{const b=e.target.closest('[data-request]');if(b)inspectRequest(Number(b.dataset.request));});
$('dismiss-intro').addEventListener('click',()=>$('intro').hidden=true);
for(const id of ['about','policy-info'])$(id).addEventListener('click',()=>$('about-dialog').showModal());
document.querySelectorAll('.close-dialog').forEach(b=>b.addEventListener('click',()=>b.closest('dialog').close()));
document.querySelectorAll('.transport button,.session-actions button,[data-fault]').forEach(b=>b.disabled=true);
poll();requestAnimationFrame(animate);

$('circuit-signals').addEventListener('click',e=>{const b=e.target.closest('[data-circuit]');if(b){selected=Number(b.dataset.circuit);render();}});

$('scheduler-tab').addEventListener('click',async e=>{e.preventDefault();if(busy)return;await send({type:'pause'});if(state?.paused)location.href='/scheduler';});

$('recovery-tab').addEventListener('click',async e=>{e.preventDefault();if(busy)return;await send({type:'pause'});if(state?.paused)location.href='/recovery';});

$('forecast-panel').addEventListener('change', e => { if(e.target.id === 'forecast-toggle') send({type:'forecast',enabled:e.target.checked}); });

$('scenario').addEventListener('change',e=>send({type:'scenario',scenario:e.target.value}));
$('run-scenario').addEventListener('click',async()=>{const scenario=state.scenario;if(await send({type:'scenario',scenario}))await send({type:'play'});});
