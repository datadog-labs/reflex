// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

import React from 'react';
import {Text} from '@datadog/druids/typography/Text';
import {TimeChart} from './time-chart.jsx';
export function PressureChart({state,service}) {
  const start=Math.max(0,state.at_ms-30000),end=Math.max(30000,state.at_ms);
  const history=state.history.filter(p=>p.service===service&&p.at_ms>=start);
  const workers=state.services[service].definition.workers;
  const series=[{name:'Queued requests',color:'purple',points:history.map(p=>[p.at_ms,p.queued])},{name:'Utilization (%)',color:'orange',right:true,points:history.map(p=>[p.at_ms,p.active/workers*100])}];
  return <div className="pressure-chart"><TimeChart series={series} start={start} end={end} leftInteger leftLabel="Queued requests" rightLabel="Utilization (%)" rightMax={100} label="Queued requests and worker utilization over the last 30 simulated seconds"/><Text size="xs" variant="secondary">Last 30 simulated seconds · queued requests: left axis; utilization: right axis.</Text></div>;
}
