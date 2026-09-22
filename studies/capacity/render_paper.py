#!/usr/bin/env python3
"""Render the paper from its manuscript and the completed study's recorded results."""
import collections
import hashlib
import html
import json
import math
from pathlib import Path
import re
import statistics
import zipfile
import markdown
from plot_patterns import render as render_patterns

ROOT = Path(__file__).resolve().parents[2]
STUDY = ROOT / 'output/capacity-study'
REPORT = STUDY / 'report'
SOURCE = ROOT / 'studies/capacity'
render_patterns()
summary = json.loads((REPORT / 'summary.json').read_text())
supplement = json.loads((REPORT / 'supplement.json').read_text())
assert summary['complete'] and summary['completed_traces'] == 120
assert not summary['blocked_by_payment']
policies = summary['policies']
labels = dict(zip(['fixed2','fixed6','reactive','hpa','ewma','persistence','toto_threshold','jev','jev_toto'], ['Fixed 2','Fixed 6','Reactive','HPA-inspired','EWMA','Persistence','Toto + reactive','Jev','Jev + Toto']))

def table(headers, rows):
    return '\n'.join(['| '+' | '.join(headers)+' |','| '+' | '.join('---' for _ in headers)+' |', *['| '+' | '.join(str(v) for v in row)+' |' for row in rows]])

aggregate = table(['Policy','SLO success','95% interval','Mean node-hours','Mean run p95 wait','Jev cost'], [[labels[p['policy']], f"{p['slo'][0]*100:.2f}%", f"{p['slo'][1]*100:.2f}–{p['slo'][2]*100:.2f}%", f"{p['node_seconds'][0]/3600:.3f}", f"{p['mean_wait_p95']:.1f} s", f"${p['cost']:.5f}"] for p in policies])
paired = []
for c in summary['comparisons']:
    s = [x*100 for x in c['slo_difference']]
    n = [x/3600 for x in c['node_seconds_difference']]
    paired.append([labels[c['policy']]+' − '+labels[c['baseline']], f'{s[0]:+.2f} pp [{s[1]:+.2f}, {s[2]:+.2f}]', f'{n[0]:+.3f} h [{n[1]:+.3f}, {n[2]:+.3f}]'])
paired = table(['Comparison','SLO difference','Node-hours difference'], paired)
raw = [r for p in STUDY.glob('*/results-equal.json') for r in json.loads(p.read_text())]
workloads = []
for pattern in ['steady','spike','ramp','waves','bursts','mix']:
    for load in ['moderate','heavy']:
        values = [statistics.mean(r['slo_rate']*100 for r in raw if r['policy']==p and r['pattern']==pattern and r['load']==load) for p in labels]
        best = max(values)
        formatted = [('**'+f'{v:.2f}%'+ '**') if math.isclose(v,best,rel_tol=0,abs_tol=1e-12) else f'{v:.2f}%' for v in values]
        workloads.append([pattern,load,*formatted])
workloads = table(['Workload','Load',*labels.values()], workloads)
forecast_rows = []
for series in ['jobs','cpu','memory']:
    for horizon in ['1–30s','31–60s','61–120s']:
        cells = [c for c in summary['forecasts']['cells'] if c['series']==series and c['horizon']==horizon]
        avg = lambda k: statistics.mean(c[k] for c in cells)
        forecast_rows.append([series,horizon,f"{avg('mae'):.3f}",f"{avg('persistence_mae'):.3f}",f"{avg('median_persistence_mae'):.3f}",f"{avg('coverage')*100:.1f}%"])
forecasts = table(['Series','Horizon','Toto MAE','Mean persistence MAE','Median persistence MAE','Coverage'], forecast_rows)
stages = collections.defaultdict(float)
for item in supplement['cost_audit']:
    stages[item['stage']] += item['known_usd']
stage_names = {'capacity-study':'Accepted primary study','capacity-study/discarded-attempts/billing-2026-09-20':'Discarded interrupted attempts','capacity-study-operational':'Measured-latency sensitivity','capacity-study-preflight-forecast':'Preflight','capacity-study-key-check':'Funded-key validation'}
costs = table(['Stage','Known estimated Jev cost'], [[stage_names[k],f'${v:.6f}'] for k,v in stages.items()] + [['Total',f"${supplement['all_stages_known_usd']:.6f}"]])
manifest = json.loads((STUDY / 'manifest-1001-equal.json').read_text()) if (STUDY / 'manifest-1001-equal.json').exists() else None
if manifest is None:
    candidates = list(STUDY.glob('*manifest*1001*'))
    manifest = next(json.loads(p.read_text()) for p in candidates if 'prompt' in json.loads(p.read_text()))
prompt = manifest['prompt']
text = (SOURCE / 'PAPER.template.md').read_text()
for key,value in dict(aggregate_table=aggregate,paired_table=paired,workload_table=workloads,forecast_table=forecasts,cost_table=costs,prompt=prompt).items():
    text = text.replace('{{'+key+'}}',value)
assert '{{' not in text
# Interpretive prose refers to this fixed dataset; do not silently reuse it for another study.
assert round(next(p for p in policies if p['policy']=='jev')['slo'][0]*100,2)==88.33
assert round(next(p for p in policies if p['policy']=='jev_toto')['slo'][0]*100,2)==88.13
(SOURCE / 'PAPER.md').write_text(text)
(REPORT / 'PAPER.md').write_text(text)
md = markdown.Markdown(extensions=['tables','fenced_code','toc','sane_lists'])
body = md.convert(text)
body = re.sub(r'(<table>.*?</table>)',r'<div class="table-wrap">\1</div>',body,flags=re.S)
css = '''
:root{color-scheme:light}*{box-sizing:border-box}body{margin:0;background:#f4f5f6;color:#202832;font:18px/1.75 Georgia,"Times New Roman",serif}a{color:#245b88;text-underline-offset:3px}.toolbar{background:#142f43;color:white;padding:14px 24px;font:13px/1.6 system-ui,sans-serif;display:flex;flex-wrap:wrap;gap:10px 25px}.toolbar a{color:#e4f1fa}main{max-width:1100px;background:white;margin:28px auto;padding:58px 64px 72px;box-shadow:0 2px 16px #142f430d}h1,h2,h3{font-family:system-ui,sans-serif;color:#142f43;line-height:1.25}h1{font-size:38px;letter-spacing:-1px;margin:0 0 12px}h2{font-size:25px;margin:48px 0 18px;padding-top:10px;border-top:1px solid #dce2e6}h3{font-size:19px;margin:30px 0 10px}.subtitle{font:22px/1.4 system-ui,sans-serif;color:#536b7b;margin:10px 0}.byline{font:12px/1.5 system-ui,sans-serif;text-transform:uppercase;letter-spacing:1px;color:#71818c;margin:18px 0 32px}p{margin:15px 0}strong{font-weight:700}li{margin:8px 0}.contents{margin:28px 0;border:1px solid #dce2e6;padding:14px 20px;background:#f8fafb;font:14px/1.6 system-ui,sans-serif}.contents summary{cursor:pointer;font-weight:650}.toc ul{padding-left:20px}.toc li{margin:4px 0}.table-wrap{overflow-x:auto;margin:20px 0 28px}table{border-collapse:collapse;width:100%;font:13px/1.5 system-ui,sans-serif;font-variant-numeric:tabular-nums}th{background:#eaf0f4;text-align:left;font-weight:650;border-top:2px solid #486373;border-bottom:1px solid #9aaeba}td{border-bottom:1px solid #dde4e9}td,th{padding:10px 12px;vertical-align:top}table strong{font-weight:800}figure{margin:28px 0}figure img{display:block;width:100%;height:auto}figcaption{font:13px/1.6 system-ui,sans-serif;color:#4a5e6c;margin-top:12px}.architecture{display:flex;align-items:center;flex-wrap:wrap;gap:10px;padding:20px;background:#f5f8fa;border:1px solid #dce2e6;font:14px/1.5 system-ui,sans-serif}.architecture div{flex:1 1 145px;text-align:center;background:white;border:1px solid #b5c6d2;padding:14px 8px}.architecture figcaption{flex-basis:100%}.architecture small{font-size:11px;color:#526776}code{font:0.82em/1.5 ui-monospace,monospace;background:#f1f4f6;padding:2px 4px;overflow-wrap:anywhere}blockquote{border-left:3px solid #8aa6b9;margin:24px 0;padding:4px 22px;background:#f8fafb;font-size:16px}#abstract+p{font-size:18px}footer{font:13px/1.6 system-ui,sans-serif;border-top:1px solid #ccd7df;margin-top:40px;padding-top:18px;color:#647784}@media(max-width:760px){main{margin:0;padding:30px 22px}body{font-size:17px}h1{font-size:30px}h2{font-size:23px}.subtitle{font-size:19px}.architecture>span{display:none}}@media print{body{background:white;font-size:11pt}main{box-shadow:none;margin:0;padding:0;max-width:none}.toolbar,.contents{display:none}h1{font-size:24pt}h2{font-size:17pt;break-after:avoid}h3{break-after:avoid}figure{break-inside:avoid}table{font-size:8pt}.table-wrap{overflow:visible}a{color:inherit}p{orphans:3;widows:3}}
'''
# Keep the document title and abstract first; contents follows the abstract.
section = '<details class="contents"><summary>Contents</summary>'+md.toc+'</details>'
body = body.replace('<h2 id="1-introduction">',section+'<h2 id="1-introduction">',1)
page = '<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Model-Guided Autoscaling Under Deterministic Constraints</title><style>'+css+'</style></head><body><nav class="toolbar"><span>RESEARCH PAPER</span><a href="explorer.html">Data and figures</a><a href="PAPER.md">Manuscript (Markdown)</a><a href="SUPPLEMENT.html">Diagnostic supplement</a><a href="runs.csv">Per-run data (CSV)</a></nav><main>'+body+'<footer>Study artifact · 120 paired traces · 1,080 policy runs · Exact replay verified</footer></main></body></html>'
(REPORT / 'index.html').write_text(page)
(REPORT / 'paper.html').write_text(page)
for record_path in [STUDY/'provenance/completion.json',REPORT/'REPRODUCIBILITY.json']:
    record = json.loads(record_path.read_text())
    for path in [SOURCE/'analyze.py',SOURCE/'render_paper.py',SOURCE/'plot_patterns.py',SOURCE/'PAPER.template.md',SOURCE/'PAPER.md',SOURCE/'README.md',SOURCE/'report-requirements.txt']:
        record['source_sha256'][str(path.relative_to(ROOT))] = hashlib.sha256(path.read_bytes()).hexdigest()
    record_path.write_text(json.dumps(record,indent=2))
with zipfile.ZipFile(ROOT/'output/capacity-study-report.zip','w',zipfile.ZIP_DEFLATED) as archive:
    for path in sorted(REPORT.iterdir()):
        if path.is_file(): archive.write(path,Path('capacity-study-report')/path.name)
print(json.dumps(dict(paper=str(REPORT/'index.html'),manuscript=str(SOURCE/'PAPER.md'),words=len(text.split()))))
