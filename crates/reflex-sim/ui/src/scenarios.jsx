import '@datadog/druids/styles.css';
import React,{useEffect,useMemo,useRef,useState} from 'react';
import {createRoot} from 'react-dom/client';
import {renderHeader} from './header.jsx';
import {DruidsEnvironment} from '@datadog/druids/layout/DruidsEnvironment';
import {Button} from '@datadog/druids/form/Button';
import {Select} from '@datadog/druids/form/Select';
import {SoftToggle} from '@datadog/druids/form/SoftToggle';
import {ToggleButtons} from '@datadog/druids/form/ToggleButtons';
import {Text} from '@datadog/druids/typography/Text';
import {CalloutValue} from '@datadog/druids/measures/CalloutValue';
import {StatusPill} from '@datadog/druids/pills/StatusPill';
import {PlayIcon} from '@datadog/druids/icons/Play';
import {PauseIcon} from '@datadog/druids/icons/Pause';
import {PlusLightIcon} from '@datadog/druids/icons/PlusLight';
import {FlowMap} from './flow-map.jsx';
import {PolicyComparison,PolicySwitcher} from './policy-comparison.jsx';
import {ScenarioTrend} from './scenario-trend.jsx';

const scenario=location.pathname.slice(1);
const titles={scheduler:'Resource scheduling topology',recovery:'Replica recovery topology',capacity:'Forecast capacity topology'};
const summaries={scheduler:'Clients → shared queue → placement → node pool',recovery:'Clients → read router → replicas',capacity:'Clients → FIFO placement → node pool'};
const $=id=>document.getElementById(id);
const mount=(id,parent)=>{const el=document.createElement('div');el.id=id;parent.append(el);return createRoot(el);};
// Reparent existing scenario content rather than recreating forms: their input
// state, validation, event handlers, and decision inspection stay intact.
const main=document.querySelector('main'),inspector=document.querySelector('.topology-inspector');
const oldHeader=document.querySelector('body>header');oldHeader.hidden=true;
main.querySelector('nav').hidden=true;main.querySelector('.heading').hidden=true;
const toolbar=document.querySelector('.toolbar');toolbar.hidden=true;
const toolbarHost=document.createElement('div');toolbar.before(toolbarHost);const toolbarRoot=mount('scenario-toolbar',toolbarHost);
const legacyComparison=$('comparison');
let comparisonRoot;
if(legacyComparison){legacyComparison.hidden=true;const host=document.createElement('div');legacyComparison.before(host);host.id='policy-comparison';comparisonRoot=createRoot(host);}
const layout=document.querySelector('.topology-layout'),mapPanel=document.querySelector('.topology-panel');
const mapRoot=mount('scenario-map',mapPanel);mapPanel.querySelector('.topology-scroll').hidden=true;mapPanel.querySelector('.topology-heading').hidden=true;mapPanel.querySelector('.topology-legend').hidden=true;
const heading=document.createElement('header');heading.className='scenario-inspector-heading';
const eyebrow=inspector.querySelector('.eyebrow');if(eyebrow)eyebrow.remove();
heading.append($('inspector-title'));if($('inspector-help'))heading.append($('inspector-help'));
const content=document.createElement('div');content.className='scenario-inspector-scroll';
const overview=document.createElement('div');overview.dataset.section='overview';
while(inspector.firstChild)overview.append(inspector.firstChild);
inspector.append(heading);const tabRoot=mount('scenario-inspector-tabs',inspector);inspector.append(content);content.append(overview);
const trends=document.createElement('div');trends.dataset.section='trends';trends.hidden=true;content.append(trends);
const activity=document.createElement('div');activity.dataset.section='activity';activity.hidden=true;content.append(activity);
if($('evidence-source'))overview.prepend($('evidence-source'));
for(const selector of ['.preset-controls','.scenario-controls','.status-line','.metrics','#intervention']){const el=main.querySelector(selector);if(el)overview.append(el);}
for(const selector of ['.bottom-grid','.pulse','.client-lag-panel','.forecast-panel']){const el=main.querySelector(selector);if(el)trends.append(el);}
const journal=main.querySelector('.journal');if(journal)activity.append(journal);
const legacyChart=$('trend')||$('chart');
const chartHost=document.createElement('div');legacyChart.before(chartHost);legacyChart.style.display='none';
const trendRoot=createRoot(chartHost);
for(const el of trends.querySelectorAll('.chart-legend,.resource-key'))el.hidden=true;
if(scenario==='recovery')trends.querySelector('.pulse .section-head>.helper').textContent='Essential success: left axis · ready replicas: right axis';
const legacyMetrics=overview.querySelector('.metrics');
const metricFields=legacyMetrics?Array.from(legacyMetrics.children,el=>({id:el.querySelector('strong').id,label:el.querySelector('span').childNodes[0].textContent.trim()})):[];
const metricsRoot=legacyMetrics?mount('scenario-metrics',overview):null;
if(legacyMetrics)legacyMetrics.hidden=true;
function MetricCards(){return <><div className="scenario-metric-cards">{metricFields.map(({id,label})=><div key={id}><CalloutValue label={label} value={$(id).textContent} size="sm"/></div>)}</div><Button label="Cost details" size="sm" isBorderless onClick={()=>$('cost-info').click()}/></>;}
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
 const policies=$('policy')?Array.from($('policy').options,o=>({value:o.value,label:o.textContent,disabled:o.disabled})):[];
 return <div className="scenario-transport"><div className="scenario-transport-group">
 <Button icon={state.paused?PlayIcon:PauseIcon} label={ended?'Complete':state.paused?(state.at_ms?'Resume':'Start traffic'):'Pause'} isPrimary isTitleCased={false} isDisabled={disabled||ended} onClick={()=>send({type:state.paused?'play':'pause'})}/>
 <Button label="+1s" ariaLabel="Step one second" isDisabled={datadog||disabled||ended} onClick={()=>send({type:'step'})}/>
 <Text isMonospace size="sm">{formatTime(state.at_ms)} / {formatTime(state.horizon_ms||state.duration_ms||180000)}</Text>
 <StatusPill isSoft level={!state.paused&&!ended?'success':'default'}>{ended?'Complete':state.replay?'Replay':state.paused?'Paused':'Running'}</StatusPill></div>
 <div className="scenario-transport-group scenario-transport-actions">{scenario==='capacity'&&<PolicySwitcher state={state} selected={selected} onSelectPolicy={onSelectPolicy}/>}
 {policies.length>0&&<div className="scenario-policy"><Select isFullWidth aria-label="Simulation policy" options={policies} value={state.policy} searchable={false} clearable={false} disabled={datadog||disabled} onChange={o=>send({type:'policy',policy:o.value},true)}/></div>}
 <ToggleButtons aria-label="Playback speed" options={(scenario==='capacity'?[1,2,4,10]:[1,2,4]).map(n=>({value:n,label:`${n}×`}))} value={state.speed} isDisabled={datadog||disabled} onChange={value=>send({type:'speed',value})}/>
 {scenario==='capacity'&&<Button label="Replay" isBorderless isDisabled={disabled||!state.at_ms} onClick={()=>send({type:'replay'},true)}/>}
 <Button label="Reset" isBorderless isDisabled={disabled} onClick={()=>send({type:'reset'},true)}/></div></div>;
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
   add('queue',{kind:'queue',id:0,name:'Shared queue',subtext:'FIFO · oldest first',metrics:[`${state.queued} waiting`,`${fmt(state.oldest_wait_ms/1000)}s oldest wait`]});edge('queue',hub,state.queued>0||state.running>0);
  }
  add(hub,{kind:'hub',id:0,name:scenario==='recovery'?'Read router':scenario==='scheduler'?'Placement':'FIFO placement',subtext:'Protected by Reflex',metrics:scenario==='recovery'?[state.essential_only?'Essential reads only':'Normal service',state.retries_enabled?'Retries enabled':'Retries off']:scenario==='scheduler'?[state.policy==='jev'?'Jev + Reflex':state.policy==='best_fit'?'Best Fit':'First Fit',`${state.running} running`]:[lane.label,`${lane.queued} queued · ${lane.running} running`],status:'success'});
  if(scenario==='recovery'){
   state.replicas.forEach(n=>{const e=state.evidence.replicas[n.id],healthy=n.phase==='ready'&&n.reachable;add(`replica-${n.id}`,{kind:'replica',id:n.id,name:n.name,phase:n.phase,status:healthy?'success':n.phase==='empty'?'default':'warning',metrics:[`${e.in_flight} in flight · ${e.queue_depth} queued`,`Snapshot v${n.version}`,n.serving?'In serving pool':'Not serving']});edge(hub,`replica-${n.id}`,n.serving&&healthy,healthy?'default':n.phase==='empty'?'default':'warning');});
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
 <div className="scenario-map-caption"><Text weight="bold">{titles[scenario]}</Text><Text size="sm" variant="secondary">{summaries[scenario]}</Text><Button label="Add client" icon={PlusLightIcon} size="sm" isBorderless isDisabled={props.busy||props.state.replay||props.state.clients.length>=8} onClick={()=>$('add-client').click()}/></div>
 <div className="scenario-map-footer"><Text size="xs" variant="secondary">Drag to pan · select a node to inspect</Text><Text size="xs" variant="secondary">{props.state.clients.length} client sources · guarded by Reflex</Text></div></div>;
}
window.renderScenarioUI=props=>{
 renderHeader({disabled:props.busy||!props.connected,navigate:async path=>{if(await props.send({type:'pause'}))location.href=path;},onHelp:()=>$('about').click(),exportHref:`/api/${scenario}/export`,exportLabel:'Export run'});
 toolbarRoot.render(env(<Toolbar {...props}/>));
 comparisonRoot?.render(env(<PolicyComparison {...props}/>));
 trendRoot.render(env(<ScenarioTrend scenario={scenario} {...props}/>));
 metricsRoot?.render(env(<MetricCards/>));
 tabRoot.render(env(<InspectorTabs selection={`${props.selected.kind}:${props.selected.id}`}/>));
 mapRoot.render(env(<ScenarioMap {...props}/>));
};
