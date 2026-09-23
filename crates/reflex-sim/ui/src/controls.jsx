// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

import '@datadog/druids/styles.css';
import React, { useCallback, useEffect, useMemo, useState } from 'react';
import { createRoot } from 'react-dom/client';
import { renderHeader } from './header.jsx';
import { DruidsEnvironment } from '@datadog/druids/layout/DruidsEnvironment';
import { Button } from '@datadog/druids/form/Button';
import { Select } from '@datadog/druids/form/Select';
import { InputSearch } from '@datadog/druids/form/InputSearch';
import { SoftToggle } from '@datadog/druids/form/SoftToggle';
import { ToggleButtons } from '@datadog/druids/form/ToggleButtons';
import { ToggleSwitch } from '@datadog/druids/form/ToggleSwitch';
import { StatusPill } from '@datadog/druids/pills/StatusPill';
import { Table } from '@datadog/druids/table/Table';
import { CalloutValue } from '@datadog/druids/measures/CalloutValue';
import { HorizontalSeparator } from '@datadog/druids/layout/HorizontalSeparator';
import { MessageBox } from '@datadog/druids/misc/MessageBox';
import { Text } from '@datadog/druids/typography/Text';
import { Modal } from '@datadog/druids/dialogs/Modal';
import { GlobeIcon } from '@datadog/druids/icons/Globe';
import { ServerIcon } from '@datadog/druids/icons/Server';
import { HomeIcon } from '@datadog/druids/icons/Home';
import { PlayIcon } from '@datadog/druids/icons/Play';
import { PauseIcon } from '@datadog/druids/icons/Pause';
import { Topology } from './topology.jsx';
import { ScenarioControls } from './scenario-controls.jsx';
import { PressureChart } from './pressure.jsx';
import { helpIntroHtml, helpDetailsHtml } from './help.js';

const fmt = (value, digits = 0) => Number(value).toLocaleString('en-US', { maximumFractionDigits: digits });
const time = ms => `${String(Math.floor(ms / 60000)).padStart(2, '0')}:${(ms / 1000 % 60).toFixed(1).padStart(4, '0')}`;
const phaseName = p => ({ closed: 'Closed', open: 'Open', half_open: 'Probe', bypassed: 'Bypassed' }[p] || p);
const phaseLevel = p => ({ closed: 'success', open: 'danger', half_open: 'warning' }[p] || 'default');
const outcomeName = p => ({ success: 'Success', error: 'Error', timeout: 'Timed out', shed: 'Circuit-shed' }[p] || 'Pending');
const descriptions = ['Looks up products and availability.', 'Processes payment transactions.', 'Finds and ranks matching products.'];
const colors = ['#acb641', '#bd53b5', '#dc8552'];
const choiceName = c => ({open:'Open circuit',probe:'Permit probe',no_change:'No change'}[c] || 'Evaluation error');
const faults = [['slow','Slowdown','6× processing time'], ['errors','Error storm','85% injected error probability'], ['surge','Traffic surge','4× offered requests']];
const NO_PAGES = {isEnabled:false};
const REQUEST_PAGES = {isEnabled:true,pageSize:5};
const TABLE_SUMMARY = {isEnabled:false};
const root = createRoot(document.getElementById('reflex-app'));

function Section({ title, children, action, description }) {
  return <section className="inspector-section"><div className="section-heading"><Text as="h2" weight="bold" size="lg" className="section-title">{title}</Text>{action}</div>{description&&<Text as="p" size="sm" variant="secondary" className="section-description">{description}</Text>}{children}<HorizontalSeparator marginTop="md" marginBottom="none" /></section>;
}
function ScenarioPreset({state,disabled,send}) {
  const options=[{value:'sandbox',label:'Live'},{value:'slowdown_surge',label:'Slowdown + traffic surge'},{value:'error_waves',label:'Recurring error storms'}];
  return <ScenarioControls state={state} disabled={disabled} send={send} options={options} description={state.scenario_description}/>;
}
function Phase({phase}) {return <StatusPill isSoft level={phaseLevel(phase)}>{phaseName(phase)}</StatusPill>;}
function Entity({name, gateway=false, color}) {
  const Icon = gateway ? GlobeIcon : ServerIcon;
  return <span className="entity"><span className="entity-icon" style={{background:color || '#54b49b'}}><Icon size="md" /></span><Text>{name}</Text></span>;
}
function Stat({label,value}) { return <span className="header-stat"><Text variant="secondary">{label}: </Text><Text weight="bold">{value}</Text></span>; }
function DataTable({data,columns,pagination=NO_PAGES,empty='No results.'}) {
  return <Table data={data} columns={columns} pagination={pagination} summary={TABLE_SUMMARY} rowHeight="md" emptyState={{title:empty,size:"sm",imagePath:null}} shouldResetOnDataChange={false} />;
}
function Transport({state,disabled,send}) {
  const ended = state.at_ms >= state.horizon_ms;
  const datadog = state.inference.evidence_source === 'datadog';
  return <div className="transport-bar">
    <div className="transport-group">
      <Button id="play" icon={state.paused ? PlayIcon : PauseIcon} isPrimary label={ended?'Complete':state.paused?(state.at_ms?'Resume':'Start traffic'):'Pause'} isTitleCased={false} isDisabled={disabled || ended} onClick={()=>send({type:state.paused?'play':'pause'})}/>
      <Button id="step" label="+1s" ariaLabel="Step one second" isDisabled={datadog || disabled || ended} onClick={()=>send({type:'step'})}/>
      <Text isMonospace size="sm">{time(state.at_ms)} / {time(state.horizon_ms)}</Text>
      <StatusPill isSoft level={!state.paused&&!ended?'success':'default'}>{ended?'Complete':state.replaying?'Replay':state.paused?'Paused':'Running'}</StatusPill>
    </div><div className="transport-group playback-actions">
      <ToggleButtons aria-label="Playback speed" options={[{value:1,label:"1×"},{value:2,label:"2×"},{value:4,label:"4×"}]} value={state.speed} isDisabled={datadog || disabled} onChange={value=>send({type:'speed',value})}/>
      <Button id="replay" label="Replay" isBorderless isDisabled={datadog || disabled || !state.at_ms} onClick={()=>send({type:'replay'})}/>
      <Button id="reset" label="Reset" isBorderless isDisabled={disabled} onClick={()=>send({type:'reset'})}/>
    </div>
  </div>;
}
function ServiceTable({state,onFocus}) {
  const [search,setSearch]=useState(''),[filter,setFilter]=useState('all');
  const data=useMemo(()=>state.services.map((s,i)=>({id:i,name:s.definition.name,phase:s.state.phase,queue:s.state.queued})).filter(s=>s.name.toLowerCase().includes(search.toLowerCase())&&(filter==='all'||s.phase===filter)),[state.services,search,filter]);
  const columns=useMemo(()=>[
    {Header:'Name',accessor:'name',width:'minmax(130px, 1fr)',Cell:({row,value})=><Button label={value} icon={ServerIcon} isPrimary isDangerouslyNaked isTitleCased={false} onClick={()=>onFocus(row.original.id)}/>},
    {Header:'Circuit',accessor:'phase',width:'95px',Cell:({value})=><Phase phase={value}/>},
    {Header:'Queued',accessor:'queue',width:'75px',type:'numeric'},
  ],[onFocus]);
  return <Section title="Downstream services"><div className="table-filters"><InputSearch ariaAttrs={{'aria-label':'Search services'}} placeholder="Search services" value={search} onChange={e=>setSearch(e.target.value)} isFullWidth/><div className="circuit-filter"><Select aria-label="Filter circuit state" value={filter} clearable={false} searchable={false} options={[{value:'all',label:'All circuits'},{value:'closed',label:'Closed'},{value:'open',label:'Open'},{value:'half_open',label:'Probe'}]} onChange={option=>setFilter(option.value)}/></div></div><DataTable data={data} columns={columns} empty="No matching services."/></Section>;
}
function RequestTable({state,focus,onInspect}) {
  const data=useMemo(()=>state.requests.filter(r=>focus===null||r.service===focus).slice(0,30),[state.requests,focus]);
  const columns=useMemo(()=>[
    {Header:'Request',accessor:'id',width:'100px',Cell:({value,row})=><Button label={`#${value}`} isPrimary isDangerouslyNaked ariaLabel={`Inspect request ${value}`} onClick={()=>onInspect(row.original)}/>},
    {Header:'Outcome',accessor:r=>outcomeName(r.outcome),id:'outcome',width:'minmax(100px, 1fr)'},
    {Header:'Work',accessor:'work_ms',width:'85px',type:'numeric',Cell:({value})=>`${fmt(value)} ms`},
  ],[onInspect]);
  return <Section title="Requests"><DataTable data={data} columns={columns} pagination={REQUEST_PAGES} empty="Start traffic to follow a request."/><Text size="xs" variant="secondary">Latest 30 requests{focus!==null?' for this service':''}. Select a request to inspect its lifecycle.</Text></Section>;
}
function Trend({state,focus}) {
  return <Section title="Queued requests and utilization"><PressureChart state={state} service={focus}/></Section>;
}

function Events({state,focus}) {
  const entries=[...state.transitions.filter(e=>focus===null||e.service===focus).map(e=>({at:e.at_ms,title:`${state.services[e.service].definition.name}: ${phaseName(e.from)} → ${phaseName(e.to)}`,detail:e.trigger})),...state.injections.filter(e=>focus===null||e.service===focus).map(e=>({at:e.at_ms,title:`${state.services[e.service].definition.name}: ${Object.values(e.faults).some(Boolean)?'faults updated':'faults repaired'}`,detail:faults.filter(([key])=>e.faults[key]).map(([,label])=>label).join(' · ')||'Existing work continues to drain.'}))].sort((a,b)=>b.at-a.at).slice(0,10);
  return <Section title="Incident log"><div className="incident-log">{entries.length?entries.map((e,i)=><div className="incident-event" key={`${e.at}-${i}`}><Text size="xs" isMonospace variant="secondary">{time(e.at)}</Text><div><Text as="div" size="sm" weight="bold">{e.title}</Text><Text size="xs" variant="secondary">{e.detail}</Text></div></div>):<Text size="sm" variant="secondary">No incidents yet. Start traffic and inject a fault to observe recovery.</Text>}</div></Section>;
}
function Decisions({state,focus,onInspect}) {
  const data=useMemo(()=>state.decisions.filter(d=>focus===null||d.service===focus).slice(0,20),[state.decisions,focus]);
  const columns=useMemo(()=>[
    {Header:'Time',accessor:'completed_at_ms',width:'80px',Cell:({value})=>time(value)},
    {Header:'Decision',id:'choice',accessor:d=>choiceName(d.inference.action),width:'minmax(100px, 1fr)',Cell:({value,row})=><Button label={value} isPrimary isDangerouslyNaked isTitleCased={false} onClick={()=>onInspect(row.original)}/>},
    {Header:'Guard',accessor:d=>d.guard.status,id:'guard',width:'90px'},
  ],[onInspect]);
  return <Section title="Jev decisions"><DataTable data={data} columns={columns} pagination={REQUEST_PAGES} empty="No judgments yet."/></Section>;
}
function Inference({state}) {
  const c=state.inference.cost,p=state.inference.pending;
  return <Section title="Jev activity">{state.inference.evidence_source==='datadog'&&<Text as="p" size="sm">Datadog evidence · {state.inference.telemetry_status}{state.inference.evidence_age_seconds==null?'':` · data ${fmt(state.inference.evidence_age_seconds)}s old`}</Text>}{(state.replaying||p)&&<Text as="p" size="sm">{state.replaying?'Replaying recorded judgments; no API calls.':`Evaluating ${state.services[p.service].definition.name}${state.paused?'; applies after resuming':''}.`}</Text>}<Stat label="Evaluations" value={fmt(state.inference.calls)}/>{c&&<details className="cost-detail"><summary>Estimated cost: ${Number(c.estimated_usd).toFixed(6)} · since server start</summary><Text as="p" size="sm">{fmt(c.input_tokens)} input tokens · {fmt(c.output_tokens)} output tokens · {fmt(c.priced_calls)} priced responses. {c.missing_usage_calls} calls without reported usage; {c.unpriced_calls} without a known rate.</Text><Text as="p" size="xs" variant="secondary">Jev 1.13 input: $0.042 per million tokens; output free. Excludes unreported and unpriced usage. Reset preserves this estimate; restarting clears it. This is not your final bill.</Text></details>}</Section>;
}
function Inspector({state,focus,onFocus,disabled,send,onRequest,onDecision}) {
  const [tab,setTab]=useState('overview');
  const service=focus===null?null:state.services[focus],counts=state.counts;
  const ended=state.at_ms>=state.horizon_ms;
  const activeFaults=service?Object.values(service.faults).filter(Boolean).length:0;
  const requests=state.requests.filter(r=>focus===null||r.service===focus);
  const activeIncidents=state.transitions.filter(t=>focus===null||t.service===focus).length+state.injections.filter(t=>focus===null||t.service===focus).length+(state.policy==='jev'?state.decisions.filter(d=>focus===null||d.service===focus).length:0);
  return <div className="discovery-panel" id="discovery-inspector">
    <header className="panel-heading" aria-label="Selected service summary">
      {service?<Button icon={HomeIcon} label="Back to gateway" isPrimary isDangerouslyNaked isTitleCased={false} size="sm" onClick={()=>onFocus(null)}/>:<Text size="sm" variant="secondary" weight="bold">Circuit breaker overview</Text>}
      <div className="entity-heading"><Entity gateway={!service} name={service?.definition.name || 'gateway'} color={service?colors[focus]:undefined}/>{service?<Phase phase={service.state.phase}/>:<StatusPill isSoft level="success">Protected by Reflex</StatusPill>}</div>
      <Text size="sm" variant="secondary">{service?descriptions[focus]:'Routes client traffic through independent service circuits.'}</Text>
      <div className="header-stats">{service?<><Stat label="Workers" value={`${service.state.active} / ${service.definition.workers}`}/><Stat label="Queued" value={fmt(service.state.queued)}/><Stat label="Utilization" value={`${fmt(service.state.stress*100)}%`}/></>:<><Stat label="Requests" value={fmt(counts.offered)}/><Stat label="Services" value={state.services.length}/><Stat label="Circuits" value={state.services.length}/></>}</div>
    </header>
    <div className="inspector-tabs">
      <SoftToggle ariaLabel="Inspector sections" value={tab} onChange={setTab} isFullWidth hasEqualWidthOptions options={[
        {label:'Overview',value:'overview'},
        {label:'Requests',value:'requests',statusLabel:String(Math.min(30,requests.length))},
        {label:'Activity',value:'activity',statusLabel:String(activeIncidents)},
      ]}/>
    </div>
    <div key={tab} className="panel-scroll">
      <div className="panel-sections" id="inspector-content" role="region" aria-label={`${tab} for ${service?.definition.name || 'gateway'}`}>
        {tab==='overview'&&<>
          {!service&&<Section title="Request outcomes"><div className="outcome-grid">{[['Useful requests',counts.success,'success'],['Errors + timeouts',counts.error+counts.timeout,counts.error+counts.timeout?'danger':'default'],['Circuit-shed',counts.shed,counts.shed?'warning':'default'],['In flight',counts.offered-counts.success-counts.error-counts.timeout-counts.shed,'default']].map(([label,value,level])=><CalloutValue className="outcome-value" key={label} label={label} value={fmt(value)} size="sm" level={level}/>)}</div></Section>}
          {service?<Section title="Fault injection" description="Apply a fault to this service and observe how its circuit responds." action={<Button id="repair" label="Repair all" isPrimary isDangerouslyNaked isTitleCased={false} size="sm" isDisabled={disabled||ended||state.replaying} onClick={()=>send({type:'repair'})}/>}>
            <div className="fault-controls">{faults.map(([key,label,detail])=><div className={`fault-row ${service.faults[key]?'fault-active':''}`} key={key}><div><Text as="div" size="sm" weight="bold">{label}</Text><Text size="xs" variant="secondary">{detail}</Text></div><ToggleSwitch id={`fault-${key}`} ariaLabel={label} isChecked={service.faults[key]} isDisabled={disabled||ended||state.replaying} onChange={()=>send({type:'fault',service:focus,faults:{...service.faults,[key]:!service.faults[key]}})}/></div>)}</div>
            {activeFaults>0&&<Text as="p" size="xs" variant="secondary">{activeFaults} active {activeFaults===1?'fault':'faults'} · Repair stops faults; queued work still drains.</Text>}
            {service.timed_out_work>0&&<MessageBox level="warning"><Text size="sm">{service.timed_out_work} timed-out requests still consume downstream capacity.</Text></MessageBox>}
          </Section>:<ServiceTable state={state} onFocus={onFocus}/>}
          {service&&<Trend state={state} focus={focus}/>}
          {state.policy==='jev'&&<Inference state={state}/>}

</>}
        {tab==='requests'&&<RequestTable state={state} focus={focus} onInspect={onRequest}/>}
        {tab==='activity'&&<><Events state={state} focus={focus}/>{state.policy==='jev'&&<Decisions state={state} focus={focus} onInspect={onDecision}/>}</>}
      </div>
    </div>
  </div>;
}

function RequestDetails({request:r,state,at}) {
  const pairs=[['Service',state.services[r.service].definition.name],['Arrival',`${(r.arrived_ms/1000).toFixed(3)}s`],['Client outcome',outcomeName(r.outcome)],['Client latency',`${fmt((r.client_finished_ms??at)-r.arrived_ms)} ms`],['Downstream',r.outcome==='shed'?'Never admitted':r.downstream_finished_ms!=null?'Finished':r.started_ms!=null?'Still running':'Still queued'],['Worker time',`${fmt(r.work_ms)} ms`],['Work after timeout',`${fmt(r.post_timeout_work_ms)} ms`],['Recovery probe',r.probe?'Yes':'No']];
  return <><Text variant="secondary" size="sm">Snapshot at inspection · {time(at)}</Text><dl className="request-details">{pairs.map(([label,value])=><React.Fragment key={label}><dt>{label}</dt><dd>{value}</dd></React.Fragment>)}</dl><Text>{r.cause||'The client is still waiting for a response.'}</Text></>;
}
function App({state,disabled=true,connected=false,error,send,navigate}) {
  const [focus,setFocus]=useState(null),[panelOpen,setPanelOpen]=useState(true),[modal,setModal]=useState(null);
  const onFocus=useCallback(id=>{setFocus(id);setPanelOpen(true);},[]);
  const onRequest=useCallback(request=>setModal({type:'request',request,at:state.at_ms}),[state?.at_ms]);
  const onDecision=useCallback(decision=>setModal({type:'decision',decision}),[]);
  useEffect(()=>renderHeader({disabled,navigate}),[disabled,navigate]);
  return <DruidsEnvironment defaultThemePreference="light"><div className="discovery-app">
    {error&&<div className="error-banner" role="alert">{error}</div>}
    {state?<><section className="simulation-intro"><div className="simulation-intro-copy"><h1 className="simulation-title">Circuit Breaker</h1><details className="circuit-description"><summary><span dangerouslySetInnerHTML={{__html:helpIntroHtml}}/><span className="description-toggle"><span className="description-more">Read more</span><span className="description-less">Show less</span></span></summary><div className="circuit-description-copy" dangerouslySetInnerHTML={{__html:helpDetailsHtml}}/></details></div><ScenarioPreset state={state} disabled={disabled} send={send}/></section><div className="discovery-layout"><Topology state={state} focus={focus} onFocus={onFocus} panelOpen={panelOpen} setPanelOpen={setPanelOpen} playback={<Transport state={state} disabled={disabled} send={send}/>}/>{panelOpen&&<Inspector key={focus??"gateway"} state={state} focus={focus} onFocus={onFocus} disabled={disabled} send={send} onRequest={onRequest} onDecision={onDecision}/>}</div><footer className="app-footer"><Text size="xs" variant="secondary">Reflex</Text><Text size="xs" variant={connected?'success':'danger'}>{connected?'● Engine connected':'Engine disconnected'}</Text></footer></>:<div className="loading-state"><Text>Connecting to the Reflex engine…</Text></div>}
    <Modal isOpen={!!modal} onClose={()=>setModal(null)} title={modal?.type==='request'?`Request #${modal.request.id}`:'Jev decision'} size="md" isScrollable>
      {modal?.type==='request'&&state&&<RequestDetails request={modal.request} at={modal.at} state={state}/>}
      {modal?.type==='decision'&&<div className="decision-detail"><Text as="p">{modal.decision.guard.status} · {modal.decision.guard.reason}</Text><Text as="h3" weight="bold">{modal.decision.inference.error?.code==='datadog_evidence'?'Telemetry unavailable · Jev was not called':'Evidence sent to Jev'}</Text><pre>{JSON.stringify(modal.decision.model_input||modal.decision.evidence,null,2)}</pre><Text as="h3" weight="bold">Provider result</Text><pre>{JSON.stringify(modal.decision.inference,null,2)}</pre></div>}
    </Modal>
  </div></DruidsEnvironment>;
}
window.renderReflexControls = props => root.render(<App {...props}/>);
window.renderReflexControls({});
