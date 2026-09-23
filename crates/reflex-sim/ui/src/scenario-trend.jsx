// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

import React from 'react';
import {TimeChart} from './time-chart.jsx';
export function ScenarioTrend({scenario,state,selected}){
  const start=Math.max(0,state.at_ms-(scenario==='capacity'?180000:60000)),end=scenario==='capacity'?Math.max(120000,state.at_ms+120000):Math.max(60000,state.at_ms);
  let series=[];
  if(scenario==='scheduler')series=[{name:'Running',unit:'jobs',points:state.history.map(p=>[p.at_ms,p.running]),color:'purple'},{name:'Queued',unit:'jobs',points:state.history.map(p=>[p.at_ms,p.queued]),color:'orange'}];
  if(scenario==='capacity'){
   const h=state.history.filter(p=>p.at_ms>=start),lane=selected.lane||0;
   series=[{name:'Offered CPU · 10s average',color:'orange',points:h.map((p,i)=>[p.at_ms,h.slice(Math.max(0,i-9),i+1).reduce((n,p)=>n+p.values[1],0)/Math.min(i+1,10)])},{name:'Ready CPU',color:'purple',points:state.trend.map(p=>[p.at_ms,p.ready_cpu[lane]])}];
   if(state.forecast){const f=state.forecast;for(const [key,name] of [['median','Toto median'],['lower','Toto p10'],['upper','Toto p90']])series.push({name:state.forecast_fresh?name:`${name} (expired)`,color:'blue',dashed:true,points:f.series[1][key].map((v,i)=>[f.origin_ms+(i+1)*1000,Math.max(0,v)]).filter(([at])=>at>=state.at_ms)});}
  }
  return <TimeChart series={series} start={start} end={end} leftLabel={scenario==='capacity'?'CPU':'Jobs'} leftInteger={scenario==='scheduler'} rightInteger rightMax={4} label={`${scenario} simulation trends`}/>;
}
