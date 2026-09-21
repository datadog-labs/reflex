#!/usr/bin/env python3
"""Independently check follow-up cohorts, cached inputs, costs and replay evidence."""
from pathlib import Path
import collections,hashlib,json,math
ROOT=Path(__file__).resolve().parents[2];old=ROOT/'output/capacity-study';new=ROOT/'output/capacity-study-efficiency'
paths=list(new.glob('*/results-equal.json'))
expected={f'{p}-{l}-{s}' for p in ['steady','spike','ramp','waves','bursts','mix'] for l in ['moderate','heavy'] for s in range(1001,1011)}
assert {p.parent.name for p in paths}==expected
prompt=(ROOT/'studies/capacity/prompts/efficiency-v1.txt').read_text()
for seed in range(1001,1011):
 manifest=json.loads((new/f'manifest-{seed}-equal.json').read_text());assert manifest['prompt']==prompt
 assert manifest['args']['model']=='jev-1.13.0' and manifest['args']['timing']=='equal'
for path in paths:
 for name in ['workload.json','forecasts.json']:
  assert hashlib.sha256((path.parent/name).read_bytes()).digest()==hashlib.sha256((old/path.parent.name/name).read_bytes()).digest(),(path,name)
 results=json.loads(path.read_text());assert {r['policy'] for r in results}=={'jev','jev_toto'}
 for r in results:
  jobs=json.loads((path.parent/f'jobs-{r["policy"]}-equal.json').read_text());before=json.loads((old/path.parent.name/f'jobs-{r["policy"]}-equal.json').read_text())
  assert [(j['arrival'],j['arrived_at']) for j in jobs]==[(j['arrival'],j['arrived_at']) for j in before]
  jobs=[j for j in jobs if 300000<j['arrived_at']<=2100000];phases=collections.Counter(j['phase'] for j in jobs)
  assert len(jobs)==r['offered'];assert phases['completed']==r['completed'];assert phases['rejected']==r['rejected'];assert r['completed']+r['rejected']+r['unfinished']==len(jobs)
  calls=0;missing=0;input_tokens=0;output_tokens=0;known=0.;unpriced=0
  for line in (path.parent/f'decisions-{r["policy"]}-equal.jsonl').read_text().splitlines():
   d=json.loads(line);x=d['result'];error=x['error'] or ''
   assert not any(code in error for code in ['typesafe_http_401','typesafe_http_402','typesafe_http_403'])
   assert 'actual_s' not in json.dumps(d['evidence']) and 'finish_at' not in json.dumps(d['evidence'])
   if x['latency_ms']<=0:continue
   calls+=1
   if x['usage'] is None:missing+=1;continue
   u=x['usage'];input_tokens+=u['input_tokens'];output_tokens+=u['output_tokens']
   if x['model']=='jev-1.13.0':known+=u['input_tokens']*.042/1e6
   else:unpriced+=1
  c=r['jev_cost'];assert (c['calls'],c['missing_usage_calls'],c['input_tokens'],c['output_tokens'],c['unpriced_calls'])==(calls,missing,input_tokens,output_tokens,unpriced)
  assert math.isclose(c['estimated_usd'],known,rel_tol=0,abs_tol=1e-12)
replays=sum((new/f'replay-{s}.log').read_text().count('REPLAY VERIFIED') for s in range(1001,1011));assert replays==120
report=json.loads((new/'report/summary.json').read_text());per_trace=json.loads((new/'report/per-trace.json').read_text())
assert len(per_trace)==1320
for policy in report['policies']:
 data=[r for r in per_trace if r['policy']==policy['policy']];assert len(data)==120
 for kind in ['wait','completion']:
  weighted=sum(r[kind+'_mean']*r['completed'] for r in data)/sum(r['completed'] for r in data)
  assert math.isclose(weighted,policy[kind]['mean'],rel_tol=0,abs_tol=1e-10)
 assert sum(r['completed'] for r in data)==policy['completed']
 assert sum(r['rejected'] for r in data)==policy['rejected']
out=dict(traces=120,policy_runs=240,exact_replay_traces=replays,checks=['original workload and forecast byte identity','offered job identities and arrivals','completion/rejection/unfinished conservation','frozen prompt and model','no hidden future execution durations in evidence','no billing/auth failures','per-call usage and cost reconciliation','exact replay of all new outcomes','pooled report means and counts reconcile with all 1320 per-trace rows'])
(new/'report/validation.json').write_text(json.dumps(out,indent=2));print(json.dumps(out))
