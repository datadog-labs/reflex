// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

import React from 'react';
import {Text} from '@datadog/druids/typography/Text';

export function DatadogState({state}) {
  const evidence = state.inference || state;
  if (evidence.evidence_source !== 'datadog') return null;
  return <section className="datadog-state" aria-label="Datadog telemetry state">
    <Text as="h2" size="sm" weight="bold">Applications → Datadog metrics → Jev → Reflex</Text>
    <Text as="p" size="sm">{evidence.telemetry_status || 'Waiting for Datadog observations'}</Text>
    <details><summary>How telemetry becomes state · run <code>{evidence.simulation_run}</code></summary>
      <p>Applications export metrics every 10 seconds. The playground queries this run’s metrics from Datadog and supplies the observations to Jev. Reflex checks each recommendation against current control state before applying it.</p>
      <p>Start traffic and allow time for ingestion. Missing, incomplete, or stale telemetry prevents a model evaluation; it is not replaced with local measurements. Playback stays at 1× so simulation and telemetry clocks agree.</p>
      <p>The topology and live charts show the simulation. Inspect a decision in Activity to see the Datadog observations used for that decision, including their timestamps. Use the run ID above to filter the Datadog dashboard.</p>
    </details>
  </section>;
}
