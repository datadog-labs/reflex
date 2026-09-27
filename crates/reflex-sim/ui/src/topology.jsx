// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

import React from 'react';
import {FlowMap} from './flow-map.jsx';

const phaseName=p=>({closed:'Closed',open:'Open',half_open:'Probe',bypassed:'Bypassed'}[p]||p);
const level=p=>({closed:'success',open:'danger',half_open:'warning'}[p]||'default');
const fmt=n=>Number(n).toLocaleString('en-US',{maximumFractionDigits:0});
export function Topology({state,focus,onFocus,playback,scenarios}) {
  const nodes=[],edges=[];
  state.services.forEach((s,i)=>{
    nodes.push({key:`client-${i}`,column:0,kind:'client',selectable:false,name:`${s.definition.name} clients`,subtext:`${fmt(s.state.offered_rate)} req/s`});
    nodes.push({key:`service-${i}`,column:2,kind:'service',name:s.definition.name,selected:focus===i,subtext:`worker pool · ${s.definition.workers}`,phase:phaseName(s.state.phase),status:level(s.state.phase),metrics:[{label:"Queued",value:fmt(s.state.queued)},{label:"Util",value:`${fmt(s.state.stress*100)}%`},{label:"Req/s",value:fmt(s.state.offered_rate)}],ratio:s.state.stress,metricLevels:['default',s.state.stress>.65?'danger':s.state.stress>.3?'warning':'default','default'],annotation:Object.values(s.faults).some(Boolean)?'Fault injected':undefined});
    edges.push({key:`in-${i}`,sourceId:`client-${i}`,targetId:'gateway',active:s.state.offered_rate>0},{key:`out-${i}`,sourceId:'gateway',targetId:`service-${i}`,active:s.state.offered_rate>0&&s.state.phase!=='open',status:s.state.phase==='open'?'danger':s.state.phase==='half_open'?'warning':'default',count:s.state.phase==='half_open'?1:3});
  });
  nodes.push({key:'gateway',column:1,kind:'hub',name:'gateway',subtext:`${fmt(state.services.reduce((n,s)=>n+s.state.offered_rate,0))} req/s in`,circuits:state.services.map(s=>({name:s.definition.name,phase:phaseName(s.state.phase),status:level(s.state.phase)}))});
  return <div className="topology-canvas" aria-label="Interactive request topology">{scenarios}<div className="map-playback" role="region" aria-label="Simulation playback">{playback}</div>
    <FlowMap nodes={nodes} edges={edges} running={!state.paused&&state.at_ms<state.horizon_ms} speed={state.speed} onSelect={key=>{if(key==='gateway')onFocus(null);else if(key.startsWith('service-'))onFocus(Number(key.slice(8)));}}/>

  </div>;
}
