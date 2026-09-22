import React from 'react';
import {Select} from '@datadog/druids/form/Select';
import {CalloutValue} from '@datadog/druids/measures/CalloutValue';
import {Text} from '@datadog/druids/typography/Text';
import {StatusPill} from '@datadog/druids/pills/StatusPill';
const number=value=>Number(value).toLocaleString('en-US');
const metrics=[
 {key:'completed',label:'Completed',format:number},
 {key:'mean_wait_ms',label:'Mean wait',format:value=>(value/1000).toFixed(1),unit:'s'},
 {key:'rejected',label:'Rejected',format:number},
 {key:'node_seconds',label:'Node-seconds',format:number},
];
function difference(value,baseline,metric,isBaseline){
 if(isBaseline)return 'Baseline';
 const delta=value-baseline;
 if(delta===0)return 'Same as baseline';
 const sign=delta>0?'+':'−';
 if(baseline===0)return `${sign}${metric.format(Math.abs(delta))}${metric.unit||''} vs baseline`;
 const percent=Math.abs(delta/baseline)*100;
 return `${sign}${percent<0.1?'<0.1':percent.toFixed(1)}% vs baseline`;
}
function policyStatus(state,id){
 if(id===0)return {label:'Reactive baseline',level:'default'};
 if(state.forecast_fresh&&state.forecast)return {label:id===2&&!state.jev_available?'Jev unavailable':'Forecast ready',level:id===2&&!state.jev_available?'warning':'success'};
 if(state.at_ms>0)return {label:'Reactive fallback',level:'warning'};
 return {label:state.forecast_pending?'Forecasting':'Waiting for forecast',level:'default'};
}
export function PolicySwitcher({state,selected,onSelectPolicy}){
 const lane=state.lanes.find(l=>l.id===selected.lane)||state.lanes[0];
 if(!lane)return null;
 return <div className="scenario-policy policy-dropdown"><Select isFullWidth aria-label="Policy" value={lane.id} onChange={option=>onSelectPolicy(option.value)} searchable={false} clearable={false} options={state.lanes.map(l=>({label:l.label,value:l.id}))}/></div>;
}
export function PolicyComparison({state,selected}){
 const lane=state.lanes.find(l=>l.id===selected.lane)||state.lanes[0];
 const baseline=state.lanes.find(l=>l.id===0);
 if(!lane||!baseline)return null;
 const status=policyStatus(state,lane.id);
 return <section className="policy-comparison" aria-label="Policy comparison">
  <div className="policy-metrics" data-policy-id={lane.id}>
   {metrics.map(metric=><CalloutValue key={metric.key} label={metric.label} value={metric.format(lane[metric.key])} unit={metric.unit} size="sm" isBorderless additionalText={difference(lane[metric.key],baseline[metric.key],metric,lane.id===0)}/>)}
  </div>
  <div className="policy-comparison-note"><StatusPill isSoft level={status.level}>{status.label}</StatusPill><Text size="xs" variant="secondary">Compared with Reactive thresholds. Mean wait includes started jobs.</Text></div>
 </section>;
}
