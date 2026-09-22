import React from 'react';
import {Button} from '@datadog/druids/form/Button';
import {Text} from '@datadog/druids/typography/Text';

export function ScenarioControls({state,disabled,send,options,description}) {
  const descriptions={
    sandbox:'Start traffic and adjust the simulation yourself. No scheduled changes.',
    slowdown_surge:'Payments slows down and traffic surges. Watch the circuit open, probe, and recover.',
    error_waves:'Search encounters recurring error storms. Watch recovery between waves.',
    traffic_burst:'Client traffic surges, then eases. Watch the shared queue build and drain.',
    resource_mix:'Jobs shift toward CPU-heavy and memory-heavy workloads, then return to their original sizes.',
    single_crash:'Replica A crashes, then restarts. Watch replacement and readiness checks.',
    rebuild_pressure:'Traffic and request costs surge while a replica rebuilds, then recover.',
    second_failure:'Replica A crashes, followed by B. Watch recovery as both replicas restart.',
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
