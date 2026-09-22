import '@datadog/druids/styles.css';
import React,{useEffect,useMemo,useRef,useState} from 'react';
import {createRoot} from 'react-dom/client';
import {renderHeader} from './header.jsx';
import {DruidsEnvironment} from '@datadog/druids/layout/DruidsEnvironment';
import {Button} from '@datadog/druids/form/Button';
import {SoftToggle} from '@datadog/druids/form/SoftToggle';
import {ToggleButtons} from '@datadog/druids/form/ToggleButtons';
import {ToggleSwitch} from '@datadog/druids/form/ToggleSwitch';
import {Text} from '@datadog/druids/typography/Text';
import {CalloutValue} from '@datadog/druids/measures/CalloutValue';
import {StatusPill} from '@datadog/druids/pills/StatusPill';
import {PlayIcon} from '@datadog/druids/icons/Play';
import {PauseIcon} from '@datadog/druids/icons/Pause';
import {PlusLightIcon} from '@datadog/druids/icons/PlusLight';
import {UsersIcon} from '@datadog/druids/icons/Users';
import {ServerIcon} from '@datadog/druids/icons/Server';
import {GlobeIcon} from '@datadog/druids/icons/Globe';
import {FlowMap} from './flow-map.jsx';
import {PolicyComparison,PolicySwitcher} from './policy-comparison.jsx';
import {ScenarioTrend} from './scenario-trend.jsx';
import {SchedulerUtilizationCharts} from './scheduler-utilization.jsx';
import {SchedulerLagChart} from './scheduler-lag.jsx';
import {ScenarioControls} from './scenario-controls.jsx';

const scenario=location.pathname.slice(1);
const $=id=>document.getElementById(id);
const mount=(id,parent)=>{const el=document.createElement('div');el.id=id;parent.append(el);return createRoot(el);};
// Reparent existing scenario content rather than recreating forms: their input
// state, validation, event handlers, and decision inspection stay intact.
const main=document.querySelector('main'),inspector=document.querySelector('.topology-inspector');
const oldHeader=document.querySelector('body>header');oldHeader.hidden=true;
main.querySelector('nav').hidden=true;main.querySelector('.heading').hidden=true;
const legacyScenarioControls=main.querySelector('.preset-controls,.scenario-controls');
const scenarioOptions=$('scenario')?Array.from($('scenario').options,o=>({value:o.value,label:o.textContent})):[];
const recoveryBudget=scenario==='recovery'?$('bandwidth').closest('label'):null;
if(legacyScenarioControls)legacyScenarioControls.hidden=true;
const toolbar=document.querySelector('.toolbar');toolbar.hidden=true;
const toolbarHost=document.createElement('div');toolbar.before(toolbarHost);const toolbarRoot=mount('scenario-toolbar',toolbarHost);
const intro=document.createElement('section');intro.className='simulation-intro';toolbarHost.before(intro);
const introTitle=document.createElement('h1');introTitle.className='simulation-title';introTitle.textContent=scenario==='scheduler'?'Resource Scheduler':'Recovery';
const introCopy=document.createElement('div');introCopy.className='simulation-intro-copy';introCopy.append(introTitle,main.querySelector('.scenario-description'));
toolbarHost.className='simulation-intro-controls';intro.append(introCopy,toolbarHost);

const legacyComparison=$('comparison');
let comparisonRoot;
if(legacyComparison){legacyComparison.hidden=true;const host=document.createElement('div');legacyComparison.before(host);host.id='policy-comparison';comparisonRoot=createRoot(host);}
const layout=document.querySelector('.topology-layout'),mapPanel=document.querySelector('.topology-panel');
const playbackHost=document.createElement('div');playbackHost.className='map-playback';playbackHost.setAttribute('role','region');playbackHost.setAttribute('aria-label','Simulation playback');mapPanel.prepend(playbackHost);const playbackRoot=mount('scenario-playback',playbackHost);
const mapRoot=mount('scenario-map',mapPanel);mapPanel.querySelector('.topology-scroll').hidden=true;mapPanel.querySelector('.topology-heading').hidden=true;mapPanel.querySelector('.topology-legend').hidden=true;
const heading=document.createElement('header');heading.className='scenario-inspector-heading';
const eyebrow=inspector.querySelector('.eyebrow');if(eyebrow)eyebrow.remove();
const legacyHeading=document.createElement('div');legacyHeading.hidden=true;
legacyHeading.append($('inspector-title'));if($('inspector-help'))legacyHeading.append($('inspector-help'));
heading.append(legacyHeading);const headingRoot=mount('scenario-entity-heading',heading);
const content=document.createElement('div');content.className='scenario-inspector-scroll';
const overview=document.createElement('div');overview.dataset.section='overview';
while(inspector.firstChild)overview.append(inspector.firstChild);
inspector.append(heading);const tabRoot=mount('scenario-inspector-tabs',inspector);inspector.append(content);content.append(overview);
const trends=document.createElement('div');trends.dataset.section='trends';trends.hidden=true;content.append(trends);
const activity=document.createElement('div');activity.dataset.section='activity';activity.hidden=true;content.append(activity);
let nodeUtilizationHost,nodeUtilizationRoot,poolUtilizationRoot;
if(scenario==='scheduler'){
  nodeUtilizationHost=document.createElement('div');nodeUtilizationHost.hidden=true;
  $('node-details').after(nodeUtilizationHost);nodeUtilizationRoot=createRoot(nodeUtilizationHost);
  const host=document.createElement('div');trends.prepend(host);poolUtilizationRoot=createRoot(host);
}

if($('evidence-source'))overview.prepend($('evidence-source'));
if(recoveryBudget)overview.append(recoveryBudget);
main.querySelectorAll('.status-line:not(#evidence-source)').forEach(el=>el.hidden=true);
for(const selector of ['.metrics','#intervention']){const el=main.querySelector(selector);if(el)overview.append(el);}
for(const selector of ['.bottom-grid','.pulse','.client-lag-panel']){const el=main.querySelector(selector);if(el)trends.append(el);}
const journal=main.querySelector('.journal');if(journal)activity.append(journal);
const legacyChart=$('trend')||$('chart');
const chartHost=document.createElement('div');legacyChart.before(chartHost);legacyChart.style.display='none';
const trendRoot=createRoot(chartHost);
const lagRoots=scenario==='scheduler'?[
 ['client-oldest-lag','oldest_wait_ms','Oldest queued request waiting time per client'],
 ['client-start-lag','recent_mean_start_lag_ms','Rolling ten second mean start lag per client'],
].map(([id,field,label])=>{
 const svg=$(id),host=document.createElement('div');svg.before(host);svg.style.display='none';
 return {root:createRoot(host),field,label};
}):[];

for(const el of trends.querySelectorAll('.chart-legend,.resource-key'))el.hidden=true;
if(scenario==='recovery')trends.querySelector('.pulse .section-head>.helper').textContent='Essential success: left axis · ready replicas: right axis';
const legacyMetrics=overview.querySelector('.metrics');
const metricFields=legacyMetrics?Array.from(legacyMetrics.children,el=>({id:el.querySelector('strong').id,label:el.querySelector('span').childNodes[0].textContent.trim()})):[];
const metricsRoot=legacyMetrics?mount('scenario-metrics',overview):null;
if(legacyMetrics)legacyMetrics.hidden=true;
function MetricCards(){return <section className="scenario-outcomes"><Text as="h2" weight="bold" size="lg">System metrics</Text><div className="scenario-metric-cards">{metricFields.map(({id,label})=><div key={id}><CalloutValue label={label} value={$(id).textContent} size="sm" level={id==='completed'||id==='success'?'success':id==='rejected'&&Number($(id).textContent.replaceAll(',',''))>0?'warning':'default'}/></div>)}</div><Button label="Cost details" size="sm" isPrimary isDangerouslyNaked isTitleCased={false} onClick={()=>$('cost-info').click()}/></section>;}
const replicaLevel=n=>n.phase==='empty'?'default':n.phase==='unavailable'||(n.phase==='ready'&&!n.reachable)?'danger':n.phase==='ready'?'success':'warning';
const titleCase=value=>value.charAt(0).toUpperCase()+value.slice(1);
function InspectorHeading({state,selected,busy,connected,send}){
 const client=selected.kind==='client'?state.clients.find(c=>c.id===selected.id):null;
 const config=client?.config||client;
 const node=selected.kind==='node'?state.nodes?.[selected.id]:null;
 const replica=selected.kind==='replica'?state.replicas[selected.id]:null;
 const observed=replica?state.evidence.replicas[replica.id]:null;
 const Icon=client?UsersIcon:node||replica?ServerIcon:GlobeIcon;
 const color=client?'#54b49b':node||replica?'#bd53b5':'#54b49b';
 const status=client?(config.enabled?'Sending':'Paused'):replica?titleCase(replica.phase):node?'Ready':'Queued';
 const level=client?(config.enabled?'success':'default'):replica?replicaLevel(replica):node?'success':'default';
 const stats=client?[['Rate',`${fmt(config.rate)} ${scenario==='scheduler'?'jobs':'reads'}/s`],...(scenario==='scheduler'?[['Priority',titleCase(client.priority||'normal')]]:[['Essential',`${config.essential_pct}%`]])]:node?[['CPU',`${node.used_cpu} / ${node.cpu}`],['Memory',`${node.used_memory_gib} / ${node.memory_gib} GiB`]]:replica?[['In flight',observed.in_flight],['Queued',observed.queue_depth],['Snapshot',`v${replica.version}`]]:[['Queued',state.queued],['Oldest wait',`${fmt(state.oldest_wait_ms/1000)}s`]];
 return <div className="scenario-entity-summary">
  <div className="scenario-heading-actions"><Text size="sm" variant="secondary" weight="bold">{scenario==='scheduler'?'Resource scheduler':'Recovery'}</Text><Button label="Add client" icon={PlusLightIcon} size="sm" isPrimary isDangerouslyNaked isTitleCased={false} isDisabled={busy||!connected||state.replay||state.clients.length>=8} onClick={()=>$('add-client').click()}/></div>
  <div className="scenario-entity-row"><span className="scenario-entity"><span className="scenario-entity-icon" style={{background:color}}><Icon size="md"/></span><Text>{$('inspector-title').textContent}</Text></span><span className="scenario-client-traffic">{scenario==='scheduler'&&client?<ToggleSwitch id={`client-traffic-${client.id}`} ariaLabel={`${client.name} traffic`} label="Traffic" size="sm" hasStatusColor isChecked={config.enabled} isDisabled={busy||!connected} onChange={()=>send({type:'client',id:client.id,config:{...config,enabled:!config.enabled}})}/>:<StatusPill isSoft level={level}>{status}</StatusPill>}</span></div>
  <Text size="sm" variant="secondary">{$('inspector-help')?.textContent}</Text>
  <div className="scenario-header-stats">{stats.map(([label,value])=><span key={label}><Text variant="secondary">{label}: </Text><Text weight="bold">{value}</Text></span>)}</div>
 </div>;
}
const formatTime=ms=>`${String(Math.floor(ms/60000)).padStart(2,'0')}:${String(Math.floor(ms/1000)%60).padStart(2,'0')}`;
const env=child=><DruidsEnvironment defaultThemePreference="light">{child}</DruidsEnvironment>;
function InspectorTabs({selection}){
 const [tab,setTab]=useState('overview');
 useEffect(()=>{setTab('overview');},[selection]);
 useEffect(()=>{for(const section of content.children)section.hidden=section.dataset.section!==tab;content.scrollTop=0;},[tab,selection]);
 return <SoftToggle ariaLabel="Inspector sections" value={tab} onChange={setTab} isFullWidth hasEqualWidthOptions options={[{label:'Overview',value:'overview'},{label:'Trends',value:'trends'},{label:'Activity',value:'activity'}]}/>;
}
function Toolbar({state,busy,connected,send,selected,onSelectPolicy}){
 const ended=state.at_ms>=(state.horizon_ms||state.duration_ms||180000),disabled=busy||!connected,datadog=state.evidence_source==='datadog';
 return <><div className="scenario-transport"><div className="scenario-transport-group">
 <Button icon={state.paused?PlayIcon:PauseIcon} label={ended?'Complete':state.paused?(state.at_ms?'Resume':'Start traffic'):'Pause'} isPrimary isTitleCased={false} isDisabled={disabled||ended} onClick={()=>send({type:state.paused?'play':'pause'})}/>
 <Button label="+1s" ariaLabel="Step one second" isDisabled={datadog||disabled||ended} onClick={()=>send({type:'step'})}/>
 <Text isMonospace size="sm">{formatTime(state.at_ms)} / {formatTime(state.horizon_ms||state.duration_ms||180000)}</Text>
 <StatusPill isSoft level={!state.paused&&!ended?'success':'default'}>{ended?'Complete':state.replay?'Replay':state.paused?'Paused':'Running'}</StatusPill>
 {scenario==='capacity'&&<PolicySwitcher state={state} selected={selected} onSelectPolicy={onSelectPolicy}/>}

 </div><div className="scenario-transport-group playback-actions">
 <ToggleButtons aria-label="Playback speed" options={(scenario==='capacity'?[1,2,4,10]:[1,2,4]).map(n=>({value:n,label:`${n}×`}))} value={state.speed} isDisabled={datadog||disabled} onChange={value=>send({type:'speed',value})}/>
 {scenario==='capacity'&&<Button label="Replay" isBorderless isDisabled={disabled||!state.at_ms} onClick={()=>send({type:'replay'},true)}/>}
 <Button label="Reset" isBorderless isDisabled={disabled} onClick={()=>send({type:'reset'},true)}/></div></div></>;
}
const fmt=n=>Number(n).toLocaleString('en-US',{maximumFractionDigits:1});
function Graph({state,selected,onSelect,width,height,panelOpen,setPanelOpen}){
 const {nodes,links}=useMemo(()=>{
  const nodes=new Map(),links=new Map(),lane=state.lanes?.[selected.lane||0];
  const add=(id,data)=>nodes.set(id,{...data,selected:selected.kind===data.kind&&selected.id===data.id});
  const edge=(a,b,active=true,status='default')=>links.set(`${a}:${b}`,{sourceId:a,targetId:b,lineWidth:2,arrowStyle:'chevron',status,strokeStyle:active?'solid':'dotted',active});
  const hub=scenario==='scheduler'?'placement':'router';
  state.clients.forEach(c=>{const config=c.config||c;add(`client-${c.id}`,{kind:'client',id:c.id,name:c.name||`Client ${c.id+1}`,subtext:config.enabled?`${fmt(config.rate)} ${scenario==='recovery'?'reads':'jobs'}/s`:'Paused',enabled:config.enabled});edge(`client-${c.id}`,scenario==='scheduler'?'queue':hub,config.enabled&&config.rate>0);});
  if(scenario==='scheduler'){
   add('queue',{kind:'queue',id:0,name:'Shared queue',subtext:'Priority-aware · FIFO per client',subtextLines:2,metrics:[`${state.queued} waiting`,`${fmt(state.oldest_wait_ms/1000)}s oldest wait`]});edge('queue',hub,state.queued>0||state.running>0);
  }
  add(hub,{kind:'hub',id:0,name:scenario==='recovery'?'Read router':scenario==='scheduler'?'Placement':'FIFO placement',subtext:'Protected by Reflex',metrics:scenario==='recovery'?[state.essential_only?'Essential reads only':'Normal service',state.retries_enabled?'Retries enabled':'Retries off']:scenario==='scheduler'?[state.policy==='jev'?'Jev + Reflex':state.policy==='best_fit'?'Best Fit':'First Fit',`${state.running} running`]:[lane.label,`${lane.queued} queued · ${lane.running} running`],status:'success'});
  if(scenario==='recovery'){
   state.replicas.forEach(n=>{const e=state.evidence.replicas[n.id],healthy=n.phase==='ready'&&n.reachable;add(`replica-${n.id}`,{kind:'replica',id:n.id,name:n.name,phase:n.phase,status:replicaLevel(n),metrics:[`${e.in_flight} in flight · ${e.queue_depth} queued`,`Snapshot v${n.version}`,n.serving?'In serving pool':'Not serving']});edge(hub,`replica-${n.id}`,n.serving&&healthy,healthy?'default':n.phase==='empty'?'default':'warning');});
   const r=state.recovery;if(r&&['rebuilding','verifying'].includes(r.phase))edge(`replica-${r.source}`,`replica-${r.target}`,true,'warning');
  }else{
   (lane?.nodes||state.nodes).forEach((n,i)=>{const id=n.id??i,phase=n.phase||'ready';add(`node-${id}`,{kind:'node',id,name:n.name||`Node ${id+1}`,phase,status:phase==='ready'?'success':['starting','draining'].includes(phase)?'warning':'default',metrics:[`${n.used_cpu} / ${n.cpu} CPU`,`${n.used_memory_gib} / ${n.memory_gib} GiB`],ratio:n.used_cpu/n.cpu});edge(hub,`node-${id}`,phase==='ready'&&n.used_cpu>0);});
  }
  return {nodes,links};
 },[state,selected]);
 const running=!state.paused&&state.at_ms<(state.horizon_ms||state.duration_ms||180000);
 const mapNodes=Array.from(nodes,([key,n])=>({...n,key,column:n.kind==='client'?0:n.kind==='queue'?1:n.kind==='hub'?(scenario==='scheduler'?2:1):(scenario==='scheduler'?3:2)}));
 return <FlowMap nodes={mapNodes} edges={Array.from(links,([key,edge])=>({...edge,key}))} running={running} speed={state.speed} panelOpen={panelOpen} setPanelOpen={setPanelOpen} onSelect={key=>{const n=nodes.get(key);if(n.kind!=='hub')onSelect({kind:n.kind,id:n.id});}}/>;
}
function ScenarioMap(props){
 const ref=useRef(null),[size,setSize]=useState({width:0,height:0}),[panelOpen,setPanelOpen]=useState(true);
 useEffect(()=>{inspector.hidden=!panelOpen;},[panelOpen]);
 useEffect(()=>{const ro=new ResizeObserver(([e])=>setSize({width:e.contentRect.width,height:e.contentRect.height}));ro.observe(ref.current);return()=>ro.disconnect();},[]);
 const onSelect=selection=>{setPanelOpen(true);props.onSelect(selection);};
 return <div className="scenario-graph" ref={ref}>
 {size.width>0&&size.height>0&&<Graph {...props} {...size} onSelect={onSelect} panelOpen={panelOpen} setPanelOpen={setPanelOpen}/>}
 <div className="scenario-map-footer"><div className="scenario-map-legend" aria-label={scenario==='recovery'?'Replica states':'Node CPU utilization'}>{(scenario==='recovery'?[['Ready','success'],['Rebuilding / Checking','warning'],['Unavailable','danger'],['Empty','default']]:[['CPU ≤30%','success'],['30–65%','warning'],['>65%','danger']]).map(([label,level])=><span key={label}><i data-level={level}/>{label}</span>)}</div><Text size="xs" variant="secondary">Drag to pan · select a node to inspect</Text></div></div>;
}
window.renderScenarioUI=props=>{
 renderHeader({disabled:props.busy||!props.connected,navigate:async path=>{if(await props.send({type:'pause'}))location.href=path;}});
 toolbarRoot.render(env(scenarioOptions.length>0?<ScenarioControls state={props.state} disabled={props.busy||!props.connected} send={props.send} options={scenarioOptions} description={$('scenario-description').textContent}/>:null));
 playbackRoot.render(env(<Toolbar {...props}/>));
 headingRoot.render(env(<InspectorHeading {...props}/>));
 comparisonRoot?.render(env(<PolicyComparison {...props}/>));
 trendRoot.render(env(<ScenarioTrend scenario={scenario} {...props}/>));
 if(nodeUtilizationRoot){
  nodeUtilizationHost.hidden=props.selected.kind!=='node';
  nodeUtilizationRoot.render(env(props.selected.kind==='node'?<SchedulerUtilizationCharts state={props.state} node={props.selected.id}/>:null));
  poolUtilizationRoot.render(env(<SchedulerUtilizationCharts state={props.state}/>));
 }

 for(const {root,field,label} of lagRoots)root.render(env(<SchedulerLagChart state={props.state} hiddenClients={props.hiddenLagClients} field={field} label={label}/>));
 metricsRoot?.render(env(<MetricCards/>));
 tabRoot.render(env(<InspectorTabs selection={`${props.selected.kind}:${props.selected.id}`}/>));
 mapRoot.render(env(<ScenarioMap {...props}/>));
};
