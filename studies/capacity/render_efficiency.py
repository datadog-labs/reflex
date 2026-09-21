#!/usr/bin/env python3
"""Render a standalone delay-first paper from the completed frozen follow-up."""
import ast,hashlib,json,math,pathlib,re,shutil,zipfile
import markdown
from interactive_charts import enhance, CSS as chart_css, JS as chart_js
from plot_patterns import render as render_patterns
from analyze_efficiency import ROOT,BASE,NEW,REPORT,LABELS,PATTERNS
SOURCE=ROOT/'studies/capacity'
s=json.loads((REPORT/'summary.json').read_text());rows=json.loads((REPORT/'per-trace.json').read_text())
validation=json.loads((REPORT/'validation.json').read_text())
assert s['new_policy_runs']==240 and validation['exact_replay_traces']==120
policies={p['policy']:p for p in s['policies']}
def table(headers,rows):
 return '\n'.join(['| '+' | '.join(headers)+' |','| '+' | '.join('---' for _ in headers)+' |',*['| '+' | '.join(map(str,row))+' |' for row in rows]])
def num(v):return '—' if v is None else f'{v:.2f}'
def ci(v,scale=1,decimals=2):return f'{v[0]*scale:+.{decimals}f} [{v[1]*scale:+.{decimals}f}, {v[2]*scale:+.{decimals}f}]'
def delta(a,b,key):return 100*(a[key]/b[key]-1)
def comparison(p,b):return next(c for c in s['comparisons'] if c['policy']==p and c['baseline']==b)
queue=table(['Policy','Mean (s)','p25 (s)','p50 (s)','p99 (s)','Rejected','Unfinished'],[[LABELS[p['policy']],*[num(p['wait'][k]) for k in ['mean','p25','p50','p99']],f"{100*p['rejection_rate']:.2f}%",f"{p['unfinished']:,}"] for p in s['policies']])
completion=table(['Policy','Mean (s)','p25 (s)','p50 (s)','p99 (s)'],[[LABELS[p['policy']],*[num(p['completion'][k]) for k in ['mean','p25','p50','p99']]] for p in s['policies']])
resource=table(['Policy','Mean node-hours','CPU reserved','Memory reserved','Starts','Drains','Reversals ≤120 s'],[[LABELS[p['policy']],f"{p['mean_node_hours']:.3f}",f"{p['cpu_utilization']*100:.1f}%",f"{p['memory_utilization']*100:.1f}%",p['starts'],p['drains'],p['reversals']] for p in s['policies']])
paired=table(['Comparison','Mean queue change (s)','Node-hours change','Rejection change (pp)'],[[LABELS[c['policy']]+' − '+LABELS[c['baseline']],ci(c['wait_mean']),ci(c['node_hours'],decimals=3),ci(c['reject_rate'],100)] for c in s['comparisons']])
inference=table(['Policy','Calls','Input tokens','Known cost','Missing usage','Unpriced calls'],[[LABELS[p['policy']],f"{p['calls']:,}",f"{p['input_tokens']:,}",f"${p['known_jev_usd']:.6f}",p['missing_usage_calls'],p['unpriced_calls']] for p in s['policies'] if p['policy'].startswith('jev')])
workload=[];rejections=[]
for pattern in PATTERNS:
 for load in ['moderate','heavy']:
  cells=[[r for r in rows if r['pattern']==pattern and r['load']==load and r['policy']==p] for p in LABELS]
  vals=[sum(r['wait_mean']*r['completed'] for r in cell)/sum(r['completed'] for r in cell) for cell in cells]
  best=min(vals)
  workload.append([pattern,load,*[f'**{v:.2f}**' if math.isclose(v,best,rel_tol=0,abs_tol=1e-12) else f'{v:.2f}' for v in vals]])
  rejections.append([pattern,load,*[f"{100*sum(r['rejected'] for r in cell)/sum(r['offered'] for r in cell):.2f}%" for cell in cells]])
a=policies['jev_efficiency'];b=policies['jev'];at=policies['jev_toto_efficiency'];bt=policies['jev_toto'];h=policies['hpa']
scaling=' '.join(f"{LABELS[p['policy']]} used {p['mean_node_hours']:.3f} node-hours per trace, a {abs(delta(p,old,'mean_node_hours')):.1f}% {'reduction' if p['mean_node_hours']<old['mean_node_hours'] else 'increase'} from its original instruction. Applied drains changed from {old['drains']:,} to {p['drains']:,}, while rapid direction reversals changed from {old['reversals']:,} to {p['reversals']:,}." for p,old in [(a,b),(at,bt)])
scaling+=f" The new Jev arms averaged {a['drains']/a['traces']:.1f} and {at['drains']/at['traces']:.1f} drains per trace, respectively, with {a['reversals']/a['traces']:.1f} and {at['reversals']/at['traces']:.1f} rapid reversals. This measures control activity as well as resource savings: repeated downscale/upscale cycles can incur startup penalties."
abstract=f"With the efficiency-aware instruction, Jev used {a['mean_node_hours']:.3f} node-hours per trace versus {b['mean_node_hours']:.3f} originally; pooled mean queue wait was {a['wait']['mean']:.2f} versus {b['wait']['mean']:.2f} seconds, and rejection was {100*a['rejection_rate']:.2f}% versus {100*b['rejection_rate']:.2f}%. With Toto evidence, the corresponding capacity values were {at['mean_node_hours']:.3f} versus {bt['mean_node_hours']:.3f} node-hours."
tradeoff='\n\n'.join(f"For {LABELS[p]}, the paired mean queue change was {ci(comparison(p,old)['wait_mean'])} seconds and the node-hour change was {ci(comparison(p,old)['node_hours'],decimals=3)}. The rejection change was {ci(comparison(p,old)['reject_rate'],100)} percentage points. These intervals compare the new instruction with the corresponding original arm on the same traces." for p,old in [('jev_efficiency','jev'),('jev_toto_efficiency','jev_toto')])
tradeoff+=f"\n\nAgainst the HPA-inspired baseline, efficiency-aware Jev used {a['mean_node_hours']:.3f} versus {h['mean_node_hours']:.3f} node-hours and had {a['macro_mean_wait']:.2f} versus {h['macro_mean_wait']:.2f} seconds of mean per-trace queue wait; pooled rejection was {100*a['rejection_rate']:.2f}% versus {100*h['rejection_rate']:.2f}%. Table 4 gives paired intervals. Forecast evidence changed the new Jev arm's mean node-hours by {ci(comparison('jev_toto_efficiency','jev_efficiency')['node_hours'],decimals=3)} and mean per-trace queue wait by {ci(comparison('jev_toto_efficiency','jev_efficiency')['wait_mean'])} seconds. No single composite score ranks these different operating points."
if a['mean_node_hours']<b['mean_node_hours'] and a['macro_mean_wait']>b['macro_mean_wait'] and a['rejection_rate']>b['rejection_rate']:
 tradeoff+='\n\nThe new Jev instruction traded service quality for lower capacity consumption: queues grew longer and more work was rejected. This is a different operating point, rather than an improvement that preserves the original service quality. A numerical delay/loss budget and comparisons at matched capacity would be needed to judge whether the savings are worthwhile.'
new_errors=sum(sum(p['errors'].values()) for p in [a,at])
cost=f"The 240 new policy runs incurred ${s['new_known_jev_usd']:.6f} in known estimated Jev charges. The follow-up preflight added ${s['preflight_known_jev_usd']:.6f}; the prior study's all-stage total was ${s['prior_all_stages_known_jev_usd']:.6f}. Cumulative known estimated Jev cost is therefore ${s['all_stages_known_jev_usd']:.6f}. There were {a['missing_usage_calls']+at['missing_usage_calls']} new calls without usage, {a['unpriced_calls']+at['unpriced_calls']} unpriced calls, and {new_errors} recorded evaluation failures. Failed evaluations used the recorded fallback; missing charges cannot be reconstructed from token usage. No new Toto requests were made. These figures exclude unknown Toto cost and infrastructure prices."
conclusion=f"Explicit efficiency guidance {'did' if a['drains']>b['drains'] else 'did not'} increase Jev's observed scale-down activity. {abstract} The direct delay tables expose the resulting service-quality tradeoff more clearly than a single deadline-success percentage. Selecting a preferred policy still requires a system owner to specify acceptable delay, loss, and resource cost; this qualitative prompt experiment does not determine that preference or establish an optimum."
values=dict(abstract_result=abstract,queue_table=queue,completion_table=completion,resource_table=resource,scaling_result=scaling,paired_table=paired,tradeoff_result=tradeoff,inference_table=inference,cost_result=cost,conclusion=conclusion,workload_delay_table=table(['Workload','Load',*LABELS.values()],workload),workload_rejection_table=table(['Workload','Load',*LABELS.values()],rejections),prompt=(SOURCE/'prompts/efficiency-v1.txt').read_text().strip().replace('\n\n','\n>\n> '))
text=(SOURCE/'EFFICIENCY_PAPER.template.md').read_text().replace('initial-paper.html','initial/index.html')
for key,value in values.items():text=text.replace('{{'+key+'}}',value)
assert '{{' not in text
(SOURCE/'EFFICIENCY_PAPER.md').write_text(text);(REPORT/'PAPER.md').write_text(text)
md=markdown.Markdown(extensions=['tables','fenced_code','toc','sane_lists']);body=md.convert(text)
body=re.sub(r'(<table>.*?</table>)',r'<div class="table-wrap">\1</div>',body,flags=re.S)
render_patterns(REPORT)
body=enhance(body,REPORT)
source_ast=ast.parse((SOURCE/'render_paper.py').read_text())
css=next(ast.literal_eval(n.value) for n in source_ast.body if isinstance(n,ast.Assign) and any(isinstance(t,ast.Name) and t.id=='css' for t in n.targets))
body=body.replace('<h2 id="1-motivation-and-research-question">','<details class="contents"><summary>Contents</summary>'+md.toc+'</details><h2 id="1-motivation-and-research-question">',1)
css+=chart_css
page='<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Balancing Delay and Capacity in Model-Guided Autoscaling</title><style>'+css+'</style></head><body><nav class="toolbar"><span>EFFICIENCY FOLLOW-UP</span><a href="initial/index.html">Original study</a><a href="PAPER.md">Manuscript</a><a href="per-trace.csv">Per-trace data</a><a href="validation.json">Validation</a></nav><main>'+body+'<footer>120 paired traces · 240 new policy runs · 1,320 comparison outcomes · Exact replay verified</footer></main><script>'+chart_js+'</script></body></html>'
(REPORT/'index.html').write_text(page);(REPORT/'paper.html').write_text(page)
shutil.copy2(SOURCE/'EFFICIENCY_PROTOCOL.md',REPORT/'EFFICIENCY_PROTOCOL.md')
shutil.copytree(BASE/'report',REPORT/'initial',dirs_exist_ok=True)
files=[SOURCE/x for x in ['analyze_efficiency.py','run_efficiency.py','validate_efficiency.py','render_efficiency.py','interactive_charts.py','interactive_charts.js','package_efficiency.py','plot_patterns.py','replay_efficiency_when_ready.py','README.md','EFFICIENCY_PROTOCOL.md','EFFICIENCY_PAPER.template.md','EFFICIENCY_PAPER.md','prompts/efficiency-v1.txt']]+[ROOT/'crates/reflex-sim/examples/capacity_study.rs']
provenance=dict(source_sha256={str(p.relative_to(ROOT)):hashlib.sha256(p.read_bytes()).hexdigest() for p in files},prompt_sha256=hashlib.sha256((SOURCE/'prompts/efficiency-v1.txt').read_bytes()).hexdigest(),validation=validation,cost={k:v for k,v in s.items() if k.endswith('usd') or k=='new_toto_calls'})
(REPORT/'REPRODUCIBILITY.json').write_text(json.dumps(provenance,indent=2))
with zipfile.ZipFile(ROOT/'output/capacity-study-efficiency-report.zip','w',zipfile.ZIP_DEFLATED) as z:
 for p in sorted(REPORT.rglob('*')):
  if p.is_file():z.write(p,pathlib.Path('capacity-study-efficiency-report')/p.relative_to(REPORT))
print(json.dumps(dict(paper=str(REPORT/'index.html'),words=len(text.split()))))
