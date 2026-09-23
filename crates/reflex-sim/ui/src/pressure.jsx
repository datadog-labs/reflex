// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

import React from 'react';
import {Text} from '@datadog/druids/typography/Text';
import {TimeChart} from './time-chart.jsx';
export function PressureChart({state,service}) {
  const start=Math.max(0,state.at_ms-30000),end=Math.max(30000,state.at_ms);
  const history=state.history.filter(p=>p.service===service&&p.at_ms>=start);
  const series=[{name:'Queue',color:'purple',points:history.map(p=>[p.at_ms,p.queued])},{name:'Stress (%)',color:'orange',right:true,points:history.map(p=>[p.at_ms,p.stress*100])}];
  return <div className="pressure-chart"><TimeChart series={series} start={start} end={end} leftInteger leftLabel="Queued requests" rightLabel="Stress (%)" rightMax={100} label="Queue and stress over the last 30 simulated seconds"/><Text size="xs" variant="secondary">Simulation clock · last 30 simulated seconds. Queue: left axis; stress: right axis.</Text></div>;
}
