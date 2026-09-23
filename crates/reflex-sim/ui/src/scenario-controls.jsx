// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

import React from 'react';
import {Button} from '@datadog/druids/form/Button';
import {Text} from '@datadog/druids/typography/Text';

export function ScenarioControls({state,disabled,send,options,description}) {
  const descriptions={
    sandbox:'Start traffic and adjust the simulation yourself. No scheduled changes.',
    slowdown_surge:'Payments slows down and traffic surges. Watch the circuit open, probe, and recover.',
    cyclic_pressure:'Payments repeats traffic and slowdown cycles, then recovers. Toto learns the pattern; Jev controls the circuit. Forecasting starts after enough observations have been collected. Runs 10 min.',
    cyclical:'Every minute, a steady client is joined by a broad peak and a short burst. Watch queues build and drain, then compare Actual vs Toto in Trends. Forecasts use history collected during the run.',
    traffic_burst:'Client traffic surges, then eases. Watch the shared queue build and drain.',
    resource_mix:'Jobs shift toward CPU-heavy and memory-heavy workloads, then return to their original sizes.',
  };
  return <section className="simulation-scenario-bar scenario-card-panel" aria-label="Simulation scenario">
    <div className="scenario-card-grid" style={{'--scenario-count':options.length}} role="group" aria-label="Available scenarios">
      {options.map(option=><div key={option.value} className="scenario-choice-card" data-selected={state.scenario===option.value}>

          <button type="button" className="scenario-choice-copy" aria-pressed={state.scenario===option.value} disabled={disabled} title={descriptions[option.value]||option.description} onClick={()=>{if(state.scenario!==option.value)send({type:'scenario',scenario:option.value},true);}}><Text size="sm" weight="bold">{option.label}</Text><Text size="sm" variant="secondary">{descriptions[option.value]||option.description}</Text></button>
        <Button label="Run" ariaLabel={`Run ${option.label}`} size="sm" isTitleCased={false} isDisabled={disabled} onClick={async()=>{if(await send({type:'scenario',scenario:option.value},true))await send({type:'play'});}}/>
      </div>)}
    </div>
  </section>;
}
