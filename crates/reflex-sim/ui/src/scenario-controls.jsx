import React from 'react';
import {Button} from '@datadog/druids/form/Button';
import {Select} from '@datadog/druids/form/Select';
import {Text} from '@datadog/druids/typography/Text';
import {PlayIcon} from '@datadog/druids/icons/Play';

export function ScenarioControls({state,disabled,send,options,description}) {
  const hint=state.scenario==='sandbox'?'Hit Start traffic and inject your own faults':description;
  return <section className="simulation-scenario-bar" aria-label="Simulation scenario">
    <div className="simulation-scenario-controls">
      <Text size="sm" weight="bold">Scenario</Text>
      <div className="simulation-scenario-picker"><Select aria-label="Scenario" isFullWidth options={options} value={state.scenario} disabled={disabled} searchable={false} clearable={false} onChange={option=>send({type:'scenario',scenario:option.value},true)}/></div>
      <Button label="Run scenario" isTitleCased={false} icon={PlayIcon} isDisabled={disabled||state.scenario==='sandbox'} onClick={async()=>{if(await send({type:'scenario',scenario:state.scenario},true))await send({type:'play'});}}/>
    </div>
    <div className="simulation-scenario-copy">
      {hint&&<Text size="sm" variant="secondary">{hint}</Text>}
    </div>
  </section>;
}
