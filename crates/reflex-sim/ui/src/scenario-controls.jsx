// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

import React, {useState} from 'react';
import {PanelLeftShrinkIcon} from '@datadog/druids/icons/PanelLeftShrink';
import {Button} from '@datadog/druids/form/Button';
import {Text} from '@datadog/druids/typography/Text';

export function ScenarioControls({state,disabled,send,options,description}) {
  const [open,setOpen]=useState(true);
  const descriptions={
    sandbox:'Adjust the simulation yourself. No scheduled changes.',
    slowdown_surge:'Payments slows down and traffic surges. The circuit opens, probes, recovers.',
    cyclic_pressure:'Repeating traffic and slowdown cycles. Toto learns the pattern; Jev controls the circuit.',
    cyclical:'A steady client joined by a broad peak and a short burst, every minute.',
    traffic_burst:'Client traffic surges, then eases. Watch the shared queue build and drain.',
    resource_mix:'Jobs shift toward CPU-heavy and memory-heavy workloads, then return to their original sizes.',
  };
  const active=options.find(option=>option.value===state.scenario);
  return <div className="scenario-floating">
    {!open?<button type="button" className="scenario-collapsed" aria-label={`Show scenarios: ${active?.label||'Live'}`} aria-expanded={false} onClick={()=>setOpen(true)}>
      <svg viewBox="0 0 24 24" width="13" height="13" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"><rect x="3" y="3" width="18" height="18" rx="2"/><path d="M9 3v18m5-13 4 4-4 4"/></svg>
      <span className="scenario-collapsed-label">Scenario</span><span className="scenario-collapsed-value">{active?.label||'Live'}</span>
    </button>:
    <section className="scenario-card-panel" aria-label="Simulation scenario">
      <div className="scenario-card-heading"><span className="eyebrow">Scenarios</span><Button isBorderless size="sm" icon={PanelLeftShrinkIcon} ariaLabel="Collapse scenarios" onClick={()=>setOpen(false)}/></div>
      <div className="scenario-card-grid" role="group" aria-label="Available scenarios">
      {options.map(option=><button type="button" key={option.value} className="scenario-choice-card" data-selected={state.scenario===option.value} aria-pressed={state.scenario===option.value} aria-label={`Run ${option.label}`} disabled={disabled} onClick={async()=>{if(await send({type:'scenario',scenario:option.value},true))await send({type:'play'});}}>
        <span className="scenario-choice-title"><Text weight="bold">{option.label}</Text><span className="scenario-choice-action">{state.scenario===option.value?(!state.paused?'Running':'Selected'):'Run'}</span></span>
        <Text className="scenario-choice-description" variant="secondary">{descriptions[option.value]||option.description}</Text>
      </button>)}
      </div><div className="scenario-card-footer">Real Rust simulation. Animation samples traffic; counters come from the engine.</div>
    </section>}
  </div>;
}
