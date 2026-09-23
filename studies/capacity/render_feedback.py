#!/usr/bin/env python3
# Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
# This product includes software developed at Datadog (https://www.datadoghq.com/).
# Copyright 2026-present Datadog, Inc.

"""Render the completed feedback study, with selectable series and traffic overlays."""
import ast,hashlib,html,json,pathlib,re,shutil,statistics,zipfile
import markdown
from analyze_feedback import ROOT,BASE,EFF,NEW,REPORT,LABELS,COLORS,SELECTED
from interactive_charts import CSS as chart_css,JS as chart_js
SOURCE=ROOT/'studies/capacity'
def table(headers,rows):return '\n'.join(['| '+' | '.join(headers)+' |','| '+' | '.join(['---']*len(headers))+' |',*['| '+' | '.join(map(str,r))+' |' for r in rows]])
def ci(v,scale=1,digits=2):return f'{v[0]*scale:+.{digits}f} [{v[1]*scale:+.{digits}f}, {v[2]*scale:+.{digits}f}]'
def widgets(body):
 configs={'delay-capacity':list(LABELS),'delay-distributions':['fixed6',*SELECTED],'capacity-moderate':SELECTED,'capacity-heavy':SELECTED}
 for name,keys in configs.items():
  svg=(REPORT/(name+'.svg')).read_text();svg=svg[svg.index('<svg'):]
  svg=re.sub(r'id="([^"]+)"',lambda m:'id="'+name+'--'+m[1]+'"',svg)
  svg=re.sub(r'((?:xlink:)?href=")#([^" ]+)',lambda m:m[1]+'#'+name+'--'+m[2],svg)
  svg=re.sub(r'url\(#([^)]+)\)',lambda m:'url(#'+name+'--'+m[1]+')',svg)
  svg=re.sub(r'<g id="'+name+r'--series--([^" ]+)--([^" ]+)">',lambda m:'<g id="'+name+'--series--'+m[1]+'--'+m[2]+'" data-series="'+m[1]+'">',svg)
  svg=re.sub(r'<g id="'+name+r'--overlay--([^" ]+)--([^" ]+)">',lambda m:'<g id="'+name+'--overlay--'+m[1]+'--'+m[2]+'" data-workload="'+m[1]+'">',svg)
  assert set(re.findall(r'data-series="([^"]+)"',svg))==set(keys)
  buttons=''.join(f'<button type="button" data-pick="{p}" aria-pressed="true"><span class="chart-swatch" style="--series-color:{COLORS[p]}"></span>{html.escape(LABELS[p])}</button>' for p in keys)
  overlay=''
  if name.startswith('capacity'):
   assert len(re.findall('data-workload=',svg))==18
   overlay='<label class="workload-control">Workload overlay <select data-workload-select><option value="none">Off</option><option value="traffic" selected>Traffic — requests / s</option><option value="cpu">CPU demand — CPU-seconds / s</option><option value="memory">Memory demand — GiB-seconds / s</option></select></label><p class="chart-help">Dotted gray line, right axis: offered workload in 30-second bins. Left axis: active nodes.</p>'
  widget=f'<div class="interactive-chart" data-chart="{name}" hidden><div class="chart-help">Click to focus a series, then add or remove others. Show all restores the comparison.</div>{overlay}<div class="chart-legend" role="group" aria-label="Choose series">{buttons}<button type="button" data-reset>Show all</button></div><div class="chart-status" aria-live="polite">Showing all series</div>{svg}</div>'
  body,count=re.subn(r'<img src="'+name+r'\.png"([^>]*)>',lambda m:widget+'<img class="chart-fallback" src="'+name+'.png"'+m[1]+'>',body);assert count==1
 return body

def main():
 s=json.loads((REPORT/'summary.json').read_text());v=json.loads((REPORT/'validation.json').read_text());rows=json.loads((REPORT/'per-trace.json').read_text())
 assert s['new_policy_runs']==v['policy_runs']==240 and v['exact_replay_traces']==120
 policies={p['policy']:p for p in s['policies']}
 def comparison(p,b):return next(c for c in s['comparisons'] if c['policy']==p and c['baseline']==b)
 pairs=[('jev_feedback','jev_efficiency'),('jev_toto_feedback','jev_toto_efficiency')]
 sentences=[];details=[];interpret=[]
 for p,b in pairs:
  x,y=policies[p],policies[b]
  resource_change=100*(x['mean_node_hours']/y['mean_node_hours']-1)
  sentence=f"{LABELS[p]} used {x['mean_node_hours']:.3f} active node-hours per trace ({resource_change:+.1f}% versus {LABELS[b]}), with pooled mean queue wait {x['wait']['mean']:.2f} versus {y['wait']['mean']:.2f} seconds and rejection {100*x['rejection_rate']:.2f}% versus {100*y['rejection_rate']:.2f}%."
  sentences.append(sentence)
  details.append(sentence+f" Pooled mean arrival-to-completion time changed from {y['completion']['mean']:.2f} to {x['completion']['mean']:.2f} seconds. Applied drains changed from {y['drains']:,} to {x['drains']:,}; rapid direction reversals changed from {y['reversals']:,} to {x['reversals']:,}.")
  c=comparison(p,b)
  interpret.append(f"For {LABELS[p]}, the paired queue-wait change is {ci(c['wait_mean'])} seconds; capacity changes by {ci(c['node_hours'],digits=3)} node-hours and rejection by {ci(c['reject_rate'],100)} percentage points. These paired intervals use equal trace weights, unlike the pooled latency tables.")
  if x['mean_node_hours']>y['mean_node_hours'] and x['wait']['mean']<y['wait']['mean']:
   interpret.append('This arm purchases shorter mean delay with more capacity; it does not demonstrate lower cost at unchanged service quality.')
  elif x['mean_node_hours']<y['mean_node_hours'] and x['wait']['mean']>y['wait']['mean']:
   interpret.append('This arm trades longer mean delay for less capacity; the acceptable operating point depends on an explicit service-quality budget.')
  elif x['mean_node_hours']<=y['mean_node_hours'] and x['wait']['mean']<=y['wait']['mean'] and x['rejection_rate']<=y['rejection_rate']:
   interpret.append('The aggregate point estimates improve or preserve all three measures against this prior arm; fresh held-out traces and repeated model draws would be needed to confirm that pattern.')
  else:interpret.append('Delay, capacity, and rejection do not provide a uniform ranking against the corresponding prior arm.')
 todo=comparison('jev_toto_feedback','jev_feedback')
 interpret.append(f"Within the new feedback experiment, adding Toto changes mean per-trace queue wait by {ci(todo['wait_mean'])} seconds, node-hours by {ci(todo['node_hours'],digits=3)}, and rejection by {ci(todo['reject_rate'],100)} percentage points. This comparison uses identical forecast records across the earlier and new forecast-enabled arms.")
 queue=table(['Policy','Mean','p25','p50','p99'],[[LABELS[p['policy']],*[f'{p['wait'][k]:.2f}' for k in ['mean','p25','p50','p99']]] for p in s['policies']])
 completion=table(['Policy','Mean','p25','p50','p99'],[[LABELS[p['policy']],*[f'{p['completion'][k]:.2f}' for k in ['mean','p25','p50','p99']]] for p in s['policies']])
 resource=table(['Policy','Node-hours/trace','CPU reserved','Memory reserved','Rejected','Unfinished','Drains','Reversals'],[[LABELS[p['policy']],f'{p['mean_node_hours']:.3f}',f'{100*p['cpu_utilization']:.1f}%',f'{100*p['memory_utilization']:.1f}%',f'{100*p['rejection_rate']:.2f}%',p['unfinished'],p['drains'],p['reversals']] for p in s['policies']])
 paired=table(['Policy − baseline','Mean queue wait (s)','Node-hours','Rejection (pp)'],[[LABELS[c['policy']]+' − '+LABELS[c['baseline']],ci(c['wait_mean']),ci(c['node_hours'],digits=3),ci(c['reject_rate'],100)] for c in s['comparisons']])
 new=[policies[p] for p,_ in pairs];known=sum(p['known_jev_usd'] for p in new);missing=sum(p['missing_usage_calls'] for p in new);errors=sum(sum(p['errors'].values()) for p in new);calls=sum(p['calls'] for p in new)
 cost=f"The new arms made {calls:,} Jev calls, with ${known:.6f} in known estimated charges. There were {errors} evaluation failures using the existing fallback, {missing} calls without usage, and {sum(p['unpriced_calls'] for p in new)} calls with unpriced model identifiers. No billing/authentication failure occurred. There were no additional paid preflight runs. Including earlier experiments, cumulative known estimated Jev charges are ${s['all_stages_known_jev_usd']:.6f}."
 costtable=table(['Arm','Calls','Input tokens','Known estimated USD','Missing usage'],[[LABELS[p['policy']],f'{p['calls']:,}',f'{p['input_tokens']:,}',f"${p['known_jev_usd']:.6f}",p['missing_usage_calls']] for p in s['policies'] if p['policy'].startswith('jev')])
 workloads=[]
 for pattern in ['steady','spike','ramp','waves','bursts','mix']:
  for load in ['moderate','heavy']:
   vals=[statistics.mean(r['wait_mean'] for r in rows if r['pattern']==pattern and r['load']==load and r['policy']==p) for p in LABELS]
   texts=[f'{value:.2f}' for value in vals];best=min(float(t) for t in texts)
   workloads.append([pattern,load,*[f'**{t}**' if float(t)==best else t for t in texts]])
 values=dict(abstract_result=' '.join(sentences),result_description='\n\n'.join(details),queue_table=queue,completion_table=completion,resource_table=resource,paired_table=paired,interpretation='\n\n'.join(interpret),cost_result=cost,cost_table=costtable,workload_table=table(['Workload','Load',*LABELS.values()],workloads),validation_result=f"All {v['exact_replay_traces']} traces replayed with exact evidence and summary equality, without inference calls. Independent checks reconstructed {v['independently_checked_windows']:,} feedback windows from final job journals and lifecycle events, including every latency distribution, event-time count, resource integral, and action timestamp. Workload and forecast files match the original study byte-for-byte; terminal outcome counts and per-call charges reconcile.",conclusion='Recent performance feedback is now an explicit, auditable part of the controller input. The observed results above describe its operating point under this objective; they do not establish an optimal latency–capacity balance or replace the need to specify acceptable loss and delay.')
 text=(SOURCE/'FEEDBACK_PAPER.template.md').read_text()
 for key,value in values.items():text=text.replace('{{'+key+'}}',value)
 assert '{{' not in text
 (SOURCE/'FEEDBACK_PAPER.md').write_text(text);(REPORT/'PAPER.md').write_text(text)
 md=markdown.Markdown(extensions=['tables','fenced_code','toc','sane_lists']);body=md.convert(text);body=re.sub(r'(<table>.*?</table>)',r'<div class="table-wrap">\1</div>',body,flags=re.S);body=widgets(body)
 ast_source=ast.parse((SOURCE/'render_paper.py').read_text());css=next(ast.literal_eval(n.value) for n in ast_source.body if isinstance(n,ast.Assign) and any(isinstance(t,ast.Name) and t.id=='css' for t in n.targets))
 page='<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Performance Feedback in Model-Guided Autoscaling</title><style>'+css+chart_css+'</style></head><body><nav class="toolbar"><span>PERFORMANCE FEEDBACK STUDY</span><a href="PAPER.md">Manuscript</a><a href="per-trace.csv">Data</a><a href="validation.json">Validation</a></nav><main>'+body+'<footer>120 paired traces · 240 new outcomes · 1,560 comparison outcomes · Exact replay verified</footer></main><script>'+chart_js+'</script></body></html>'
 (REPORT/'index.html').write_text(page)
 for source,dest in [('EFFICIENCY_PAPER.md','EFFICIENCY_PAPER.md'),('FEEDBACK_PROTOCOL.md','FEEDBACK_PROTOCOL.md'),('prompts/feedback-v1.txt','feedback-v1.txt')]:shutil.copy2(SOURCE/source,REPORT/dest)
 files=['analyze_feedback.py','validate_feedback.py','render_feedback.py','FEEDBACK_PAPER.template.md','FEEDBACK_PAPER.md']
 (REPORT/'REPRODUCIBILITY.json').write_text(json.dumps(dict(study_sources=json.loads((NEW/'source-sha256.json').read_text()),report_sources={name:hashlib.sha256((SOURCE/name).read_bytes()).hexdigest() for name in files},validation=v),indent=2))
 with zipfile.ZipFile(ROOT/'output/capacity-study-feedback-report.zip','w',zipfile.ZIP_DEFLATED) as z:
  for p in sorted(REPORT.iterdir()):
   if p.is_file():z.write(p,pathlib.Path('capacity-study-feedback-report')/p.name)
 print(json.dumps(dict(report=str(REPORT/'index.html'),words=len(text.split()))))
if __name__=='__main__':main()
