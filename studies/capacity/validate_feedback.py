#!/usr/bin/env python3
# Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
# This product includes software developed at Datadog (https://www.datadoghq.com/).
# Copyright 2026-present Datadog, Inc.

"""Independent verification from final job journals and recorded decision evidence."""
from pathlib import Path
import collections, hashlib, json, math, argparse
ROOT=Path(__file__).resolve().parents[2]
BASE=ROOT/'output/capacity-study';NEW=ROOT/'output/capacity-study-feedback'

def read(p):return json.loads(p.read_text())
def close(a,b):
 if a is None or b is None:assert a is b,(a,b)
 else:assert math.isclose(a,b,rel_tol=1e-10,abs_tol=1e-10),(a,b)
def quantile(v,p):return sorted(v)[math.ceil((len(v)-1)*p)] if v else None
def check_dist(actual,values):
 assert actual['count']==len(values)
 close(actual['mean'],sum(values)/len(values) if values else None)
 for q in [25,50,99]:close(actual[f'p{q}'],quantile(values,q/100))

def main():
 parser=argparse.ArgumentParser();parser.add_argument('--trace');args=parser.parse_args()
 paths=list(NEW.glob((args.trace or '*')+'/results-equal.json'));assert paths
 expected={f'{p}-{l}-{s}' for p in ['steady','spike','ramp','waves','bursts','mix'] for l in ['moderate','heavy'] for s in range(1001,1011)}
 if not args.trace:assert {p.parent.name for p in paths}==expected
 hashes=read(NEW/'source-sha256.json')
 for name,digest in hashes.items():assert hashlib.sha256((ROOT/name).read_bytes()).hexdigest()==digest,name
 prompt=(ROOT/'studies/capacity/prompts/feedback-v1.txt').read_text()
 for seed in sorted({int(p.parent.name.rsplit('-',1)[1]) for p in paths}):
  m=read(NEW/f'manifest-{seed}-equal.json');assert m['prompt']==prompt
  assert m['args']['performance_feedback'] and m['args']['model']=='jev-1.13.0' and m['args']['timing']=='equal'
 checked=0;windows=0
 for path in paths:
  for name in ['workload.json','forecasts.json']:
   assert hashlib.sha256((path.parent/name).read_bytes()).digest()==hashlib.sha256((BASE/path.parent.name/name).read_bytes()).digest()
  results=read(path);assert {r['policy'] for r in results}=={'jev','jev_toto'}
  for r in results:
   policy=r['policy'];jobs=read(path.parent/f'jobs-{policy}-equal.json');old=read(BASE/path.parent.name/f'jobs-{policy}-equal.json')
   assert [(j['arrival'],j['arrived_at']) for j in jobs]==[(j['arrival'],j['arrived_at']) for j in old]
   cohort=[j for j in jobs if 300000<j['arrived_at']<=2100000];phases=collections.Counter(j['phase'] for j in cohort)
   assert len(cohort)==r['offered'];assert phases['completed']==r['completed'];assert phases['rejected']==r['rejected']
   assert r['completed']+r['rejected']+r['unfinished']==len(cohort)
   outcomes=[]
   for j in jobs:
    if j['phase']=='completed':outcomes.append((j['started_at']+j['arrival']['actual_s']*1000,j))
    elif j['phase']=='rejected':outcomes.append((j['arrived_at']+(60000 if j['reason']=='Queue deadline exceeded (60s)' else 0),j))
   timeline=read(path.parent/f'timeline-{policy}-equal.json')
   changes=[x for x in timeline if x.get('applied') and x['action']!='hold']
   # Independent integral of active capacity from initial two nodes, applied actions,
   # startup timers, and final job reservation intervals. All query endpoints are past.
   ready=[True,True,False,False,False,False];starting={};draining=set();active_integral=[0]
   cpu_integral=[0];mem_integral=[0]
   by_t=collections.defaultdict(list)
   for x in changes:by_t[x['at']].append(x)
   running_intervals=[(j['started_at']//1000,j['started_at']//1000+j['arrival']['actual_s'],j) for j in jobs if j['started_at'] is not None]
   for t in range(0,2100):
    # Node lifecycle at t, after completions and actions at t; it serves [t,t+1).
    used=[0]*6;memory=[0]*6
    for start,end,j in running_intervals:
     if start<=t<end:used[j['node']]+=j['arrival']['cpu'];memory[j['node']]+=j['arrival']['memory_gib']
    for node,due in list(starting.items()):
     if due<=t:ready[node]=True;del starting[node]
    for node in list(draining):
     if used[node]==0:draining.remove(node)
    for change in by_t[t]:
     if change['action'].startswith('start'):
      n=2 if change['action']=='start_two' else 1
      off=[i for i in range(6) if not ready[i] and i not in starting and i not in draining]
      for i in off[:n]:starting[i]=t+60
     else:
      # Engine drains the ready node with least reserved CPU, then memory, then node ID.
      candidates=[i for i in range(6) if ready[i]]
      node=min(candidates,key=lambda i:(used[i],memory[i],i));ready[node]=False
      if used[node]>0:draining.add(node)
    active_integral.append(active_integral[-1]+sum(ready)+len(starting)+len(draining))
    cpu_integral.append(cpu_integral[-1]+sum(used));mem_integral.append(mem_integral[-1]+sum(memory))
   calls=missing=tokens=outputs=unpriced=0;cost=0.
   for line in (path.parent/f'decisions-{policy}-equal.jsonl').read_text().splitlines():
    d=json.loads(line);e=d['evidence'];perf=e['performance'];now=d['at']*1000
    assert e['current']['observed_at_ms']==now
    assert 'actual_s' not in json.dumps(e) and 'finish_at' not in json.dumps(e)
    for key,offset in [('recent_60s',0),('previous_60s',60000)]:
     w=perf[key];end=max(0,now-offset);start=max(0,end-60000)
     assert (w['from_ms_exclusive'],w['through_ms'],w['observed_seconds'])==(start,end,(end-start)/1000)
     completed=[j for at,j in outcomes if start<at<=end and j['phase']=='completed']
     rejected=sum(start<at<=end and j['phase']=='rejected' for at,j in outcomes)
     offered=sum(start<j['arrived_at']<=end for j in jobs)
     assert (w['offered'],w['completed'],w['rejected'])==(offered,len(completed),rejected)
     check_dist(w['completed_job_queue_wait_s'],[(j['started_at']-j['arrived_at'])/1000 for j in completed])
     check_dist(w['completed_job_latency_s'],[(j['started_at']-j['arrived_at'])/1000+j['arrival']['actual_s'] for j in completed])
     seconds=(end-start)/1000
     for name,count in [('offered',offered),('completed',len(completed)),('rejected',rejected)]:close(w[name+'_per_s'],count/seconds if seconds else None)
     total=len(completed)+rejected;close(w['rejected_fraction_of_terminal_outcomes'],rejected/total if total else None)
     i,j=start//1000,end//1000;nodes=active_integral[j]-active_integral[i]
     assert w['active_node_seconds']==nodes,(path,policy,now,key,w['active_node_seconds'],nodes)
     close(w['mean_active_nodes'],nodes/seconds if seconds else None)
     close(w['cpu_reservation_utilization'],(cpu_integral[j]-cpu_integral[i])/(8*nodes) if nodes else None)
     close(w['memory_reservation_utilization'],(mem_integral[j]-mem_integral[i])/(16*nodes) if nodes else None)
     windows+=1
    expected_changes=[dict(at_ms=x['at']*1000,action=x['action']) for x in changes if max(0,now-120000)<x['at']*1000<=now]
    assert perf['applied_capacity_changes_120s']==expected_changes
    result=d['result'];error=result['error'] or ''
    assert not any(code in error for code in ['typesafe_http_401','typesafe_http_402','typesafe_http_403'])
    if result['latency_ms']<=0:continue
    calls+=1
    if result['usage'] is None:missing+=1;continue
    u=result['usage'];tokens+=u['input_tokens'];outputs+=u['output_tokens']
    if result['model']=='jev-1.13.0':cost+=u['input_tokens']*.042/1e6
    else:unpriced+=1
   c=r['jev_cost'];assert (c['calls'],c['missing_usage_calls'],c['input_tokens'],c['output_tokens'],c['unpriced_calls'])==(calls,missing,tokens,outputs,unpriced)
   close(c['estimated_usd'],cost);checked+=1
 if args.trace:
  print(json.dumps(dict(checked_policy_runs=checked,independently_checked_windows=windows)));return
 replays=sum((NEW/f'replay-{s}.log').read_text().count('REPLAY VERIFIED') for s in range(1001,1011));assert replays==120
 report=read(NEW/'report/summary.json');rows=read(NEW/'report/per-trace.json');assert len(rows)==1560
 for p in report['policies']:
  data=[r for r in rows if r['policy']==p['policy']];assert len(data)==120
  for kind in ['wait','completion']:close(sum(r[kind+'_mean']*r['completed'] for r in data)/sum(r['completed'] for r in data),p[kind]['mean'])
 result=dict(traces=120,policy_runs=checked,exact_replay_traces=replays,independently_checked_windows=windows,checks=['frozen source and prompt hashes','byte-identical workloads and Toto forecasts','offered jobs and terminal outcome conservation','every feedback latency distribution and event-time count reconstructed from job journals','capacity and reservation integrals independently reconstructed from lifecycle events and job intervals','window boundaries and action timestamps','per-call cost reconciliation','exact evidence and summary replay','pooled report means reconcile'])
 (NEW/'report/validation.json').write_text(json.dumps(result,indent=2));print(json.dumps(result))
if __name__=='__main__':main()
