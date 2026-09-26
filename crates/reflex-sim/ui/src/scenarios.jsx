// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

import '@datadog/druids/styles.css';
import React,{useEffect,useMemo,useState} from 'react';
import {createRoot} from 'react-dom/client';
import {renderHeader} from './header.jsx';
import {ReflexEnvironment} from './theme.jsx';
import {Button} from '@datadog/druids/form/Button';
import {Select} from '@datadog/druids/form/Select';
import {ArrowLeftIcon} from '@datadog/druids/icons/ArrowLeft';
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
import './forecast-panel.jsx';

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
if(legacyScenarioControls)legacyScenarioControls.hidden=true;
const toolbar=document.querySelector('.toolbar');toolbar.hidden=true;
const toolbarHost=document.createElement('div');toolbar.before(toolbarHost);const toolbarRoot=mount('scenario-toolbar',toolbarHost);
main.querySelector('.scenario-description').hidden=true;

const legacyComparison=$('comparison');
let comparisonRoot;
if(legacyComparison){legacyComparison.hidden=true;const host=document.createElement('div');legacyComparison.before(host);host.id='policy-comparison';comparisonRoot=createRoot(host);}
const mapPanel=document.querySelector('.topology-panel');
mapPanel.prepend(toolbarHost);
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
let nodeUtilizationHost,nodeUtilizationRoot,poolUtilizationRoot,queueLagRoot;
if(scenario==='scheduler'){
  const queueHost=document.createElement('div');queueHost.className='queue-wait-chart';$('queue-details').append(queueHost);queueLagRoot=createRoot(queueHost);
  $('queue').style.display='none';$('queue-more').style.display='none';
  nodeUtilizationHost=document.createElement('div');nodeUtilizationHost.hidden=true;
  $('node-details').after(nodeUtilizationHost);nodeUtilizationRoot=createRoot(nodeUtilizationHost);
  const host=document.createElement('div');trends.prepend(host);poolUtilizationRoot=createRoot(host);
}

if($('evidence-source'))$('evidence-source').style.display='none';
main.querySelectorAll('.status-line:not(#evidence-source)').forEach(el=>el.hidden=true);
for(const selector of ['.metrics','#intervention']){const el=main.querySelector(selector);if(el)overview.append(el);}
for(const selector of ['.bottom-grid','.pulse','.client-lag-panel','.forecast-panel']){const el=main.querySelector(selector);if(el)trends.append(el);}
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
const legacyMetrics=overview.querySelector('.metrics');
const metricFields=legacyMetrics?Array.from(legacyMetrics.children,el=>({id:el.querySelector('strong').id,label:el.querySelector('span').childNodes[0].textContent.trim()})):[];
const metricsHost=document.createElement('div');overview.prepend(metricsHost);const metricsRoot=legacyMetrics?mount('scenario-metrics',metricsHost):null;
const poolHost=document.createElement('div');metricsHost.after(poolHost);const poolRoot=createRoot(poolHost);
const policyHost=document.createElement('div');overview.prepend(policyHost);const policyRoot=createRoot(policyHost);
if(legacyMetrics)legacyMetrics.hidden=true;
function MetricCards(){return <section className="scenario-outcomes"><Text as="h2" weight="bold" size="lg">Throughput</Text><div className="scenario-metric-cards">{metricFields.map(({id,label})=><div key={id}><CalloutValue isBorderless label={label} value={$(id).textContent} size="sm" level={id==='completed'||id==='success'?'success':id==='rejected'&&Number($(id).textContent.replaceAll(',',''))>0?'warning':'default'}/></div>)}</div><Button label="Cost details" size="sm" isPrimary isDangerouslyNaked isTitleCased={false} onClick={()=>$('cost-info').click()}/></section>;}
const titleCase=value=>value.charAt(0).toUpperCase()+value.slice(1);
function InspectorHeading({state,selected,busy,connected,send,onSelect}){
 const client=selected.kind==='client'?state.clients.find(c=>c.id===selected.id):null;
 const config=client?.config||client;
 const node=selected.kind==='node'?state.nodes?.[selected.id]:null;
 const Icon=client?UsersIcon:node?ServerIcon:GlobeIcon;
 const status=client?(config.enabled?'Sending':'Paused'):node?'Ready':'Queued';
 const level=client?(config.enabled?'success':'default'):node?'success':'default';
 const stats=client?[['Rate',`${fmt(config.rate)} jobs/s`],['Priority',titleCase(client.priority||'normal')]]:node?[['CPU',`${node.used_cpu} / ${node.cpu}`],['Memory',`${node.used_memory_gib} / ${node.memory_gib} GiB`]]:[['Queued',state.queued],['Oldest wait',`${fmt(state.oldest_wait_ms/1000)}s`]];
 return <div className="scenario-entity-summary">
  <div className="scenario-heading-actions">{selected.kind==='hub'||scenario!=='scheduler'?<Text size="sm" variant="secondary" weight="bold">Resource scheduler</Text>:<Button icon={ArrowLeftIcon} label="Back to placement" isPrimary isDangerouslyNaked isTitleCased={false} onClick={()=>onSelect({kind:'hub',id:0})}/>}<Button label="Add client" icon={PlusLightIcon} size="sm" isPrimary isDangerouslyNaked isTitleCased={false} isDisabled={busy||!connected||state.replay||state.clients.length>=8} onClick={()=>$('add-client').click()}/></div>
  <div className="scenario-entity-row"><span className="scenario-entity"><span className="scenario-entity-icon"><Icon size="md"/></span><Text>{selected.kind==='hub'?'Placement':$('inspector-title').textContent}</Text></span><span className="scenario-client-traffic">{scenario==='scheduler'&&client?<ToggleSwitch id={`client-traffic-${client.id}`} ariaLabel={`${client.name} traffic`} label="Traffic" size="sm" hasStatusColor isChecked={config.enabled} isDisabled={busy||!connected} onChange={()=>send({type:'client',id:client.id,config:{...config,enabled:!config.enabled}})}/>:<StatusPill isSoft level={level}>{selected.kind==='hub'?'Protected by Reflex':status}</StatusPill>}</span></div>
  <Text size="sm" variant="secondary">{selected.kind==='hub'?'Places queued jobs on nodes with available CPU and memory.':$('inspector-help')?.textContent}</Text>
  <div className="scenario-header-stats">{stats.map(([label,value])=><span key={label}><Text variant="secondary">{label}: </Text><Text weight="bold">{value}</Text></span>)}</div>
 </div>;
}
const formatTime=ms=>`${String(Math.floor(ms/60000)).padStart(2,'0')}:${String(Math.floor(ms/1000)%60).padStart(2,'0')}`;
const env=child=><ReflexEnvironment>{child}</ReflexEnvironment>;
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
 <Button isBorderless label="+1s" ariaLabel="Step one second" isDisabled={datadog||disabled||ended} onClick={()=>send({type:'step'})}/>
 <span className="transport-divider"/><Text isMonospace size="sm" title={`Run duration ${formatTime(state.horizon_ms||state.duration_ms||180000)}`}>{formatTime(state.at_ms)}</Text>
 <StatusPill isSoft level={!state.paused&&!ended?'success':'default'}>{ended?'Complete':state.replay?'Replay':state.paused?(state.at_ms?'Paused':'Ready'):'Running'}</StatusPill>
 {scenario==='capacity'&&<PolicySwitcher state={state} selected={selected} onSelectPolicy={onSelectPolicy}/>}

 </div><div className="scenario-transport-group playback-actions">
 {!datadog && <ToggleButtons aria-label="Playback speed" options={(scenario==='capacity'?[1,2,4,10]:[1,2,4]).map(n=>({value:n,label:`${n}×`}))} value={state.speed} isDisabled={disabled} onChange={value=>send({type:'speed',value})}/>}
 {scenario==='capacity'&&<Button label="Replay" isBorderless isDisabled={disabled||!state.at_ms} onClick={()=>send({type:'replay'},true)}/>}
 <Button label="Reset" isBorderless isDisabled={disabled} onClick={()=>send({type:'reset'},true)}/></div></div></>;
}
const fmt=n=>Number(n).toLocaleString('en-US',{maximumFractionDigits:1});
function Graph({state,selected,onSelect}){
 const {nodes,links}=useMemo(()=>{
  const nodes=new Map(),links=new Map(),lane=state.lanes?.[selected.lane||0];
  const add=(id,data)=>nodes.set(id,{...data,selected:data.kind!=='hub'&&selected.kind===data.kind&&selected.id===data.id});
  const edge=(a,b,active=true,status='default')=>links.set(`${a}:${b}`,{sourceId:a,targetId:b,lineWidth:2,arrowStyle:'chevron',status,strokeStyle:active?'solid':'dotted',active});
  const hub=scenario==='scheduler'?'placement':'router';
  state.clients.forEach(c=>{const config=c.config||c;add(`client-${c.id}`,{kind:'client',id:c.id,name:c.name||`Client ${c.id+1}`,subtext:config.enabled?`${fmt(config.rate)} job/s · ${c.priority||'normal'}`:'Paused',enabled:config.enabled});edge(`client-${c.id}`,scenario==='scheduler'?'queue':hub,config.enabled&&config.rate>0);});
  if(scenario==='scheduler'){
   add('queue',{kind:'queue',id:0,name:'Shared queue',subtext:'priority-aware · FIFO',metrics:[{label:'Waiting',value:state.queued},{label:'Oldest',value:`${fmt(state.oldest_wait_ms/1000)}s`}]});edge('queue',hub,state.queued>0||state.running>0);
  }
  add(hub,{kind:'hub',id:0,name:scenario==='scheduler'?'Placement':'FIFO placement',subtext:scenario==='scheduler'?(state.policy==='jev'?'Jev + Reflex':state.policy==='best_fit'?'Best Fit':'First Fit'):lane.label,metrics:scenario==='scheduler'?[{label:'Running',value:state.running},{label:'Placed',value:state.running+state.completed}]:[{label:'Queued',value:lane.queued},{label:'Running',value:lane.running}],status:'success'});
   (lane?.nodes||state.nodes).forEach((n,i)=>{const id=n.id??i,phase=n.phase||'ready';add(`node-${id}`,{kind:'node',id,name:n.name||`Node ${id+1}`,subtext:`${n.cpu} CPU · ${n.memory_gib} GiB`,phase,status:phase==='ready'?'success':['starting','draining'].includes(phase)?'warning':'default',metrics:[{label:"CPU",value:`${n.used_cpu} / ${n.cpu}`},{label:"Mem",value:`${n.used_memory_gib} / ${n.memory_gib}`} ],ratio:n.used_cpu/n.cpu});edge(hub,`node-${id}`,phase==='ready'&&n.used_cpu>0);});
  return {nodes,links};
 },[state,selected]);
 const running=!state.paused&&state.at_ms<(state.horizon_ms||state.duration_ms||180000);
 const mapNodes=Array.from(nodes,([key,n])=>({...n,key,column:n.kind==='client'?0:n.kind==='queue'?1:n.kind==='hub'?(scenario==='scheduler'?2:1):(scenario==='scheduler'?3:2)}));
 return <FlowMap nodes={mapNodes} edges={Array.from(links,([key,edge])=>({...edge,key}))} running={running} speed={state.speed} onSelect={key=>{const n=nodes.get(key);if(n.kind!=='hub'||scenario==='scheduler')onSelect({kind:n.kind,id:n.id});}}/>;
}
function ScenarioMap(props){return <div className="scenario-graph"><Graph {...props}/></div>;}
function SchedulerPolicy({state,busy,connected,send}){
 return <div className="scheduler-policy-control"><h2>Policy</h2><div><Select aria-label="Scheduling policy" value={state.policy} clearable={false} searchable={false} disabled={busy||!connected||state.evidence_source==='datadog'} options={[{value:'jev',label:'Jev + Reflex',isDisabled:!state.available},{value:'first_fit',label:'First Fit'},{value:'best_fit',label:'Best Fit'}]} onChange={option=>send({type:'policy',policy:option.value},true)}/></div></div>;
}
function NodePool({state,onSelect}){
 return <section className="scheduler-pool"><h2>Node pool</h2><table><thead><tr><th>Name</th><th>CPU</th><th>Memory</th></tr></thead><tbody>{state.nodes.map((node,id)=><tr key={id}><td><Button className="service-name" label={node.name} icon={ServerIcon} isDangerouslyNaked isTitleCased={false} onClick={()=>onSelect({kind:'node',id})}/></td><td>{node.used_cpu} / {node.cpu}</td><td>{node.used_memory_gib} / {node.memory_gib}</td></tr>)}</tbody></table></section>;
}
window.renderScenarioUI=props=>{
 renderHeader({disabled:props.busy||!props.connected,connected:props.connected,onHelp:()=>$('about-dialog').showModal(),navigate:async path=>{if(await props.send({type:'pause'}))location.href=path;}});
 toolbarRoot.render(env(scenarioOptions.length>0?<ScenarioControls state={props.state} disabled={props.busy||!props.connected} send={props.send} options={scenarioOptions} description={$('scenario-description').textContent}/>:null));
 playbackRoot.render(env(<Toolbar {...props}/>));
 policyRoot.render(env(scenario==='scheduler'?<SchedulerPolicy {...props}/>:null));
 poolHost.hidden=scenario!=='scheduler'||props.selected.kind!=='hub';poolRoot.render(env(scenario==='scheduler'?<NodePool {...props}/>:null));
 headingRoot.render(env(<InspectorHeading {...props}/>));
 comparisonRoot?.render(env(<PolicyComparison {...props}/>));
 trendRoot.render(env(<ScenarioTrend scenario={scenario} {...props}/>));
 if(nodeUtilizationRoot){
  nodeUtilizationHost.hidden=props.selected.kind!=='node';
  nodeUtilizationRoot.render(env(props.selected.kind==='node'?<SchedulerUtilizationCharts state={props.state} node={props.selected.id}/>:null));
  poolUtilizationRoot.render(env(<SchedulerUtilizationCharts state={props.state}/>));
 }

 queueLagRoot?.render(env(props.selected.kind==='queue'?<section aria-label="Queue waiting time by client"><Text as="h2" size="lg" weight="bold">Oldest waiting time by client</Text><SchedulerLagChart state={props.state} field="oldest_wait_ms" label="Oldest waiting time by client"/><Text as="p" size="xs" variant="secondary">Last 60 simulated seconds. Each line shows the oldest queued job for a client; zero means no queued jobs. Dashed markers show priority changes.</Text></section>:null));
 for(const {root,field,label} of lagRoots)root.render(env(<SchedulerLagChart state={props.state} hiddenClients={props.hiddenLagClients} field={field} label={label}/>));
 metricsRoot?.render(env(<MetricCards/>));
 tabRoot.render(env(<InspectorTabs selection={`${props.selected.kind}:${props.selected.id}`}/>));
 mapRoot.render(env(<ScenarioMap {...props}/>));
};
