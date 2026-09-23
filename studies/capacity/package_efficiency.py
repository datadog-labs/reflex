#!/usr/bin/env python3
# Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
# This product includes software developed at Datadog (https://www.datadoghq.com/).
# Copyright 2026-present Datadog, Inc.

"""Package only the paper and its figures; uploading remains an explicit separate step."""
from pathlib import Path
import re,base64,hashlib
root=Path(__file__).resolve().parents[2]/'output/capacity-study-efficiency/report'
s=(root/'index.html').read_text()
s=re.sub(r'<nav class="toolbar">.*?</nav>', '<nav class="toolbar"><span>EFFICIENCY FOLLOW-UP</span></nav>', s, flags=re.S)
def embed(m):
 p=root/m.group(1);assert p.suffix=='.png'
 return 'src="data:image/png;base64,'+base64.b64encode(p.read_bytes()).decode()+'"'
s=re.sub(r'src="([^"\s]+\.png)"',embed,s)
s=re.sub(r'<a href="(?!https?://|#)([^"]+)">(.*?)</a>',lambda m:m.group(2),s,flags=re.S)
s=s.replace('The local report package includes per-trace data, summary statistics, validation, and the follow-up protocol.','Supporting per-trace data, summary statistics, validation records, and the follow-up protocol are retained separately in the local report package.')
assert s.count('src="data:image/png;base64,')==5
assert s.count('data-chart="')==5 and s.count('data-workload-select>')==2
assert not any(x in s for x in ['data:application/','data:text/','/Users/','127.0.0.1','localhost','TYPESAFE_API_KEY','DD_API_KEY'])
assert all(x.startswith(('https://','#','data:image/')) for x in re.findall(r'(?:href|src)="([^"]+)"',s))
p=Path('/tmp/reflex-efficiency-interactive-paper.html');p.write_text(s)
print({'file':str(p),'bytes':p.stat().st_size,'interactive_charts':5,'workload_selectors':2,'sha256':hashlib.sha256(p.read_bytes()).hexdigest()})
