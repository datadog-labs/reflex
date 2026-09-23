// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

'use strict';
const report=JSON.parse(document.getElementById('report-data').textContent);
const $=id=>document.getElementById(id);
const esc=value=>String(value).replace(/[&<>"']/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
const number=new Intl.NumberFormat('en-US');
const fmt=(v,d=0)=>v==null?'—':Number(v).toLocaleString('en-US',{maximumFractionDigits:d,minimumFractionDigits:d});
const clock=ms=>`${String(Math.floor(ms/60000)).padStart(2,'0')}:${String(Math.floor(ms/1000)%60).padStart(2,'0')}`;
const colors=['#91a3b0','#2563eb','#138a76','#a879c4'];
let selected=Math.max(0,report.scenarios.findIndex(s=>s.scenario.id==='slowdown'));
let scope=-1,metric='throughput',journal='transitions',cursor=32000,playing=false,lastFrame=0,lastPaint=0;
let seriesCache=[];
const current=()=>report.scenarios[selected];
const phaseLabel=p=>({closed:'Closed',open:'Open',half_open:'Half-open',bypassed:'Bypassed'}[p]||p);
const pill=p=>`<span class="state-pill ${esc(p)}">${esc(phaseLabel(p))}</span>`;
function indexAt(list,at){let lo=0,hi=list.length;while(lo<hi){const m=(lo+hi)>>1;if(list[m].at_ms<=at)lo=m+1;else hi=m;}return Math.max(0,lo-1);}
function activePhase(){const s=current().scenario;return s.phases.find(p=>cursor>=p.start_ms&&cursor<p.end_ms)||s.phases.at(-1);}
function phaseAt(run,sid,at){let p=run.snapshots.find(s=>s.service===sid)?.phase||'closed';for(const t of run.transitions){if(t.at_ms>at)break;if(t.service===sid)p=t.to;}return p;}
function focusService(){return scope>=0?scope:Math.max(0,current().scenario.services.findIndex(s=>current().scenario.phases.some(p=>p.changes.some(c=>c.service===s.id))));}
function buildSeries(run){
  const map=new Map();
  for(const s of run.snapshots){if(scope>=0&&s.service!==scope)continue;let v=map.get(s.at_ms);if(!v){v={at_ms:s.at_ms,queue:0,stress:0,success:0,bad:0};map.set(s.at_ms,v);}v.queue+=s.queued;v.stress=Math.max(v.stress,s.stress*100);v.success+=s.counts.success;v.bad+=s.counts.error+s.counts.timeout;}
  const values=[...map.values()];
  return values.map((v,i)=>{const j=indexAt(values,Math.max(0,v.at_ms-1000));const old=values[j];const seconds=(v.at_ms-old.at_ms)/1000;return{...v,throughput:seconds>0?(v.success-old.success)/seconds:0,failures:seconds>0?(v.bad-old.bad)/seconds:0};});
}
function nav(){
  $('scenario-count').textContent=String(report.scenarios.length).padStart(2,'0');$('seed').textContent=report.seed;
  $('scenario-nav').innerHTML=report.scenarios.map((s,i)=>`<button class="scenario-button ${i===selected?'selected':''}" data-scenario="${i}" aria-current="${i===selected?'page':'false'}"><span class="scenario-index">${String(i+1).padStart(2,'0')}</span><span><strong>${esc(s.scenario.name)}</strong><small>${s.scenario.duration_ms/1000}s · ${s.scenario.services.length} downstreams</small></span></button>`).join('');
}
function comparison(){
 const data=current(),service=scope>=0?data.scenario.services[scope].name:'All downstreams';
 const baseline=scope<0?data.runs[0].metrics:data.runs[0].service_metrics[scope];
 $('comparison').innerHTML=data.runs.map((run,i)=>{
  const m=scope<0?run.metrics:run.service_metrics[scope],c=m.counts,percent=c.offered?100*c.success/c.offered:0,delta=c.success-baseline.counts.success;
  return `<article class="result-card ${i?'accent':''}" style="--run-color:${colors[i%colors.length]}"><div class="result-name"><i class="swatch"></i>${esc(run.name)}<span class="result-tag">${esc(service.toUpperCase())}</span></div><div class="result-top"><div><div class="result-value">${fmt(percent,1)}<small>%</small></div><div class="result-sub">${number.format(c.success)} successful / ${number.format(c.offered)} offered</div></div>${i?`<span class="delta ${delta<0?'negative':''}">${delta>=0?'+':''}${number.format(delta)} useful requests</span>`:`<span class="result-tag">REFERENCE RUN</span>`}</div><div class="result-grid"><div><strong>${fmt(m.success_p95_ms,0)}<small> ms</small></strong><span>Successful latency · P95</span></div><div><strong>${number.format(c.error+c.timeout)}</strong><span>Errors + timeouts</span></div><div><strong>${number.format(c.shed)}</strong><span>Circuit-shed requests</span></div></div><div class="outcome-bar" aria-label="Outcome proportions: success, error, timeout, shed">${[[c.success,'#62a78d'],[c.error,'#d66f5d'],[c.timeout,'#e3a781'],[c.shed,'#d6bd83']].map(([n,color])=>`<span style="width:${c.offered?100*n/c.offered:0}%;background:${color}"></span>`).join('')}</div><div class="result-foot"><span>${fmt(m.wasted_work_ms/1000,1)} worker-s of unsuccessful work</span><span>${fmt(m.post_timeout_work_ms/1000,1)} worker-s after timeout</span></div></article>`;
 }).join('');
}
function scenario(){
 const d=current(),s=d.scenario;cursor=Math.min(cursor,s.duration_ms);playing=false;nav();
 $('scenario-title').textContent=s.name;$('scenario-description').textContent=s.description;
 $('offered').textContent=number.format(d.offered_requests);$('trace-id').textContent=`TRACE ${d.trace_fingerprint.slice(0,10)}`;
 $('duration-label').textContent=`${s.duration_ms/1000}s`;$('clock-end').textContent=clock(s.duration_ms);$('scrubber').max=s.duration_ms;
 $('phase-strip').innerHTML=s.phases.map((p,i)=>`<button class="phase-block ${p.changes.length?'fault':p.start_ms>=45000?'recovery':''}" data-phase="${i}" style="flex:${p.end_ms-p.start_ms}" title="${esc(p.name)} · ${p.start_ms/1000}–${p.end_ms/1000}s: ${esc(p.description)}">${esc(p.name)}</button>`).join('');
 $('service-tabs').innerHTML=[{name:'All downstreams'},...s.services].map((v,i)=>`<button data-service="${i-1}" class="${scope===i-1?'selected':''}">${esc(v.name)}</button>`).join('');
 $('model-config').textContent=`Client timeout: ${s.timeout_ms} ms · Stress build / recovery: ${s.stress_build_ms} / ${s.stress_recovery_ms} ms · Stress error factor: ${s.stress_error_probability}\nThreshold policy: ≥20 responses in 5 s, ≥50% failures, 3 s cooldown.\nTrace ${d.trace_fingerprint} · seed ${report.seed}`;
 $('model-config').style.whiteSpace='pre-line';
 seriesCache=d.runs.map(buildSeries);comparison();drawChart();stateTracks();update();
}
function drawChart(){
 const s=current().scenario,W=1000,L=42,R=12,T=12,B=215,inner=W-L-R;
 let max=Math.max(1,...seriesCache.flatMap(values=>values.map(v=>v[metric])));
 if(metric==='stress')max=100;else max=Math.ceil(max/5)*5;
 const y=v=>B-(B-T)*v/max,x=ms=>L+inner*ms/s.duration_ms;
 let svg='';
 for(const p of s.phases){if(p.changes.length)svg+=`<rect x="${x(p.start_ms)}" y="${T}" width="${x(p.end_ms)-x(p.start_ms)}" height="${B-T}" fill="#faeee7" opacity=".6"/>`;}
 for(let i=0;i<=4;i++){const v=max*i/4,yy=y(v);svg+=`<line x1="${L}" y1="${yy}" x2="${W-R}" y2="${yy}" stroke="#e9eef2" stroke-dasharray="3 4"/><text x="${L-10}" y="${yy+3}" text-anchor="end">${fmt(v,Number.isInteger(v)?0:1)}</text>`;}
 for(let ms=0;ms<=s.duration_ms;ms+=s.duration_ms/6){svg+=`<text x="${x(ms)}" y="238" text-anchor="middle">${fmt(ms/1000)}s</text>`;}
 seriesCache.forEach((values,i)=>{const path=values.map((v,j)=>`${j?'L':'M'}${x(v.at_ms).toFixed(2)},${y(v[metric]).toFixed(2)}`).join(' ');svg+=`<path d="${path}" fill="none" stroke="${colors[i%colors.length]}" stroke-width="${i?2.1:1.7}" stroke-linejoin="round" stroke-linecap="round"/>`;});
 svg+=`<line data-cursor x1="${x(cursor)}" x2="${x(cursor)}" y1="${T}" y2="${B}" stroke="#244f6c" stroke-width="1" stroke-dasharray="3 3"/><circle data-cursor-dot cx="${x(cursor)}" cy="${T}" r="3" fill="#244f6c"/>`;
 $('chart').innerHTML=svg;$('chart').setAttribute('aria-label',`${metric} over simulated time, comparing ${current().runs.map(r=>r.name).join(' and ')}`);
 $('chart-unit').textContent={throughput:'SUCCESSFUL RESPONSES / SECOND · TRAILING 1s',queue:'QUEUED REQUESTS',stress:scope<0?'MAX DOWNSTREAM STRESS · %':'DOWNSTREAM STRESS · %',failures:'ERRORS + TIMEOUTS / SECOND · TRAILING 1s'}[metric];
 $('legend').innerHTML=current().runs.map((r,i)=>`<span style="--run-color:${colors[i%colors.length]}"><i></i>${esc(r.name)}</span>`).join('');
}
function stateTracks(){
 const sid=focusService(),s=current().scenario;$('state-scope').textContent=s.services[sid].name.toUpperCase()+(scope<0?' · FOCUS DEPENDENCY':'');
 $('state-tracks').innerHTML=current().runs.map(run=>{
  let phase=run.snapshots.find(s=>s.service===sid)?.phase||'closed',start=0,parts=[];
  for(const t of run.transitions.filter(t=>t.service===sid&&t.at_ms<=s.duration_ms)){parts.push({phase,start,end:t.at_ms});phase=t.to;start=t.at_ms;}
  parts.push({phase,start,end:s.duration_ms});
  return `<div class="state-track"><span>${esc(run.name)}</span><div class="track">${parts.map(p=>`<span class="state-segment ${p.phase}" style="left:${100*p.start/s.duration_ms}%;width:${Math.max(.08,100*(p.end-p.start)/s.duration_ms)}%" title="${phaseLabel(p.phase)}: ${fmt(p.start/1000,2)}–${fmt(p.end/1000,2)}s"></span>`).join('')}<span class="track-cursor" style="left:${100*cursor/s.duration_ms}%"></span></div></div>`;
 }).join('');
}
function fleets(){
 const s=current().scenario;
 $('server-fleets').innerHTML=current().runs.map((run,ri)=>{
  const rows=s.services.flatMap((server,sid)=>{
   if(scope>=0&&scope!==sid)return[];
   const samples=run.snapshots.filter(v=>v.service===sid),v=samples[indexAt(samples,cursor)],phase=phaseAt(run,sid,cursor);
   return [`<div class="server-row"><div><div class="server-label">${esc(server.name)}</div><div class="server-state ${phase}"><i></i>${phaseLabel(phase)}</div></div><div><div class="workers" title="${v.active} / ${server.workers} workers busy">${Array.from({length:Math.min(24,server.workers)},(_,i)=>`<i class="worker ${i<v.active?'busy':''}"></i>`).join('')}</div><div class="queue-text">${v.active} busy · ${v.queued} queued</div></div><div><div class="stress-label"><span>STRESS</span><b>${fmt(v.stress*100)}%</b></div><div class="stress-meter"><span style="width:${v.stress*100}%;background:${v.stress>.65?'#d48664':v.stress>.3?'#c2a05c':'#73a98e'}"></span></div></div></div>`];
  }).join('');
  return `<div class="fleet" style="--run-color:${colors[ri%colors.length]}"><div class="fleet-name">${esc(run.name.toUpperCase())}<small>AT ${clock(cursor)}</small></div>${rows}</div>`;
 }).join('');
}
function evidence(){
 const data=current(),s=data.scenario;
 $('outcome-filter').hidden=journal!=='requests';
 if(journal==='transitions'){
  const rows=data.runs.flatMap(run=>run.transitions.filter(t=>t.at_ms<=cursor&&(scope<0||t.service===scope)).map(t=>({...t,algorithm:run.name}))).sort((a,b)=>b.at_ms-a.at_ms);
  $('journal-description').textContent=`${rows.length} state changes through ${clock(cursor)}. Exact event times; showing the latest 12.`;
  $('journal-content').innerHTML=rows.length?`<table><thead><tr><th>Time</th><th>Policy</th><th>Downstream</th><th>Transition</th><th>Trigger</th></tr></thead><tbody>${rows.slice(0,12).map(t=>`<tr><td>${fmt(t.at_ms/1000,3)}s</td><td>${esc(t.algorithm)}</td><td>${esc(s.services[t.service].name)}</td><td>${pill(t.from)}<span class="state-arrow">→</span>${pill(t.to)}</td><td>${esc(t.trigger)}</td></tr>`).join('')}</tbody></table>`:`<div class="empty-state"><strong>No state changes at this point.</strong>Move the timeline into the incident, or inspect individual requests.</div>`;
 }else{
  const filter=$('outcome-filter').value;
  const rows=data.runs.flatMap(run=>run.requests.filter(r=>r.arrived_ms<=cursor&&(scope<0||r.service===scope)).map(r=>({...r,algorithm:run.name,status:r.client_finished_ms<=cursor&&r.client_finished_ms!=null?r.outcome:r.started_ms!=null&&r.started_ms<=cursor?'working':'queued'}))).filter(r=>filter==='all'||r.status===filter).sort((a,b)=>b.arrived_ms-a.arrived_ms||a.id-b.id);
  $('journal-description').textContent=`${number.format(rows.length)} matching requests through ${clock(cursor)}. Latest 20; complete records are in the JSON export.`;
  $('journal-content').innerHTML=rows.length?`<table><thead><tr><th>Arrival / ID</th><th>Policy</th><th>Downstream</th><th>Client outcome</th><th>Elapsed</th><th>Server work</th></tr></thead><tbody>${rows.slice(0,20).map(r=>{
   const done=r.client_finished_ms!=null&&r.client_finished_ms<=cursor;const elapsed=(done?r.client_finished_ms:cursor)-r.arrived_ms;
   const serverDone=r.downstream_finished_ms!=null&&r.downstream_finished_ms<=cursor;
   const backend=r.status==='shed'?'Never admitted':serverDone?'Finished':r.started_ms!=null&&r.started_ms<=cursor?'Still running':'Still queued';
   return `<tr><td>${fmt(r.arrived_ms/1000,3)}s <span class="muted">#${r.id}</span></td><td>${esc(r.algorithm)}</td><td>${esc(s.services[r.service].name)}${r.probe?' · probe':''}</td><td class="${esc(r.status)}">${esc({success:'Successful',error:'Error',timeout:'Timed out',shed:'Shed',working:'In flight',queued:'Queued'}[r.status])}</td><td>${fmt(elapsed)} ms</td><td>${backend}</td></tr>`;
  }).join('')}</tbody></table>`:'<div class="empty-state"><strong>No matching requests yet.</strong>Move the timeline forward or change the outcome filter.</div>';
 }
}
function update(){
 const s=current().scenario;$('scrubber').value=cursor;$('clock').textContent=clock(cursor);$('play').textContent=playing?'Ⅱ':'▶';$('play').setAttribute('aria-label',playing?'Pause simulation':'Play simulation');
 const p=activePhase();document.querySelectorAll('[data-phase]').forEach(b=>b.classList.toggle('current',s.phases[Number(b.dataset.phase)]===p));
 $('phase-detail').innerHTML=`<b>${esc(p.name)} · ${fmt(p.start_ms/1000)}–${fmt(p.end_ms/1000)}s</b><span> &nbsp; ${esc(p.description)}</span>`;
 $('environment-values').innerHTML=s.services.map(server=>{const c=p.changes.find(v=>v.service===server.id);const rate=server.rate*(c?.rate_multiplier??1),latency=c?.latency_multiplier??1,error=1-(1-server.base_error_probability)*(1-(c?.error_probability??0));return `<div><strong>${esc(server.name)}</strong><span>${fmt(rate,1)} req/s · ${fmt(server.work_ms*latency)} ms nominal service</span><span>${fmt(100*error,1)}% injected error · ${server.workers} workers · queue limit ${server.queue_limit}</span></div>`;}).join('');
 const x=42+946*cursor/s.duration_ms;$('chart').querySelector('[data-cursor]').setAttribute('x1',x);$('chart').querySelector('[data-cursor]').setAttribute('x2',x);$('chart').querySelector('[data-cursor-dot]').setAttribute('cx',x);
 document.querySelectorAll('.track-cursor').forEach(n=>n.style.left=`${100*cursor/s.duration_ms}%`);fleets();evidence();
}
function seek(at){cursor=Math.max(0,Math.min(current().scenario.duration_ms,at));update();}
$('scenario-nav').addEventListener('click',e=>{const b=e.target.closest('[data-scenario]');if(!b)return;selected=Number(b.dataset.scenario);scope=-1;cursor=Math.min(32000,current().scenario.duration_ms);scenario();});
$('service-tabs').addEventListener('click',e=>{const b=e.target.closest('[data-service]');if(!b)return;scope=Number(b.dataset.service);seriesCache=current().runs.map(buildSeries);document.querySelectorAll('[data-service]').forEach(n=>n.classList.toggle('selected',Number(n.dataset.service)===scope));comparison();drawChart();stateTracks();update();});
$('metric-tabs').addEventListener('click',e=>{const b=e.target.closest('[data-metric]');if(!b)return;metric=b.dataset.metric;document.querySelectorAll('[data-metric]').forEach(n=>n.classList.toggle('selected',n.dataset.metric===metric));drawChart();});
$('phase-strip').addEventListener('click',e=>{const b=e.target.closest('[data-phase]');if(b)seek(current().scenario.phases[Number(b.dataset.phase)].start_ms);});
$('journal-tabs').addEventListener('click',e=>{const b=e.target.closest('[data-tab]');if(!b)return;journal=b.dataset.tab;document.querySelectorAll('[data-tab]').forEach(n=>n.classList.toggle('selected',n.dataset.tab===journal));evidence();});
$('outcome-filter').addEventListener('change',evidence);
$('scrubber').addEventListener('input',e=>{playing=false;seek(Number(e.target.value));});
$('play').addEventListener('click',()=>{if(cursor>=current().scenario.duration_ms)cursor=0;playing=!playing;lastFrame=performance.now();update();});
$('reset').addEventListener('click',()=>{playing=false;seek(0);});
$('model-button').addEventListener('click',()=>$('model-dialog').showModal());$('close-model').addEventListener('click',()=>$('model-dialog').close());
$('model-dialog').addEventListener('click',e=>{if(e.target===$('model-dialog')){const r=e.target.getBoundingClientRect();if(e.clientX<r.left||e.clientX>r.right||e.clientY<r.top||e.clientY>r.bottom)e.target.close();}});
$('export-button').addEventListener('click',()=>{const blob=new Blob([JSON.stringify(report)],{type:'application/json'});const url=URL.createObjectURL(blob);const a=document.createElement('a');a.href=url;a.download=`reflex-circuit-lab-seed-${report.seed}.json`;a.click();setTimeout(()=>URL.revokeObjectURL(url),1000);});
function chartAt(e){const r=$('chart').getBoundingClientRect();return Math.max(0,Math.min(1,((e.clientX-r.left)/r.width*1000-42)/946))*current().scenario.duration_ms;}
$('chart').addEventListener('pointermove',e=>{const at=chartAt(e),tip=$('chart-tooltip'),r=$('chart').getBoundingClientRect();tip.hidden=false;tip.style.left=`${Math.min(r.width-205,Math.max(0,e.clientX-r.left+12))}px`;tip.innerHTML=`${fmt(at/1000,2)}s${seriesCache.map((list,i)=>`<span>${esc(current().runs[i].name)}: ${fmt(list[indexAt(list,at)][metric],metric==='stress'?1:0)}${metric==='stress'?'%':''}</span>`).join('')}`;});
$('chart').addEventListener('pointerleave',()=>$('chart-tooltip').hidden=true);
$('chart').addEventListener('click',e=>{playing=false;seek(chartAt(e));});
function frame(now){if(playing){const dt=Math.min(200,now-lastFrame);cursor=Math.min(current().scenario.duration_ms,cursor+dt*Number($('speed').value));if(cursor>=current().scenario.duration_ms)playing=false;if(now-lastPaint>=90){update();lastPaint=now;}}lastFrame=now;requestAnimationFrame(frame);}
scenario();requestAnimationFrame(frame);
