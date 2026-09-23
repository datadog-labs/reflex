// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

import React from 'react';
import {TimeChart} from './time-chart.jsx';

const colors=['#7f9d61','#b7865a','#79a4a7','#aa84aa','#bf927b','#738fbd','#aaa055','#849585'];
export function SchedulerLagChart({state,field,label,hiddenClients=[]}) {
  const start=Math.max(0,state.at_ms-60000),end=Math.max(60000,state.at_ms),hidden=new Set(hiddenClients);
  const samples=[...state.history.filter(p=>p.at_ms>=start&&p.at_ms<state.at_ms),{at_ms:state.at_ms,clients:state.live_lag||[]}];
  const ids=[...new Set(samples.flatMap(p=>(p.clients||[]).map(c=>c.client)))].filter(id=>!hidden.has(id)).sort((a,b)=>a-b);
  const series=ids.map(id=>({name:`Client ${id+1}`,color:colors[id%colors.length],unit:'s',points:samples.map(p=>{
    const value=(p.clients||[]).find(c=>c.client===id)?.[field];
    return [p.at_ms,value==null?null:value/1000];
  })}));
  const markers=(state.priority_changes||[]).filter(e=>!hidden.has(e.client)).map(e=>({at:e.at_ms,color:colors[e.client%colors.length],label:`Client ${e.client+1} → ${e.priority} priority`}));
  return <TimeChart series={series} markers={markers} start={start} end={end} leftLabel="Seconds" label={label}/>;
}
