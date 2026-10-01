// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

import '@datadog/druids/styles.css';
import React, { useCallback, useEffect, useMemo, useState } from 'react';
import { createRoot } from 'react-dom/client';
import { renderHeader } from './header.jsx';
import { ReflexEnvironment } from './theme.jsx';
import { Button } from '@datadog/druids/form/Button';
import { SoftToggle } from '@datadog/druids/form/SoftToggle';
import { ToggleButtons } from '@datadog/druids/form/ToggleButtons';
import { ToggleSwitch } from '@datadog/druids/form/ToggleSwitch';
import { StatusPill } from '@datadog/druids/pills/StatusPill';
import { Table } from '@datadog/druids/table/Table';
import { CalloutValue } from '@datadog/druids/measures/CalloutValue';
import { Text } from '@datadog/druids/typography/Text';
import { Modal } from '@datadog/druids/dialogs/Modal';
import { ArrowLeftIcon } from '@datadog/druids/icons/ArrowLeft';
import { PlayIcon } from '@datadog/druids/icons/Play';
import { PauseIcon } from '@datadog/druids/icons/Pause';
import { FlowMap } from './flow-map.jsx';
import { ScenarioControls } from './scenario-controls.jsx';
import { ForecastPanel } from './forecast-panel.jsx';
import { TimeChart } from './time-chart.jsx';

const fmt = (value, digits = 0) => Number(value).toLocaleString('en-US', { maximumFractionDigits: digits });
const usd = (value, digits = 2) => `$${Number(value).toFixed(digits)}`;
const time = ms => `${String(Math.floor(ms / 60000)).padStart(2, '0')}:${String(Math.floor(ms / 1000) % 60).padStart(2, '0')}`;
const phaseName = p => ({ stable: 'Stable', scaling_up: 'Scaling up', scaling_down: 'Scaling down' }[p] || p);
const phaseLevel = p => (p === 'stable' ? 'success' : 'warning');
const nodePhase = p => ({ provisioning: 'Provisioning', ready: 'Ready', draining: 'Draining', removed: 'Removed' }[p] || p);
const nodeLevel = p => ({ ready: 'success', provisioning: 'warning', draining: 'warning' }[p] || 'default');
const statusName = s => ({ applied: 'Applied', unchanged: 'No change', rejected: 'Rejected', evaluation_error: 'Evaluation error' }[s] || s);
const statusLevel = s => ({ applied: 'success', rejected: 'danger', evaluation_error: 'warning' }[s] || 'default');
const scenarios = [
  { value: 'live', label: 'Live', description: 'Drive the load yourself. No scheduled changes.' },
  { value: 'surge_recovery', label: 'Surge and recovery', description: 'Two workloads triple their replicas, then recover. Pods wait for nodes; nodes must be removed safely.' },
  { value: 'memory_heavy_burst', label: 'Memory-heavy burst', description: 'Pods need four times the memory. Only the memory-heavy group can hold them.' },
  { value: 'cyclical_load', label: 'Cyclical load · Toto', description: 'Demand ramps every two minutes. Toto learns the pattern; Jev can provision ahead of it.' },
];
const NO_PAGES = { isEnabled: false };
const PAGES = { isEnabled: true, pageSize: 15 };
const TABLE_SUMMARY = { isEnabled: false };
const root = createRoot(document.getElementById('reflex-app'));

// "scale_up:general_large:2", "remove:large-4" and "no_change" are the labels Jev chooses from.
const groupNames = { general_small: 'Small general', general_large: 'Large general', memory_heavy: 'Memory-heavy' };
function choiceName(choice) {
  if (!choice) return 'No recommendation';
  if (choice === 'no_change') return 'No change';
  const [kind, target, count] = choice.split(':');
  if (kind === 'remove') return `Remove ${target}`;
  return `Add ${count} × ${groupNames[target] || target}`;
}
function controlName(control, state) {
  const on = control.enabled ? 'on' : 'off';
  if (control.kind === 'stockout') return `${state.groups.find(g => g.group === control.group)?.name || control.group}: stockout ${on}`;
  const workload = state.workloads[control.workload]?.name || control.workload;
  return `${workload}: ${control.kind === 'replica_surge' ? 'replica surge' : 'memory-heavy pods'} ${on}`;
}
const podStatus = (pod, at) => pod.node == null ? 'Pending' : pod.available_at > at ? 'Starting' : 'Running';

function Section({ title, children, action, description }) {
  return <section className="inspector-section"><div className="section-heading"><Text as="h2" weight="bold" size="lg" className="section-title">{title}</Text>{action}</div>{description && <Text as="p" size="sm" variant="secondary" className="section-description">{description}</Text>}{children}</section>;
}
function Stat({ label, value }) { return <span className="header-stat"><Text variant="secondary">{label}: </Text><Text weight="bold">{value}</Text></span>; }
function DataTable({ data, columns, pagination = NO_PAGES, empty = 'No results.' }) {
  return <Table data={data} columns={columns} pagination={pagination} summary={TABLE_SUMMARY} rowHeight="md" emptyState={{ title: empty, size: 'sm', imagePath: null }} shouldResetOnDataChange={false} />;
}
function Transport({ state, disabled, send }) {
  const ended = state.at_ms >= state.horizon_ms;
  const datadog = state.evidence_source === 'datadog';
  return <div className="transport-bar">
    <div className="transport-group">
      <Button id="play" icon={state.paused ? PlayIcon : PauseIcon} isPrimary label={ended ? 'Complete' : state.paused ? (state.at_ms ? 'Resume' : 'Start cluster') : 'Pause'} isTitleCased={false} isDisabled={disabled || ended} onClick={() => send({ type: state.paused ? 'play' : 'pause' })} />
      <Button id="step" isBorderless label="+1s" ariaLabel="Step one second" isDisabled={datadog || disabled || ended} onClick={() => send({ type: 'step' })} />
      <span className="transport-divider" /><Text isMonospace size="sm" title={`Run duration ${time(state.horizon_ms)}`}>{time(state.at_ms)}</Text>
      <StatusPill isSoft level={!state.paused && !ended ? 'success' : 'default'}>{ended ? 'Complete' : state.paused ? (state.at_ms ? 'Paused' : 'Ready') : 'Running'}</StatusPill>
    </div><div className="transport-group playback-actions">
      {!datadog && <ToggleButtons aria-label="Playback speed" options={[{ value: 1, label: '1×' }, { value: 2, label: '2×' }, { value: 4, label: '4×' }]} value={state.speed} isDisabled={disabled} onChange={value => send({ type: 'speed', value })} />}
      <Button id="reset" label="Reset" isBorderless isDisabled={disabled} onClick={() => send({ type: 'reset' })} />
    </div>
  </div>;
}

function ClusterMap({ state, focus, onFocus, scenarioPicker, playback }) {
  const { nodes, edges } = useMemo(() => {
    const nodes = [], edges = [];
    const selected = (kind, id) => focus?.kind === kind && focus.id === id;
    state.workloads.forEach((w, i) => {
      const notes = [w.surge && 'Replica surge', w.memory_heavy && 'Memory-heavy'].filter(Boolean).join(' · ');
      nodes.push({ key: `workload-${i}`, column: 0, kind: 'client', name: w.name, selected: selected('workload', i), subtext: `${w.cpu} CPU · ${w.pod_memory_gib} GiB per pod`, annotation: notes || undefined, metrics: [{ label: 'Wanted', value: w.desired }, { label: 'Serving', value: w.available }, { label: 'Pending', value: w.pending }], metricLevels: ['default', w.available < w.min_available ? 'danger' : 'default', w.pending ? 'warning' : 'default'] });
      edges.push({ key: `pending-${i}`, sourceId: `workload-${i}`, targetId: 'queue', active: w.pending > 0, status: 'warning', count: Math.min(3, w.pending) });
    });
    nodes.push({ key: 'queue', column: 1, kind: 'queue', selectable: false, name: 'Pending queue', subtext: 'Pods that fit no ready node', metrics: [{ label: 'Pending', value: state.pending_pods }, { label: 'Oldest', value: `${fmt(state.oldest_pending_ms / 1000)}s` }], metricLevels: [state.pending_pods ? 'warning' : 'default', state.oldest_pending_ms > state.provision_ms ? 'danger' : 'default'] });
    edges.push({ key: 'queue-hub', sourceId: 'queue', targetId: 'hub', active: state.pending_pods > 0, status: 'warning' });
    const latest = state.decisions[0];
    const recent = latest && state.at_ms - latest.at_ms <= 8000 ? (latest.status === 'rejected' ? `Rejected · ${latest.code}` : latest.status === 'applied' ? `Applied · ${choiceName(latest.choice)}` : statusName(latest.status)) : undefined;
    nodes.push({ key: 'hub', column: 1, kind: 'hub', name: 'Autoscaler', subtext: 'Jev + Reflex', phase: phaseName(state.phase), status: phaseLevel(state.phase), annotation: recent, metrics: [{ label: 'Nodes', value: `${state.active_nodes} / ${state.node_budget}` }, { label: 'Cost', value: `${usd(state.hourly_usd)}/h` }] });
    state.groups.forEach(g => {
      nodes.push({ key: `group-${g.group}`, column: 2, kind: 'group', name: g.name, selected: selected('group', g.group), subtext: `${g.cpu} CPU · ${g.memory_gib} GiB · ${usd(g.hourly_usd)}/h`, annotation: g.stockout ? 'Out of stock' : undefined, metrics: [{ label: 'Ready', value: g.ready }, { label: 'Starting', value: g.provisioning }, { label: 'Limit', value: `${g.min}–${g.max}` }], metricLevels: ['default', g.provisioning ? 'warning' : 'default', 'default'] });
      edges.push({ key: `hub-${g.group}`, sourceId: 'hub', targetId: `group-${g.group}`, active: g.provisioning > 0, status: 'warning', count: Math.max(1, g.provisioning) });
    });
    state.nodes.filter(n => n.phase !== 'removed').forEach(n => {
      const pods = state.pods.filter(p => p.node === n.id).length;
      const remaining = Math.max(0, Math.ceil((n.requested_at + state.provision_ms - state.at_ms) / 1000));
      nodes.push({ key: `node-${n.id}`, column: 3, kind: 'node', name: n.name, selected: selected('node', n.id), subtext: n.phase === 'provisioning' ? (remaining ? `${remaining}s left` : 'Almost ready') : `${n.used_cpu}/${n.cpu} CPU · ${n.used_memory_gib}/${n.memory_gib} GiB`, phase: nodePhase(n.phase), status: nodeLevel(n.phase), ratio: Math.max(n.used_cpu / n.cpu, n.used_memory_gib / n.memory_gib), ratioColor: 'var(--r-ai)' });
      edges.push({ key: `group-node-${n.id}`, sourceId: `group-${n.group}`, targetId: `node-${n.id}`, active: n.phase === 'ready' && pods > 0, count: 1 });
    });
    return { nodes, edges };
  }, [state, focus]);
  const select = key => {
    const [kind, ...rest] = key.split('-'), id = rest.join('-');
    if (kind === 'workload' || kind === 'node') onFocus({ kind, id: Number(id) });
    else if (kind === 'group') onFocus({ kind, id });
    else onFocus(null);
  };
  return <div className="topology-canvas" aria-label="Cluster topology">{scenarioPicker}<div className="map-playback" role="region" aria-label="Simulation playback">{playback}</div>
    <FlowMap nodes={nodes} edges={edges} running={!state.paused && state.at_ms < state.horizon_ms} speed={state.speed} onSelect={select} />
  </div>;
}

function ControlRow({ id, label, detail, checked, disabled, onChange }) {
  return <div className={`fault-row ${checked ? 'fault-active' : ''}`}><div><Text as="div" size="sm" weight="bold">{label}</Text><Text size="xs" variant="secondary">{detail}</Text></div><ToggleSwitch id={id} ariaLabel={label} isChecked={checked} isDisabled={disabled} onChange={onChange} /></div>;
}
function LoadControls({ state, focus, disabled, send }) {
  const ended = state.at_ms >= state.horizon_ms, off = disabled || ended;
  const control = control => send({ type: 'control', control });
  const workloads = state.workloads.map((w, i) => ({ ...w, id: i })).filter(w => focus?.kind !== 'workload' || focus.id === w.id);
  const groups = state.groups.filter(g => focus?.kind !== 'group' || focus.id === g.group);
  return <Section title="Load controls" description="Change what the workloads ask for, or take a node group out of stock, and watch how the cluster is scaled.">
    {focus?.kind !== 'group' && workloads.map(w => <div className="autoscaler-control-group" key={w.id}>
      <Text as="div" size="xs" weight="bold" className="autoscaler-control-title">{w.name} · {w.desired} × {w.cpu} CPU / {w.pod_memory_gib} GiB</Text>
      <div className="fault-controls">
        <ControlRow id={`surge-${w.id}`} label="Replica surge" detail="Three times as many replicas" checked={w.surge} disabled={off} onChange={() => control({ kind: 'replica_surge', workload: w.id, enabled: !w.surge })} />
        <ControlRow id={`memory-${w.id}`} label="Memory-heavy pods" detail="Four times the memory per pod; only memory-heavy nodes fit them" checked={w.memory_heavy} disabled={off} onChange={() => control({ kind: 'memory_heavy', workload: w.id, enabled: !w.memory_heavy })} />
      </div>
    </div>)}
    {focus?.kind !== 'workload' && <div className="autoscaler-control-group">
      <Text as="div" size="xs" weight="bold" className="autoscaler-control-title">Node group stock</Text>
      <div className="fault-controls">{groups.map(g => <ControlRow key={g.group} id={`stockout-${g.group}`} label={`${g.name} stockout`} detail={`New ${g.cpu} CPU / ${g.memory_gib} GiB nodes fail to provision${g.failures ? ` · ${g.failures} failed so far` : ''}`} checked={g.stockout} disabled={off} onChange={() => control({ kind: 'stockout', group: g.group, enabled: !g.stockout })} />)}</div>
    </div>}
  </Section>;
}
function ClusterSummary({ state }) {
  const values = [
    ['Pending pods', fmt(state.pending_pods), state.pending_pods ? 'warning' : 'success'],
    ['Oldest wait', `${fmt(state.oldest_pending_ms / 1000)}s`, state.oldest_pending_ms > state.provision_ms ? 'danger' : 'default'],
    ['Pending time', `${fmt(state.pending_pod_seconds)} pod-s`, 'default'],
    ['Nodes', `${state.active_nodes} / ${state.node_budget}`, 'default'],
    ['Cost rate', `${usd(state.hourly_usd)}/h`, 'default'],
    ['Cost this run', usd(state.cluster_cost_usd, 3), 'default'],
  ];
  return <Section title="Pending time, nodes and cost"><div className="outcome-grid">{values.map(([label, value, level]) => <CalloutValue isBorderless className="outcome-value" key={label} label={label} value={value} size="sm" level={level} />)}</div><Text size="xs" variant="secondary">Pending time adds up every second each pod waited. Node prices are illustrative hourly rates.</Text></Section>;
}
function CapacityChart({ state }) {
  const series = [
    { name: 'Requested CPU', color: 'purple', points: state.history.map(p => [p.at_ms, p.requested_cpu]) },
    { name: 'Ready CPU', color: 'orange', step: true, points: state.history.map(p => [p.at_ms, p.ready_cpu]) },
    { name: 'Pending pods', color: 'var(--r-bad)', right: true, points: state.history.map(p => [p.at_ms, p.pending_pods]) },
  ];
  return <Section title="Demand and capacity"><TimeChart series={series} start={0} end={state.horizon_ms} leftLabel="CPU" rightLabel="Pending pods" rightInteger label="Requested CPU, ready CPU and pending pods over the run" emptyMessage="Start or step the cluster to collect history." /><Text size="xs" variant="secondary">Requested CPU counts every pod, placed or pending. Ready CPU rises only when a node finishes provisioning.</Text></Section>;
}
function Forecast({ state, disabled, send }) {
  if (!state.forecast?.configured) return null;
  const h = state.forecast_headroom;
  return <section id="forecast-panel" className="forecast-panel inspector-section" aria-label="Toto forecast">
    <ForecastPanel view={state.forecast} title="Cluster demand" policy="jev" disabled={disabled} onToggle={enabled => send({ type: 'forecast', enabled })} />
    <Text as="p" size="xs" variant="secondary">{h ? `Forecast headroom now: ${h.cpu} CPU and ${h.memory_gib} GiB above current requests within a minute. It can justify provisioning ahead of demand; it is never counted as capacity.` : 'No fresh forecast: scale-ups are justified by pending pods only.'}</Text>
  </section>;
}
const GIB = 1073741824;
// Readiness and age of the Datadog observations Jev is being shown, as on the other pages.
function TelemetryStatus({ state }) {
  if (state.evidence_source !== 'datadog') return null;
  const t = state.telemetry;
  const label = t ? `${t.age_seconds > 60 ? 'Stale telemetry' : 'Telemetry age'}: ${Math.floor(t.age_seconds)}s` : /collecting|warming/i.test(state.telemetry_status || '') ? 'Waiting for telemetry' : 'Telemetry unavailable';
  return <span className="circuit-telemetry" title={t ? 'Age of the latest Datadog observation of this cluster' : state.telemetry_status} role="status">{label}</span>;
}
function Telemetry({ state }) {
  if (state.evidence_source !== 'datadog') return null;
  const t = state.telemetry;
  const pending = t?.workloads.reduce((n, w) => n + w.pending_pods, 0);
  return <Section title="Datadog telemetry" description="This run publishes its metrics to Datadog and queries them back. Jev always decides on the live cluster; these delayed observations are attached as context.">
    <Text as="p" size="sm" role="status">{state.telemetry_status}</Text>
    {t && <div className="outcome-grid">{[
      ['Observed', `${fmt(t.age_seconds)}s ago`],
      [`CPU asked · live ${state.requested.cpu}`, fmt(t.requested_cpu)],
      [`Memory GiB · live ${state.requested.memory_gib}`, fmt(t.requested_memory_bytes / GIB)],
      [`Pending pods · live ${state.pending_pods}`, fmt(pending)],
    ].map(([label, value]) => <CalloutValue isBorderless className="outcome-value" key={label} label={label} value={value} size="sm" />)}</div>}
    <Text as="p" size="xs" variant="secondary">Playback is 1× in real time and stepping is off, because Datadog observes the run on the wall clock. Export is every 10 seconds and queries skip the newest 20, so observations are tens of seconds old. Pausing or resetting starts a new collection window.</Text>
  </Section>;
}
function Inference({ state }) {
  const c = state.cost, p = state.pending;
  return <Section title="Jev activity">
    {!state.available && <Text as="p" size="sm">Jev is not configured on this server; the cluster will not be scaled.</Text>}
    {p && <Text as="p" size="sm">Evaluating the cluster as of {time(p.observed_at_ms)}{p.response_ready && state.paused ? '; the reply applies after resuming' : ''}.</Text>}
    <div className="inference-count"><Stat label="Evaluations" value={fmt(state.calls)} /></div>
    <Text as="p" size="xs" variant="secondary" className="autoscaler-run">Run tag <code>{state.simulation_run}</code>: with Datadog export on, every metric from this run carries it as <code>simulation_run</code>.</Text>
    {c && <details className="cost-detail"><summary>Estimated Jev cost: ${Number(c.estimated_usd).toFixed(6)} · since server start</summary><Text as="p" size="sm">{fmt(c.input_tokens)} input tokens · {fmt(c.output_tokens)} output tokens · {fmt(c.priced_calls)} priced responses. {c.missing_usage_calls} calls without reported usage; {c.unpriced_calls} without a known rate.</Text><Text as="p" size="xs" variant="secondary">Jev 1.13 input: $0.042 per million tokens; output free. Excludes unreported and unpriced usage. Reset preserves this estimate; restarting clears it. This is not your final bill.</Text></details>}
  </Section>;
}
function Events({ state, limit = 12 }) {
  const entries = [
    ...state.transitions.map(e => ({ at: e.at_ms, title: e.from === e.to ? phaseName(e.to) : `${phaseName(e.from)} → ${phaseName(e.to)}`, detail: e.trigger })),
    ...state.controls.map(e => ({ at: e.at_ms, title: controlName(e.control, state), detail: e.source === 'preset' ? 'Scheduled by the scenario' : 'Changed by you' })),
    ...state.decisions.filter(d => (d.status === 'rejected' && d.choice !== 'no_change') || d.status === 'evaluation_error').map(d => ({ at: d.at_ms, level: d.status === 'rejected' ? 'danger' : 'warning', title: `${statusName(d.status)}: ${choiceName(d.choice)}`, detail: d.reason })),
  ].sort((a, b) => b.at - a.at).slice(0, limit);
  return <Section title="Activity"><div className="incident-log">{entries.length ? entries.map((e, i) => <div className="incident-event" data-level={e.level} key={`${e.at}-${i}`}><Text size="xs" isMonospace variant="secondary">{time(e.at)}</Text><div><Text as="div" size="sm" weight="bold">{e.title}</Text><Text size="xs" variant="secondary">{e.detail}</Text></div></div>) : <Text size="sm" variant="secondary">Nothing yet. Start the cluster and change the load.</Text>}</div></Section>;
}
function Decisions({ state, onInspect }) {
  const columns = useMemo(() => [
    { Header: 'Time', accessor: 'at_ms', width: '52px', Cell: ({ value }) => time(value) },
    { Header: 'Recommendation', id: 'choice', accessor: d => choiceName(d.choice), width: 'minmax(100px, 1fr)', Cell: ({ value, row }) => <Button label={value} isPrimary isDangerouslyNaked isTitleCased={false} onClick={() => onInspect(row.original)} /> },
    { Header: 'Reflex', id: 'guard', accessor: d => d.status, width: '132px', Cell: ({ row }) => <span className="autoscaler-guard" data-level={statusLevel(row.original.status)} title={row.original.reason}>{row.original.status === 'rejected' ? row.original.code : statusName(row.original.status)}</span> },
  ], [onInspect]);
  return <Section title="Jev decisions" description="Select a recommendation to see the evidence Jev received, its reply, and what Reflex did with it."><DataTable data={state.decisions} columns={columns} pagination={PAGES} empty="No recommendations yet." /></Section>;
}
function Pods({ state, focus }) {
  const data = useMemo(() => state.pods.filter(p => !focus || (focus.kind === 'workload' ? p.workload === focus.id : focus.kind === 'node' ? p.node === focus.id : state.nodes.find(n => n.id === p.node)?.group === focus.id)).map(p => {
    const node = state.nodes.find(n => n.id === p.node), status = podStatus(p, state.at_ms);
    return { id: p.id, name: `${state.workloads[p.workload].name}-${p.id}`, request: `${p.cpu} CPU · ${p.memory_gib} GiB`, status, wait: p.pending_since == null ? 0 : state.at_ms - p.pending_since, where: node ? node.name : `Waited ${fmt((state.at_ms - p.pending_since) / 1000)}s` };
  }).sort((a, b) => b.wait - a.wait || a.where.localeCompare(b.where) || a.id - b.id), [state.pods, state.nodes, state.at_ms, focus]);
  const columns = useMemo(() => [
    { Header: 'Pod', accessor: 'name', width: 'minmax(60px, 1fr)' },
    { Header: 'Request', accessor: 'request', width: '100px' },
    { Header: 'Node', accessor: 'where', width: '120px', Cell: ({ row, value }) => <span className="autoscaler-pod" data-status={row.original.status} title={row.original.status}>{value}{row.original.status === 'Starting' ? ' (starting)' : ''}</span> },
  ], []);
  return <Section title="Pods" description="Placement is automatic best fit. A pod that fits no ready node waits as pending."><DataTable data={data} columns={columns} pagination={PAGES} empty="No pods here." /></Section>;
}
function Inspector({ state, focus, onFocus, disabled, send, onDecision }) {
  const [tab, setTab] = useState('overview');
  const workload = focus?.kind === 'workload' ? state.workloads[focus.id] : null;
  const group = focus?.kind === 'group' ? state.groups.find(g => g.group === focus.id) : null;
  const node = focus?.kind === 'node' ? state.nodes.find(n => n.id === focus.id) : null;
  const name = workload?.name || group?.name || node?.name || 'Cluster';
  const description = workload ? `Deployment · ${workload.cpu} CPU and ${workload.pod_memory_gib} GiB per pod · at least ${workload.min_available} replicas must stay available`
    : group ? `Node group · ${group.cpu} CPU and ${group.memory_gib} GiB per node · ${group.min} to ${group.max} nodes`
    : node ? `${state.groups.find(g => g.group === node.group)?.name} node · ${nodePhase(node.phase)}`
    : 'Three deployments on three node groups. Jev recommends one change at a time; Reflex checks it against the live cluster.';
  const stats = workload ? [['Wanted', workload.desired], ['Serving', workload.available], ['Pending', workload.pending]]
    : group ? [['Ready', group.ready], ['Starting', group.provisioning], ['Cost', `${usd(group.hourly_usd)}/h`]]
    : node ? [['CPU', `${node.used_cpu} / ${node.cpu}`], ['Memory', `${node.used_memory_gib} / ${node.memory_gib} GiB`]]
    : [['Nodes', `${state.active_nodes} / ${state.node_budget}`], ['Pending', state.pending_pods], ['Cost', `${usd(state.hourly_usd)}/h`]];
  const notable = state.transitions.length + state.controls.length + state.decisions.length;
  return <div className="discovery-panel" id="discovery-inspector">
    <header className="panel-heading" aria-label="Selection summary">
      <div className="inspector-topline">{focus ? <Button icon={ArrowLeftIcon} label="Back to cluster" isPrimary isDangerouslyNaked isTitleCased={false} size="sm" onClick={() => onFocus(null)} /> : <Text size="sm" variant="secondary" weight="bold">Cluster autoscaler overview</Text>}{focus ? (node ? <StatusPill isSoft level={nodeLevel(node.phase)}>{nodePhase(node.phase)}</StatusPill> : null) : <StatusPill isSoft level={phaseLevel(state.phase)}>{phaseName(state.phase)}</StatusPill>}</div>
      <div className="entity-heading"><span className="entity"><Text>{name}</Text></span>{!focus && <span className="reflex-protection">Protected by Reflex</span>}</div>
      {!focus && <TelemetryStatus state={state} />}
      <Text size="sm" variant="secondary">{description}</Text>
      <div className="header-stats">{stats.map(([label, value]) => <Stat key={label} label={label} value={value} />)}</div>
    </header>
    <div className="inspector-tabs">
      <SoftToggle ariaLabel="Inspector sections" value={tab} onChange={setTab} isFullWidth hasEqualWidthOptions options={[
        { label: 'Overview', value: 'overview' },
        { label: 'Pods', value: 'pods', statusLabel: String(state.pods.length) },
        { label: 'Activity', value: 'activity', statusLabel: String(notable) },
      ]} />
    </div>
    <div key={tab} className="panel-scroll">
      <div className="panel-sections" id="inspector-content" role="region" aria-label={`${tab} for ${name}`}>
        {tab === 'overview' && <>
          <Text as="p" size="sm" className="autoscaler-status" role="status">{state.status}</Text>
          {!node && <LoadControls state={state} focus={focus} disabled={disabled} send={send} />}
          {node && <Pods state={state} focus={focus} />}
          {!focus && <Telemetry state={state} />}
          {!focus && <ClusterSummary state={state} />}
          {!focus && <CapacityChart state={state} />}
          {!focus && <Forecast state={state} disabled={disabled} send={send} />}
          {!focus && <Inference state={state} />}
          <Events state={state} limit={6} />
        </>}
        {tab === 'pods' && <Pods state={state} focus={focus} />}
        {tab === 'activity' && <><Events state={state} limit={40} /><Decisions state={state} onInspect={onDecision} /></>}
      </div>
    </div>
  </div>;
}
function DecisionDetail({ decision: d, state }) {
  return <div className="decision-detail">
    <Text as="p"><StatusPill isSoft level={statusLevel(d.status)}>{statusName(d.status)}</StatusPill> {choiceName(d.choice)} · evidence from {time(d.observed_at_ms)}, decided at {time(d.at_ms)}</Text>
    <Text as="h3" weight="bold">What Reflex did</Text>
    <Text as="p">{d.reason}. Phase: {phaseName(d.from)}{d.from === d.to ? ' (unchanged)' : ` → ${phaseName(d.to)}`}.</Text>
    {(state.evidence_source === 'datadog' || d.evidence.telemetry || d.evidence.forecast_basis) && <>
      <Text as="h3" weight="bold">Where the evidence came from</Text>
      <Text as="p">{d.evidence.telemetry ? `Datadog telemetry for run ${d.evidence.telemetry.simulation_run}, observed at ${new Date(d.evidence.telemetry.observed_at_unix_ms).toLocaleTimeString()} and ${fmt(d.evidence.telemetry.age_seconds)}s old when Jev was asked. It is context: the cluster state beside it is live.` : 'No Datadog telemetry was attached; Jev decided from the live cluster state alone.'}</Text>
      <Text as="p">{d.evidence.forecast_basis ? `Forecast from ${d.evidence.forecast_basis.source === 'datadog_observations' ? 'Datadog history' : 'simulator samples'} (${d.evidence.forecast_basis.history_seconds}s of history), ${d.evidence.forecast_basis.age_seconds}s old; it may justify a scale-up until it is ${d.evidence.forecast_basis.max_age_seconds}s old.` : 'No fresh forecast was attached, so only pending pods could justify a scale-up.'}</Text>
    </>}
    <Text as="h3" weight="bold">Evidence sent to Jev</Text>
    <pre>{JSON.stringify(d.evidence, null, 2)}</pre>
    <Text as="h3" weight="bold">Jev's reply</Text>
    <pre>{JSON.stringify(d.result, null, 2)}</pre>
  </div>;
}
function Help({ state }) {
  return <div className="help-content">
    <p>Three deployments (<strong>web, api and batch</strong>) run as pods on three node groups: small general, large general and memory-heavy. A pod that fits no ready node waits as <strong>pending</strong>.</p>
    <p>Jev looks at the cluster every few seconds and recommends one action: <strong>scale up</strong> a group by one or two nodes, <strong>remove</strong> a named node, or <strong>no change</strong>. A new node takes about 30 seconds to become ready. Placement is automatic best fit.</p>
    <p>Reflex owns the cluster as one state machine and checks every recommendation when it executes:</p>
    <ul>
      <li><strong>fresh:</strong> the cluster has not changed since Jev looked, and the evidence is at most five simulated seconds old.</li>
      <li><strong>within_limits:</strong> the group stays under its maximum and the cluster under its node budget.</li>
      <li><strong>justified_scale_up:</strong> ready and provisioning nodes do not already cover pending pods plus a capped forecast headroom.</li>
      <li><strong>scale_down_cooldown:</strong> no scale-up finished in the last {(state?.scale_down_cooldown_ms ?? 30000) / 1000} seconds, no pods are pending, and the group is above its minimum.</li>
      <li><strong>drainable:</strong> every pod on the node fits on another ready node right now.</li>
      <li><strong>disruption_budget:</strong> each workload keeps at least half of its replicas available.</li>
    </ul>
    {state?.evidence_source === 'datadog' && <p><strong>Datadog evidence is on.</strong> The run publishes its metrics, queries them back, and attaches them to Jev's evidence as delayed context; Toto forecasts from that queried history. Playback is 1× in real time without stepping, the run lasts {(state.horizon_ms / 60000).toFixed(0)} minutes, and a forecast stops justifying a scale-up once its last Datadog bucket is 60 seconds old.</p>}
    <p>Use the load controls to triple a workload's replicas, make its pods memory-heavy, or put a node group out of stock. With Toto running, a forecast lets Jev provision before a predicted ramp; a forecast never counts as capacity, and confidence bypasses no guard.</p>
  </div>;
}
function App({ state, disabled = true, connected = false, error, send, navigate }) {
  const [focus, setFocus] = useState(null), [modal, setModal] = useState(null);
  const onDecision = useCallback(decision => setModal({ type: 'decision', decision }), []);
  useEffect(() => renderHeader({ disabled, connected, navigate, onHelp: () => setModal({ type: 'help' }) }), [disabled, connected, navigate]);
  // A selected node disappears when it is removed.
  const current = focus?.kind === 'node' && !state?.nodes.some(n => n.id === focus.id && n.phase !== 'removed') ? null : focus;
  return <ReflexEnvironment><div className="discovery-app">
    {error && <div className="error-banner" role="alert">{error}</div>}
    {state ? <div className="discovery-layout">
      <ClusterMap state={state} focus={current} onFocus={setFocus} scenarioPicker={<ScenarioControls state={state} disabled={disabled} send={send} options={scenarios} description={state.scenario_description} />} playback={<Transport state={state} disabled={disabled} send={send} />} />
      <Inspector key={current ? `${current.kind}-${current.id}` : 'cluster'} state={state} focus={current} onFocus={setFocus} disabled={disabled} send={send} onDecision={onDecision} />
    </div> : <div className="loading-state"><Text>Connecting to the Reflex engine…</Text></div>}
    <Modal isOpen={!!modal} onClose={() => setModal(null)} title={modal?.type === 'help' ? 'How it works' : 'Jev decision'} size="md" isScrollable>
      {modal?.type === 'help' && <Help state={state} />}
      {modal?.type === 'decision' && state && <DecisionDetail decision={modal.decision} state={state} />}
    </Modal>
  </div></ReflexEnvironment>;
}
window.renderAutoscaler = props => root.render(<App {...props} />);
window.renderAutoscaler({});
