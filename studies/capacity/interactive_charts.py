"""Self-contained SVG chart controls; only the existing plotted aggregates are embedded."""
import html,re
from pathlib import Path
from analyze_efficiency import LABELS,COLORS
SOURCE=Path(__file__).resolve().parent

def enhance(body,report):
 configs={
  'workload-patterns': [('cpu','CPU work','#126B8A'),('memory','Memory work','#C76A18'),('arrival','Arrival rate','#657684')],
  'delay-distributions':[(p,LABELS[p],COLORS[p]) for p in ['hpa','fixed6','jev','jev_efficiency','jev_toto','jev_toto_efficiency']],
  'capacity-moderate':[(p,LABELS[p],COLORS[p]) for p in ['hpa','jev','jev_toto','jev_efficiency','jev_toto_efficiency']],
  'capacity-heavy':[(p,LABELS[p],COLORS[p]) for p in ['hpa','jev','jev_toto','jev_efficiency','jev_toto_efficiency']],
  'delay-capacity':[(p,LABELS[p],COLORS[p]) for p in LABELS],
 }
 body=re.sub(r'<a href="workload-patterns.png">(<img[^>]+>)</a>',r'\1',body)
 for name,series in configs.items():
  svg=(report/(name+'.svg')).read_text();svg=svg[svg.index('<svg'):]
  # Namespace all SVG references so multiple inline figures cannot share clip paths or markers.
  svg=re.sub(r'id="([^"]+)"',lambda m:'id="'+name+'--'+m[1]+'"',svg)
  svg=re.sub(r'((?:xlink:)?href=")#([^" ]+)',lambda m:m[1]+'#'+name+'--'+m[2],svg)
  svg=re.sub(r'url\(#([^)]+)\)',lambda m:'url(#'+name+'--'+m[1]+')',svg)
  svg=re.sub(r'<g id="'+name+r'--series--([^" ]+)--([^" ]+)">',lambda m:'<g id="'+name+'--series--'+m[1]+'--'+m[2]+'" data-series="'+m[1]+'">',svg)
  svg=re.sub(r'<g id="'+name+r'--overlay--([^" ]+)--([^" ]+)">',lambda m:'<g id="'+name+'--overlay--'+m[1]+'--'+m[2]+'" data-workload="'+m[1]+'">',svg)
  found=set(re.findall(r'data-series="([^"]+)"',svg));assert found=={p for p,_,_ in series},(name,found)
  buttons=''.join(f'<button type="button" data-pick="{p}" aria-pressed="true"><span class="chart-swatch" style="--series-color:{color}"></span>{html.escape(label)}</button>' for p,label,color in series)
  overlay=''
  if name.startswith('capacity-'):
   assert len(re.findall(r'data-workload=',svg))==18
   overlay='<label class="workload-control">Workload overlay <select data-workload-select><option value="none">Off</option><option value="traffic" selected>Traffic — requests / s</option><option value="cpu">CPU demand — CPU-seconds / s</option><option value="memory">Memory demand — GiB-seconds / s</option></select></label><div class="chart-help">Dotted gray line and right axis: actual offered workload, averaged in 30-second bins across the same ten seeds. Resource demand uses requested resources × estimated duration. Left axis: active nodes.</div>'
  scope='Resource-mix panel: ' if name=='workload-patterns' else ''
  widget=f'<div class="interactive-chart" data-chart="{name}" hidden><div class="chart-help">{scope}Click a series or its label to focus. Click others to add or remove them. Show all restores every series.</div>{overlay}<div class="chart-legend" role="group" aria-label="Choose series">{buttons}<button type="button" data-reset>Show all</button></div><div class="chart-status" aria-live="polite">Showing all series</div>{svg}</div>'
  pattern=r'<img src="'+name+r'\.png"([^>]*)>'
  body,count=re.subn(pattern,lambda m:widget+'<img class="chart-fallback" src="'+name+'.png"'+m[1]+'>',body)
  assert count==1,(name,count)
 return body

CSS='''
.interactive-chart[hidden]{display:none}.interactive-chart svg{display:block;width:100%;height:auto}.interactive-chart{font:13px/1.5 system-ui,sans-serif;border:1px solid #dce2e6;border-radius:8px;padding:12px;background:#fff}.chart-help{color:#4a5e6c;margin-bottom:9px}.chart-legend{display:flex;flex-wrap:wrap;gap:6px}.chart-legend button{font:inherit;border:1px solid #b9c9d3;background:#fff;color:#263e50;border-radius:5px;padding:7px 9px;cursor:pointer;display:inline-flex;align-items:center;gap:7px;min-height:34px}.chart-legend button[aria-pressed="false"]{opacity:.4;background:#f4f6f8}.chart-legend button:hover{border-color:#245b88;background:#eef5fa}.chart-legend button:focus-visible{outline:3px solid #245b88;outline-offset:2px}.chart-swatch{width:18px;height:3px;background:var(--series-color);display:inline-block}.chart-status{margin:8px 0;color:#526776;min-height:20px}.interactive-chart [data-series]{cursor:pointer}.interactive-chart [data-series][data-muted],.interactive-chart [data-workload][data-muted]{display:none}.workload-control{display:flex;gap:10px;align-items:center;flex-wrap:wrap;font-weight:600;margin:12px 0 6px}.workload-control select{font:inherit;max-width:100%;padding:6px;border:1px solid #b9c9d3;border-radius:4px;background:white;color:#263e50}.chart-enhanced>.chart-fallback{display:none}@media print{.interactive-chart{display:none!important}.chart-enhanced>.chart-fallback{display:block!important}}
'''
JS=(SOURCE/'interactive_charts.js').read_text()
