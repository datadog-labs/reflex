#!/usr/bin/env python3
"""Cost audit and prespecified diagnostic comparisons; reads recordings only."""
import collections,html,json,pathlib,statistics
root=pathlib.Path(__file__).resolve().parents[2]
base=root/'output'; study=base/'capacity-study'; report=study/'report'; report.mkdir(exist_ok=True)
def load(directory,mode):return [r for p in directory.glob(f'*/results-{mode}.json') for r in json.loads(p.read_text())]
primary=load(study,'equal'); lookup={(r['pattern'],r['load'],r['seed'],r['policy']):r for r in primary}
ops=load(base/'capacity-study-operational','measured'); oracle=load(base/'capacity-study-oracle','equal')
audit=[]
for directory,mode,pattern in [
    ('capacity-study','equal','*/decisions-jev*-equal.jsonl'),
    ('capacity-study/discarded-attempts/billing-2026-09-20','equal','*/decisions-jev*-equal.jsonl'),
    ('capacity-study-operational','measured','*/decisions-jev*-measured.jsonl'),
    ('capacity-study-preflight-forecast','equal','*/decisions-jev*-equal.jsonl'),
    ('capacity-study-key-check','equal','*/decisions-jev*-equal.jsonl'),
]:
    by_policy=collections.defaultdict(lambda:dict(calls=0,input_tokens=0,output_tokens=0,known_usd=0.,missing_usage=0,billing_declined=0,other_missing_usage=0,unpriced=0))
    for p in (base/directory).glob(pattern):
        policy=p.name.removeprefix('decisions-').removesuffix(f'-{mode}.jsonl');g=by_policy[policy]
        for line in p.read_text().splitlines():
            r=json.loads(line)['result']
            if r['latency_ms']<=0:continue
            g['calls']+=1;u=r['usage']
            if u is None:
                g['missing_usage']+=1
                g['billing_declined' if 'typesafe_http_402' in (r['error'] or '') else 'other_missing_usage']+=1
                continue
            g['input_tokens']+=u['input_tokens'];g['output_tokens']+=u['output_tokens']
            if r['model']=='jev-1.13.0':g['known_usd']+=u['input_tokens']*.042/1e6
            else:g['unpriced']+=1
    audit.extend(dict(stage=directory,policy=p,**v) for p,v in by_policy.items())
comparisons=[]
for r in ops:
    before=lookup.get((r['pattern'],r['load'],r['seed'],r['policy']))
    if before:comparisons.append(dict(pattern=r['pattern'],policy=r['policy'],equal_slo=before['slo_rate'],measured_slo=r['slo_rate'],slo_change_pp=100*(r['slo_rate']-before['slo_rate']),node_seconds_change=r['node_seconds']-before['node_seconds'],measured_latency_p95_ms=r['inference_latency_p95_ms'],guards=r['guard_rejections'],fallbacks=r['fallbacks']))
oracle_comparison=[]
for r in oracle:
    before=lookup.get((r['pattern'],r['load'],r['seed'],'toto_threshold'))
    if before:oracle_comparison.append(dict(pattern=r['pattern'],toto_slo=before['slo_rate'],perfect_forecast_slo=r['slo_rate'],toto_node_seconds=before['node_seconds'],perfect_forecast_node_seconds=r['node_seconds']))
result=dict(cost_audit=audit,all_stages_known_usd=sum(x['known_usd'] for x in audit),all_stages_calls=sum(x['calls'] for x in audit),all_stages_missing_usage=sum(x['missing_usage'] for x in audit),billing_declined_attempts=sum(x['billing_declined'] for x in audit),other_missing_usage_calls=sum(x['other_missing_usage'] for x in audit),operational=comparisons,perfect_forecast=oracle_comparison)
(report/'supplement.json').write_text(json.dumps(result,indent=2))
md=['# Additional diagnostics and inference cost audit','', '| Stage | Policy | Calls | Input tokens | Known estimated cost | Billing declined | Other missing usage |','|---|---|---:|---:|---:|---:|---:|']
md += [f'| {x["stage"]} | {x["policy"]} | {x["calls"]} | {x["input_tokens"]} | ${x["known_usd"]:.6f} | {x["billing_declined"]} | {x["other_missing_usage"]} |' for x in audit]
md += ['',f'Known estimated Jev cost across these stages: **${result["all_stages_known_usd"]:.6f}**. This includes successful calls in archived interrupted attempts and the funded-key check. It excludes unknown-price calls and calls without usage. Billing-declined HTTP 402 attempts are separated from other missing-usage responses; no charge is inferred for either category without usage. Accepted primary results exclude all interrupted attempts. Replay and the perfect-forecast diagnostic issue no inference requests. Operational runs reuse primary Toto forecasts; do not count those as new Toto RPCs. The primary forecast stage makes 105 calls per trace (12,600 across 120 traces); Toto monetary cost is unknown. Preflight includes additional forecast attempts, outside that primary total.','', '## Measured-latency sensitivity','', 'First evaluation seed, six moderate workloads, new live Jev calls. Variation includes new model responses as well as latency; this is not a pure causal latency comparison. Delays round up to one-second simulation ticks.','', '| Pattern | Policy | Equal-delay SLO | Measured-delay SLO | Change | Node-seconds change |','|---|---|---:|---:|---:|---:|']
md += [f'| {x["pattern"]} | {x["policy"]} | {x["equal_slo"]*100:.2f}% | {x["measured_slo"]*100:.2f}% | {x["slo_change_pp"]:+.2f} pp | {x["node_seconds_change"]:+} |' for x in comparisons]
md += ['','## Known-future diagnostic','','The reactive forecast controller receives exact future offered-demand series. This diagnoses forecast/policy interaction, not a deployable algorithm or proven optimal bound. First evaluation seed, moderate load.','','| Pattern | Toto SLO | Known-future SLO | Toto node-seconds | Known-future node-seconds |','|---|---:|---:|---:|---:|']
md += [f'| {x["pattern"]} | {x["toto_slo"]*100:.2f}% | {x["perfect_forecast_slo"]*100:.2f}% | {x["toto_node_seconds"]} | {x["perfect_forecast_node_seconds"]} |' for x in oracle_comparison]
(report/'SUPPLEMENT.md').write_text('\n'.join(md)+'\n')
print(json.dumps({k:v for k,v in result.items() if not isinstance(v,list)}))

# A standalone browser-readable supplement, with the same tables as the audit.
def table(headers,rows):
    return '<table><thead><tr>'+''.join('<th>'+html.escape(str(x))+'</th>' for x in headers)+'</tr></thead><tbody>'+''.join('<tr>'+''.join('<td>'+html.escape(str(x))+'</td>' for x in row)+'</tr>' for row in rows)+'</tbody></table>'
body='<h1>Diagnostics and full inference cost audit</h1><p><a href="index.html">Back to the study</a></p>'
body+=f'<p>Known estimated Jev cost across all stages: <b>${result["all_stages_known_usd"]:.6f}</b>. This includes successful calls in interrupted attempts and the funded-key check. HTTP 402 declined attempts: {result["billing_declined_attempts"]:,}; other calls without usage: {result["other_missing_usage_calls"]}. Their cost is unknown and excluded. Toto monetary cost is unknown.</p>'
body+=table(['Stage','Policy','Calls','Input tokens','Estimated USD','Billing declined','Other missing usage'],[[x['stage'],x['policy'],x['calls'],x['input_tokens'],f"${x['known_usd']:.6f}",x['billing_declined'],x['other_missing_usage']] for x in audit])
body+='<p>The original account ran out of credit. All affected or incomplete traces were archived and rerun with a funded key, keeping the same workloads, forecasts, model, prompt and policy settings. Archived attempts contribute to expense only. No billing-failure run contributes to accepted results. Replay and known-future diagnostics issue no inference requests.</p>'
body+='<h2>Measured-latency sensitivity</h2><p>First evaluation seed, six moderate workloads, new live Jev calls. Variation includes model responses as well as latency; this is not a pure causal latency comparison. Delays round up to one-second simulation ticks.</p>'
body+=table(['Pattern','Policy','Equal SLO','Measured SLO','SLO change','Node-seconds change'],[[x['pattern'],x['policy'],f"{x['equal_slo']*100:.2f}%",f"{x['measured_slo']*100:.2f}%",f"{x['slo_change_pp']:+.2f} pp",x['node_seconds_change']] for x in comparisons])
body+='<h2>Known-future diagnostic</h2><p>The reactive forecast controller receives exact future offered demand. This diagnoses forecast/policy interaction; it is not deployable or a proven optimal bound. First evaluation seed, moderate load.</p>'
body+=table(['Pattern','Toto SLO','Known-future SLO','Toto node-seconds','Known-future node-seconds'],[[x['pattern'],f"{x['toto_slo']*100:.2f}%",f"{x['perfect_forecast_slo']*100:.2f}%",x['toto_node_seconds'],x['perfect_forecast_node_seconds']] for x in oracle_comparison])
(report/'SUPPLEMENT.html').write_text('<!doctype html><html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Study diagnostics and cost audit</title><style>body{font:16px/1.6 system-ui,sans-serif;max-width:1200px;margin:40px auto;padding:0 24px;background:#f7f9fc;color:#18283b}table{border-collapse:collapse;width:100%;font-size:13px;background:white}th,td{padding:10px;text-align:right;border-bottom:1px solid #dce4ee}th{background:#eaf0f7}td:first-child,th:first-child{text-align:left}h2{margin-top:40px}a{color:#0369a1}</style><body>'+body+'</body></html>')
