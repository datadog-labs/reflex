// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

import React, {useState} from 'react';
import {createRoot} from 'react-dom/client';
import {DruidsEnvironment} from '@datadog/druids/layout/DruidsEnvironment';
import {Text} from '@datadog/druids/typography/Text';
import {HorizontalSeparator} from '@datadog/druids/layout/HorizontalSeparator';
import {TimeChart, simulationTime} from './time-chart.jsx';

const number=v=>Number(v).toLocaleString('en-US',{maximumFractionDigits:2});
const relativeTime=ms=>`+${simulationTime(ms)}`;
const titleFor=name=>name.replaceAll('_',' ').replace(/^./,c=>c.toUpperCase()).replace(/\bcpu\b/gi,'CPU').replace(/\bgib\b/gi,'GiB');
function ForecastChart({name,points=[],horizon=120000,now}) {
  const percent=name==='utilization',factor=percent?100:1;
  const label=percent?'Utilization (%)':titleFor(name);
  const value=v=>v==null?null:v*factor;
  const series=[
    {name:'Actual',color:'purple',unit:percent?'%':undefined,points:points.map(p=>[p.at,value(p.actual)])},
    {name:'Toto forecast (p50)',color:'orange',dashed:true,unit:percent?'%':undefined,points:points.map(p=>[p.at,value(p.p50)])},
  ];
  const bands=points.length?[{name:'Toto p10–p90',color:'orange',points:points.map(p=>[p.at,value(p.p10),value(p.p90)])}]:[];
  const markers=now>=0&&now<=horizon?[{at:now,label:'Now'}]:[];
  return <div className="forecast-chart">
    <Text as="h3" weight="bold" size="lg">{label}</Text>
    <TimeChart series={series} bands={bands} markers={markers} start={0} end={horizon} leftLabel={label} leftInteger={name.endsWith('queue_depth')||name==='active_requests'||name==='outstanding_requests'} label={`${label}: actual values versus Toto forecast`} timeFormat={relativeTime} emptyMessage="Waiting for Toto’s first forecast."/>
    <HorizontalSeparator marginTop="md" marginBottom="none"/>
  </div>;
}
export function ForecastPanel({view,title,policy,disabled=false,onToggle}) {
  const [origin,setOrigin]=useState(null);
  if(!view?.configured)return null;
  const runs=view.comparisons||[],comparison=runs.find(r=>r.origin_ms===origin)||runs[0];
  const remote=view.source==='datadog_observations',forecast=view.forecast;
  const originLabel=at=>remote?new Date(1780000000000+at).toLocaleTimeString():`${number(at/1000)}s`;
  const names=(view.series_names||[]).filter(Boolean);
  return <>
    <div className="forecast-heading"><div><Text as="h2" size="lg" weight="bold">Actual vs Toto · {title}</Text></div>
      <label className="forecast-toggle"><input type="checkbox" role="switch" id="forecast-toggle" aria-label="Toto forecasting" checked={view.enabled} disabled={disabled} onChange={e=>onToggle?.(e.target.checked)}/> Toto forecasting <strong>{view.enabled?'On':'Off'}</strong></label>
    </div>
    <Text as="p" size="xs" variant="secondary" className="forecast-status">{view.status||'Collecting history'}</Text>
    {view.enabled?<>
      {comparison&&<div className="forecast-comparison-controls"><label>Compare forecast from <select id="forecast-origin" aria-label="Forecast to compare" value={comparison.origin_ms} onChange={e=>setOrigin(Number(e.target.value))}>{runs.map(r=><option key={r.origin_ms} value={r.origin_ms}>{originLabel(r.origin_ms)}</option>)}</select></label><Text size="xs" variant="secondary">Available {number((comparison.available_at_ms-comparison.origin_ms)/1000)}s after origin · {remote?'Datadog observations':'Simulator observations'}</Text></div>}
      <div className="forecast-charts">
        {comparison?comparison.series.map(series=><ForecastChart key={series.name} name={series.name} horizon={comparison.horizon_ms} now={view.now_ms-comparison.origin_ms} points={series.points.map(p=>({...p,at:(p.from_ms+p.through_ms)/2-comparison.origin_ms}))}/>):names.map(name=><ForecastChart key={name} name={name}/>)}
      </div>
      <Text as="p" size="xs" variant="secondary">{comparison?`Time is relative to the selected forecast’s origin. Actuals appear only after a complete matching ten-second bucket is observed${remote?' in Datadog; telemetry arrives with a delay':''}. The selected forecast stays fixed as new observations arrive.`:`The comparison appears automatically after Toto produces its first forecast. This run needs ${view.minimum_history_seconds||64} ${remote?'wall-clock seconds plus ingestion delay':'simulated seconds'} of observed history.`}</Text>
      {forecast&&<details><summary>Current Jev forecast evidence</summary><Text as="p" size="xs" variant="secondary">{forecast.history_seconds}s history · {forecast.horizon_seconds}s horizon · {number(forecast.age_ms/1000)}s old. {policy==='jev'?'Fresh forecasts accompany Jev evaluations.':'The deterministic policy does not use forecasts.'}</Text><p>{forecast.model_provenance}</p><pre>{JSON.stringify(forecast,null,2)}</pre></details>}
    </>:<Text as="p" size="xs" variant="secondary">Toto calls are off. Jev uses observed state only. Switching this on does not reset the simulation.</Text>}
    <Text as="p" size="xs" variant="secondary" className="forecast-note">Forecasts never override Reflex guards. Both lines use matching bucket means; the band averages pointwise quantiles. {view.calls||0} / {view.limit||60} refresh attempts this run.</Text>
  </>;
}

// Scheduler keeps its existing DOM hosts and control handlers.
const roots=new WeakMap();
if(typeof window!=='undefined')window.renderForecast=(view,title,policy,disabled=false)=>{
  const panel=document.getElementById('forecast-panel');
  if(!panel)return;
  panel.hidden=!view?.configured;
  if(!view?.configured)return;
  if(!roots.has(panel))roots.set(panel,createRoot(panel));
  roots.get(panel).render(<DruidsEnvironment defaultThemePreference="light"><ForecastPanel view={view} title={title} policy={policy} disabled={disabled}/></DruidsEnvironment>);
};
