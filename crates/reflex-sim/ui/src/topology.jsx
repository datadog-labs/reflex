import React from 'react';
import {Text} from '@datadog/druids/typography/Text';
import {FlowMap} from './flow-map.jsx';

const phaseName=p=>({closed:'Closed',open:'Open',half_open:'Probe',bypassed:'Bypassed'}[p]||p);
const level=p=>({closed:'success',open:'danger',half_open:'warning'}[p]||'default');
const fmt=n=>Number(n).toLocaleString('en-US',{maximumFractionDigits:0});
export function Topology({state,focus,onFocus,panelOpen,setPanelOpen,playback}) {
  const nodes=[],edges=[];
  state.services.forEach((s,i)=>{
    nodes.push({key:`client-${i}`,column:0,kind:'client',name:`${s.definition.name} clients`,subtext:`${fmt(s.state.offered_rate)} req/s`});
    nodes.push({key:`service-${i}`,column:2,kind:'service',name:s.definition.name,selected:focus===i,phase:phaseName(s.state.phase),status:level(s.state.phase),metrics:[`${fmt(s.state.queued)} queued`,`${fmt(s.state.stress*100)}% utilization`,`${fmt(s.state.offered_rate)} req/s`],ratio:s.state.stress,metricLevels:['default',s.state.stress>.65?'danger':s.state.stress>.3?'warning':'default','default'],annotation:Object.values(s.faults).some(Boolean)?'Fault injected':undefined});
    edges.push({key:`in-${i}`,sourceId:`client-${i}`,targetId:'gateway',active:s.state.offered_rate>0},{key:`out-${i}`,sourceId:'gateway',targetId:`service-${i}`,active:s.state.offered_rate>0&&s.state.phase!=='open',status:s.state.phase==='open'?'danger':s.state.phase==='half_open'?'warning':'default',count:s.state.phase==='half_open'?1:3});
  });
  nodes.push({key:'gateway',column:1,kind:'hub',name:'gateway',subtext:'Protected by Reflex',selected:focus===null,metrics:[`${fmt(state.services.reduce((n,s)=>n+s.state.offered_rate,0))} req/s`,`${state.services.length} guarded circuits`]});
  return <div className="topology-canvas" aria-label="Interactive request topology"><div className="map-playback" role="region" aria-label="Simulation playback">{playback}</div>
    <FlowMap nodes={nodes} edges={edges} running={!state.paused&&state.at_ms<state.horizon_ms} speed={state.speed} panelOpen={panelOpen} setPanelOpen={setPanelOpen} onSelect={key=>{if(key==='gateway')onFocus(null);else if(key.startsWith('service-'))onFocus(Number(key.slice(8)));}}/>
    <div className="canvas-footer"><div className="map-legend"><span><i className="status-closed"/>Closed</span><span><i className="status-half_open"/>Probe</span><span><i className="status-open"/>Open</span></div><Text size="xs" variant="secondary">Drag to pan · select a service to inspect</Text></div>
  </div>;
}
