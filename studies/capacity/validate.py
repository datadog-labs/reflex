#!/usr/bin/env python3
# Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
# This product includes software developed at Datadog (https://www.datadoghq.com/).
# Copyright 2026-present Datadog, Inc.

"""Independent outcome and cost-accounting checks against stored job records."""
import argparse,collections,json,pathlib,math
p=argparse.ArgumentParser();p.add_argument('root',type=pathlib.Path);p.add_argument('--expected-traces',type=int,default=120);a=p.parse_args()
paths=list(a.root.glob('*/results-equal.json'));assert len(paths)==a.expected_traces,(len(paths),a.expected_traces)
if a.expected_traces==120:
    assert {p.parent.name for p in paths}=={f'{pattern}-{load}-{seed}' for pattern in ['steady','spike','ramp','waves','bursts','mix'] for load in ['moderate','heavy'] for seed in range(1001,1011)}
expected={'fixed2','fixed6','reactive','hpa','ewma','persistence','toto_threshold','jev','jev_toto'}
calls=0;cost=0.;errors=0
for path in paths:
    for journal in path.parent.glob('decisions-jev*-equal.jsonl'):
        for line in journal.read_text().splitlines():
            error=json.loads(line)['result']['error'] or ''
            assert not any(code in error for code in ['typesafe_http_401','typesafe_http_402','typesafe_http_403']), (journal,error)
    rows=json.loads(path.read_text());assert {r['policy'] for r in rows}==expected
    workload=json.loads((path.parent/'workload.json').read_text());offered=sum(len(b['jobs']) for b in workload if 300000<b['at_ms']<=2100000)
    trace_ids=[j['id'] for b in workload if 300000<b['at_ms']<=2100000 for j in b['jobs']]
    forecasts=json.loads((path.parent/'forecasts.json').read_text());assert len(forecasts)==105
    for f in forecasts:
        assert f['input']['origin_ms']==f['at']*1000
        assert f['input']['timestamps'][-1]==1780000000+f['at']
        assert len(f['input']['values'])==256
        preceding=[b for b in workload if b['at_ms']<=f['at']*1000][-256:]
        assert f['input']['values']==[b['values'] for b in preceding]
        assert f['input']['timestamps']==[1780000000+b['at_ms']//1000 for b in preceding]
        for series in (f['snapshot'] or {}).get('series',[]):
            assert all(len(series[k])==120 for k in ['median','lower','upper'])
    for r in rows:
        assert r['offered']==offered
        assert r['completed']+r['rejected']+r['unfinished']==offered
        jobs=[j for j in json.loads((path.parent/f'jobs-{r["policy"]}-equal.json').read_text()) if 300000<j['arrived_at']<=2100000]
        assert [j['arrival']['id'] for j in jobs]==trace_ids
        phases=collections.Counter(j['phase'] for j in jobs)
        assert phases['completed']==r['completed'] and phases['rejected']==r['rejected']
        good=sum(j['phase']=='completed' and (j['started_at']-j['arrived_at'])/1000+j['arrival']['actual_s']<=j['arrival']['estimated_s']+20 for j in jobs)
        assert good==r['slo_success'] and math.isclose(r['slo_rate'],good/offered,abs_tol=1e-15)
        c=r['jev_cost'];assert c['priced_calls']+c['missing_usage_calls']+c['unpriced_calls']==c['calls']
        if c['unpriced_calls']==0:assert math.isclose(c['estimated_usd'],c['input_tokens']*.042/1e6,abs_tol=1e-12)
        assert 1800<=r['node_seconds']<=6*(1800+120)
        calls+=c['calls'];cost+=c['estimated_usd'];errors+=c['missing_usage_calls']
    # This baseline is mathematically equivalent under the specified forecast rule.
    reactive=next(r for r in rows if r['policy']=='reactive');persist=next(r for r in rows if r['policy']=='persistence')
    assert {k:v for k,v in reactive.items() if k!='policy'}=={k:v for k,v in persist.items() if k!='policy'}
result={'traces':len(paths),'policy_runs':len(paths)*9,'checked':'absence of billing/auth failures, job identities, cohort conservation, SLO arithmetic, forecast origins/shapes and historical-only inputs, cost arithmetic, capacity bounds, persistence equivalence','model_calls':calls,'known_jev_usd':cost,'missing_usage_calls':errors}
(a.root/'report/validation.json').write_text(json.dumps(result,indent=2));print(json.dumps(result))
