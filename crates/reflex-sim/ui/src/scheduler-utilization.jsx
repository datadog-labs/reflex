import React from 'react';
import {Text} from '@datadog/druids/typography/Text';
import {TimeChart} from './time-chart.jsx';

export function SchedulerUtilizationCharts({state,node=null}) {
  const start=Math.max(0,state.at_ms-60000),end=Math.max(60000,state.at_ms);
  const samples=[...state.history.filter(p=>p.at_ms>=start&&p.at_ms<state.at_ms),{at_ms:state.at_ms,nodes:state.nodes}];
  const scope=node==null?'Node pool':state.nodes[node]?.name;
  if(!scope)return null;
  const utilization=(sample,used,total)=>{
    if(!sample.nodes)return null;
    const nodes=node==null?sample.nodes:[sample.nodes[node]].filter(Boolean);
    const capacity=nodes.reduce((sum,n)=>sum+n[total],0);
    return capacity>0?nodes.reduce((sum,n)=>sum+n[used],0)/capacity*100:null;
  };
  return <section className="scheduler-utilization" aria-label={`${scope} resource utilization`}>
    <Text as="h2" weight="bold" size="lg">{scope} utilization</Text>
    {[['CPU','used_cpu','cpu','purple'],['Memory','used_memory_gib','memory_gib','orange']].map(([name,used,total,color])=><div key={name}>
      <Text as="h3" weight="bold" size="sm">{name} utilization</Text>
      <TimeChart series={[{name:`${name} utilization`,unit:'%',color,points:samples.map(p=>[p.at_ms,utilization(p,used,total)])}]} start={start} end={end} leftLabel="Utilization (%)" leftMax={100} label={`${scope} ${name.toLowerCase()} utilization over the last 60 simulated seconds`}/>
    </div>)}
    <Text as="p" size="xs" variant="secondary">Last 60 simulated seconds · reserved resources / capacity{node==null?' across all nodes':''}.</Text>
  </section>;
}
