#!/usr/bin/env python3
# Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
# This product includes software developed at Datadog (https://www.datadoghq.com/).
# Copyright 2026-present Datadog, Inc.

"""Generate an offline research report from recorded outcomes. No network calls."""
import argparse, collections, csv, html, json, math, pathlib, statistics
import numpy as np
import matplotlib
matplotlib.use('Agg')
import matplotlib.pyplot as plt

POLICIES=['fixed2','fixed6','reactive','hpa','ewma','persistence','toto_threshold','jev','jev_toto']
LABELS=dict(zip(POLICIES,['Fixed 2','Fixed 6','Reactive','HPA-inspired','EWMA','Persistence','Toto + reactive','Jev','Jev + Toto']))
COLORS=dict(zip(POLICIES,['#94a3b8','#475569','#0284c7','#0369a1','#8b5cf6','#38bdf8','#0d9488','#ea580c','#be123c']))
PATTERNS=['steady','spike','ramp','waves','bursts','mix']

def avg(xs): return statistics.mean(xs) if xs else float('nan')
def pct(xs,p): return float(np.quantile(xs,p)) if xs else None

def bootstrap(rows,field,other=None):
    """Paired cluster bootstrap: an entire seed (all cells/policies) is one block."""
    blocks=collections.defaultdict(list)
    lookup={(r['pattern'],r['load'],r['seed'],r['policy']):r for r in all_rows}
    for r in rows:
        value=r[field]
        if other:
            pair=lookup.get((r['pattern'],r['load'],r['seed'],other))
            if pair is None: continue
            value-=pair[field]
        blocks[r['seed']].append(value)
    means=np.array([avg(v) for v in blocks.values()]); rng=np.random.default_rng(917)
    if not len(means):return [None,None,None]
    draws=means[rng.integers(0,len(means),size=(10000,len(means)))].mean(axis=1)
    return [float(means.mean()),*map(float,np.quantile(draws,[.025,.975]))]

def money(v):return f'${v:.5f}'
def table(headers,rows,bold_cells=()):
    def cell(value,row,column):
        text=html.escape(str(value))
        return '<td>'+('<strong>'+text+'</strong>' if (row,column) in bold_cells else text)+'</td>'
    return '<table><thead><tr>'+''.join('<th>'+html.escape(str(x))+'</th>' for x in headers)+'</tr></thead><tbody>'+''.join('<tr>'+''.join(cell(x,i,j) for j,x in enumerate(r))+'</tr>' for i,r in enumerate(rows))+'</tbody></table>'

def forecast_accuracy(root):
    groups=collections.defaultdict(lambda:dict(abs=[],signed=[],coverage=[],width=[],latency=[],persistence_abs=[],median_persistence_abs=[]))
    total=failures=0
    for path in sorted(root.glob('*/forecasts.json')):
        records=json.loads(path.read_text()); trace={b['at_ms']//1000:b['values'] for b in json.loads((path.parent/'workload.json').read_text())};pattern,load,_=path.parent.name.split('-')
        for rec in records:
            total+=1
            if not rec['snapshot']:failures+=1;continue
            f=rec['snapshot'];origin=f['origin_ms']//1000;persistence=np.mean(rec['input']['values'][-10:],axis=0);median_persistence=np.median(rec['input']['values'][-10:],axis=0)
            for k,name in enumerate(['jobs','cpu','memory']):
                series=f['series'][k]
                for j,median in enumerate(series['median']):
                    at=origin+j+1
                    if at not in trace or at<=300:continue
                    horizon='1–30s' if j<30 else '31–60s' if j<60 else '61–120s'
                    g=groups[(pattern,load,name,horizon)];actual=trace[at][k];lo=series['lower'][j];hi=series['upper'][j]
                    g['abs'].append(abs(median-actual));g['persistence_abs'].append(abs(persistence[k]-actual));g['median_persistence_abs'].append(abs(median_persistence[k]-actual));g['signed'].append(median-actual);g['coverage'].append(lo<=actual<=hi);g['width'].append(hi-lo)
            groups[('all','all','rpc','all')]['latency'].append(rec['latency_ms'])
    result=[]
    for key,g in groups.items():
        if key[2]=='rpc':continue
        result.append(dict(pattern=key[0],load=key[1],series=key[2],horizon=key[3],n=len(g['abs']),mae=avg(g['abs']),persistence_mae=avg(g['persistence_abs']),median_persistence_mae=avg(g['median_persistence_abs']),bias=avg(g['signed']),coverage=avg(g['coverage']),interval_width=avg(g['width'])))
    latency=groups[('all','all','rpc','all')]['latency']
    return dict(calls=total,failures=failures,latency_p50_ms=pct(latency,.5),latency_p95_ms=pct(latency,.95),cells=result)

def diagnostics(root):
    out={p:dict(actions=collections.Counter(),errors=collections.Counter(),latencies=[],changes=0,measurement_changes=0,probability_records=0,reversals=0,guard_reasons=collections.Counter()) for p in POLICIES}
    for path in root.glob('*/results-equal.json'):
        d=path.parent
        for p in POLICIES:
            journal=d/f'decisions-{p}-equal.jsonl'
            if not journal.exists():continue
            for line in journal.read_text().splitlines():
                rec=json.loads(line);g=out[p];g['actions'][rec['action']]+=1;r=rec['result']
                if r['error']:g['errors'][r['error']]+=1
                if r['latency_ms']>0:g['latencies'].append(r['latency_ms'])
                if r['probabilities']:g['probability_records']+=1
            timeline=json.loads((d/f'timeline-{p}-equal.json').read_text()); last=None
            for t in timeline:
                if 'applied' not in t:continue
                if not t['applied']:out[p]['guard_reasons'][t['reason'].split(':')[0]]+=1
                if t['applied'] and t['action']!='hold':
                    g['changes']+=1
                    if t['at']>300:g['measurement_changes']+=1
                    direction='down' if t['action']=='drain_one' else 'up'
                    if last and last[1]!=direction and t['at']-last[0]<=120:g['reversals']+=1
                    last=(t['at'],direction)
    for p,g in out.items():
        latencies=g.pop('latencies');g['latency_p50_ms']=pct(latencies,.5);g['latency_p95_ms']=pct(latencies,.95)
    return out

if __name__=='__main__':
    ap=argparse.ArgumentParser();ap.add_argument('root',type=pathlib.Path);ap.add_argument('--expected-traces',type=int,default=120);args=ap.parse_args();root=args.root.resolve(); report=root/'report';report.mkdir(exist_ok=True)
    candidate_paths=sorted(root.glob('*/results-equal.json'))
    valid_paths=[]
    for candidate in candidate_paths:
        billing_error=False
        for journal in candidate.parent.glob('decisions-jev*-equal.jsonl'):
            for line in journal.read_text().splitlines():
                try:error=json.loads(line)['result']['error'] or ''
                except json.JSONDecodeError:continue
                if any(code in error for code in ['typesafe_http_402','typesafe_http_401','typesafe_http_403']):billing_error=True;break
            if billing_error:break
        if not billing_error:valid_paths.append(candidate)
    cells_by_seed=collections.defaultdict(set)
    for candidate in valid_paths:
        pattern,load,seed=candidate.parent.name.split('-');cells_by_seed[seed].add((pattern,load))
    expected_cells={(p,l) for p in PATTERNS for l in ['moderate','heavy']}
    complete_seeds={seed for seed,cells in cells_by_seed.items() if cells==expected_cells}
    paths=[p for p in valid_paths if p.parent.name.rsplit('-',1)[1] in complete_seeds]
    all_rows=[r for p in paths for r in json.loads(p.read_text())]
    if not all_rows:raise SystemExit('No complete balanced seed blocks without billing failures')
    complete=len(paths)==args.expected_traces
    blocked=(root/'STOP.json').exists()
    with (report/'runs.csv').open('w',newline='') as out:
        rows=[{**{k:v for k,v in r.items() if not isinstance(v,dict)},**{'jev_'+k:v for k,v in r['jev_cost'].items()}} for r in all_rows];writer=csv.DictWriter(out,fieldnames=list(rows[0]));writer.writeheader();writer.writerows(rows)
    summaries=[]
    for p in POLICIES:
        rows=[r for r in all_rows if r['policy']==p]
        if not rows:continue
        total=sum(r['jev_cost']['estimated_usd'] for r in rows);offered=sum(r['offered'] for r in rows)
        summaries.append(dict(policy=p,n=len(rows),slo=bootstrap(rows,'slo_rate'),node_seconds=bootstrap(rows,'node_seconds'),mean_wait_p95=avg([r['wait_p95_s'] for r in rows if r['wait_p95_s'] is not None]),reject_rate=avg([r['rejected']/r['offered'] for r in rows]),changes=avg([r['capacity_changes'] for r in rows]),cpu_utilization=avg([r['cpu_reservation_utilization'] for r in rows]),memory_utilization=avg([r['memory_reservation_utilization'] for r in rows]),calls=sum(r['jev_cost']['calls'] for r in rows),input_tokens=sum(r['jev_cost']['input_tokens'] for r in rows),output_tokens=sum(r['jev_cost']['output_tokens'] for r in rows),cost=total,cost_per_1000_offered=total/offered*1000,missing_usage=sum(r['jev_cost']['missing_usage_calls'] for r in rows),unpriced=sum(r['jev_cost']['unpriced_calls'] for r in rows),guards=sum(r['guard_rejections'] for r in rows),fallbacks=sum(r['fallbacks'] for r in rows),known_total=sum(r['known_combined_cost_usd'] for r in rows),unfinished=sum(r['unfinished'] for r in rows)))
    comparisons=[]
    for policy,baseline in [('jev','reactive'),('jev','hpa'),('jev_toto','jev'),('toto_threshold','reactive'),('jev_toto','toto_threshold')]:
        rows=[r for r in all_rows if r['policy']==policy];comparisons.append(dict(policy=policy,baseline=baseline,slo_difference=bootstrap(rows,'slo_rate',baseline),node_seconds_difference=bootstrap(rows,'node_seconds',baseline)))
    fstats=forecast_accuracy(root);diag=diagnostics(root)
    summary=dict(completed_traces=len(paths),expected_traces=args.expected_traces,complete=complete,blocked_by_payment=blocked,clean_complete_traces=len(valid_paths),balanced_seeds=sorted(complete_seeds),excluded_or_unbalanced_traces=len(candidate_paths)-len(paths),policies=summaries,comparisons=comparisons,forecasts=fstats,diagnostics=diag)
    (report/'summary.json').write_text(json.dumps(summary,indent=2))
    plt.rcParams.update({'font.family':'DejaVu Sans','font.size':10,'axes.spines.top':False,'axes.spines.right':False,'figure.dpi':150})
    fig,(full,zoom)=plt.subplots(1,2,figsize=(14,5.5),gridspec_kw={'width_ratios':[1,1.4]})
    markers=dict(zip(POLICIES,['s','s','o','D','^','x','P','o','*']))
    for s in summaries:
        x,xlo,xhi=s['node_seconds']; y,ylo,yhi=[v*100 for v in s['slo']];p=s['policy']
        for ax in [full,zoom]:
            if ax is zoom and p=='fixed2':continue
            ax.errorbar(x/3600,y,xerr=np.array([[max(0,x-xlo)],[max(0,xhi-x)]])/3600,yerr=np.array([[max(0,y-ylo)],[max(0,yhi-y)]]),fmt=markers[p],color=COLORS[p],capsize=3,label=LABELS[p],markersize=7)
    full.set_ylim(0,100);full.set_title('Full scale, including fixed-two reference');zoom.set_title('Expanded view of the other policies')
    for ax in [full,zoom]:
        ax.set(xlabel='Mean active node-hours per trace',ylabel='Offered jobs meeting completion SLO (%)');ax.grid(alpha=.15)
    full.legend(loc='upper left',ncol=2,fontsize=7)
    fig.suptitle('Capacity consumption versus service quality · 95% seed-cluster intervals')
    fig.tight_layout();fig.savefig(report/'tradeoff.png');plt.close(fig)
    fig,axes=plt.subplots(2,3,figsize=(14,7),sharey=True)
    for pattern,ax in zip(PATTERNS,axes.flat):
        for p in ['fixed6','reactive','hpa','ewma','toto_threshold','jev','jev_toto']:
            rows=[r for r in all_rows if r['pattern']==pattern and r['policy']==p];v=bootstrap(rows,'slo_rate')
            if v[0] is None:continue
            ax.scatter(avg([r['node_seconds'] for r in rows])/3600,v[0]*100,color=COLORS[p],label=LABELS[p]);ax.set_title(pattern.capitalize());ax.grid(alpha=.15)
        ax.set_xlabel('Node-hours');ax.set_ylabel('SLO success (%)')
    axes.flat[0].legend(fontsize=7);fig.suptitle('Workload-specific tradeoffs · both load levels');fig.tight_layout();fig.savefig(report/'workloads.png');plt.close(fig)
    # Prespecified example, not selected for an outcome: first seed, moderate spike.
    example=sorted([p.parent for p in paths if p.parent.name.startswith('spike-moderate-')])
    if example:
        d=example[0];trace=json.loads((d/'workload.json').read_text());fig,axes=plt.subplots(4,1,figsize=(12,10),sharex=True)
        xs=[b['at_ms']/1000-300 for b in trace if b['at_ms']>300000];cpu=[b['values'][1] for b in trace if b['at_ms']>300000];smooth=np.convolve(cpu,np.ones(30)/30,mode='same');axes[0].plot(xs,smooth,color='#334155',label='Offered work (30s mean)');memory=[b['values'][2] for b in trace if b['at_ms']>300000];axes[1].plot(xs,np.convolve(memory,np.ones(30)/30,mode='same'),color='#334155')
        for p in ['reactive','hpa','toto_threshold','jev','jev_toto']:
            data=json.loads((d/f'timeline-{p}-equal.json').read_text());data=[t for t in data if 'ready' in t and t['at']>=300];x=[t['at']-300 for t in data];axes[0].step(x,[t['ready']*8 for t in data],where='post',color=COLORS[p],alpha=.7,label=LABELS[p]);axes[1].step(x,[t['ready']*16 for t in data],where='post',color=COLORS[p],alpha=.7);axes[2].plot(x,[t['queued'] for t in data],color=COLORS[p]);axes[3].step(x,[t['starting'] for t in data],where='post',color=COLORS[p])
        for ax in axes:ax.grid(alpha=.15)
        axes[0].set_ylabel('CPU work / ready CPU');axes[0].legend(ncol=3,fontsize=8);axes[1].set_ylabel('Memory work / ready GiB');axes[2].set_ylabel('Queued jobs');axes[3].set_ylabel('Starting nodes');axes[3].set_xlabel('Seconds after warm-up');fig.suptitle('Prespecified example: moderate spike, first evaluation seed');fig.tight_layout();fig.savefig(report/'timeline.png');plt.close(fig)
    fig,axes=plt.subplots(1,2,figsize=(12,4.5),sharey=True)
    labels=[LABELS[c['policy']]+' − '+LABELS[c['baseline']] for c in comparisons]
    for i,c in enumerate(comparisons):
        for ax,key,scale in [(axes[0],'slo_difference',100),(axes[1],'node_seconds_difference',1/60)]:
            mean,low,high=[v*scale for v in c[key]];ax.errorbar(mean,i,xerr=[[max(0,mean-low)],[max(0,high-mean)]],fmt='o',color=COLORS[c['policy']],capsize=4)
    for ax in axes:ax.axvline(0,color='#64748b',linestyle='--',linewidth=1);ax.grid(axis='x',alpha=.15)
    axes[0].set_yticks(range(len(labels)),labels);axes[0].invert_yaxis();axes[0].set_xlabel('SLO change (percentage points; right is better)');axes[1].set_xlabel('Node-minutes change (left is cheaper)');fig.suptitle('Paired ablations · 95% seed-cluster bootstrap intervals');fig.tight_layout();fig.savefig(report/'ablations.png');plt.close(fig)
    rows=[]
    for s in summaries:
        rows.append([LABELS[s['policy']],f'{s["slo"][0]*100:.2f}%',f'{s["slo"][1]*100:.2f}–{s["slo"][2]*100:.2f}%',f'{s["node_seconds"][0]/3600:.3f}',f'{s["mean_wait_p95"]:.1f}s',f'{s["reject_rate"]*100:.2f}%',s['calls'],money(s['cost'])])
    main_table=table(['Policy','SLO success','95% interval','Mean node-hours','Mean run p95 wait','Rejected','Jev calls','Jev cost (all runs)'],rows)
    pair_rows=[]
    for c in comparisons:
        a,lo,hi=[v*100 for v in c['slo_difference']];n,nlo,nhi=c['node_seconds_difference'];pair_rows.append([LABELS[c['policy']]+' − '+LABELS[c['baseline']],f'{a:+.2f} pp [{lo:+.2f}, {hi:+.2f}]',f'{n/3600:+.3f} h [{nlo/3600:+.3f}, {nhi/3600:+.3f}]'])
    pair_table=table(['Paired comparison','SLO change, 95% interval','Node-hours change, 95% interval'],pair_rows)
    cell_rows=[];best_cells=set()
    for pattern in PATTERNS:
        for load in ['moderate','heavy']:
            vals=[];means=[]
            for p in POLICIES:
                v=[r['slo_rate']*100 for r in all_rows if r['pattern']==pattern and r['load']==load and r['policy']==p];vals.append(f'{avg(v):.2f}%' if v else 'pending');means.append(avg(v) if v else float('-inf'))
            best=max(means)
            best_cells.update((len(cell_rows),i+2) for i,value in enumerate(means) if math.isfinite(value) and math.isclose(value,best,rel_tol=0,abs_tol=1e-12))
            cell_rows.append([pattern,load,*vals])
    cell_table=table(['Workload','Load',*[LABELS[p] for p in POLICIES]],cell_rows,bold_cells=best_cells)
    cost_table=table(['Policy','Input tokens','Output tokens','Cost / 1,000 offered jobs','Cost / applied capacity change','Missing usage','Unpriced model calls','Guard rejects','Fallback / missing forecast'],[[LABELS[s['policy']],f'{s["input_tokens"]:,}',f'{s["output_tokens"]:,}',money(s['cost_per_1000_offered']),money(s['cost']/max(1,diag[s['policy']]['changes'])),s['missing_usage'],s['unpriced'],s['guards'],s['fallbacks']] for s in summaries if s['calls']])
    utilization_table=table(['Policy','CPU reservation utilization','Memory reservation utilization','Mean capacity changes','Rapid direction reversals (all runs)'],[[LABELS[s['policy']],f"{s['cpu_utilization']*100:.1f}%",f"{s['memory_utilization']*100:.1f}%",f"{s['changes']:.1f}",diag[s['policy']]['reversals']] for s in summaries])
    f_table=table(['Series','Horizon','Toto median MAE','10s-mean persistence MAE','10s-median persistence MAE','Empirical interval coverage','Mean interval width'],[[series,horizon,f'{avg([v["mae"] for v in fstats["cells"] if v["series"]==series and v["horizon"]==horizon]):.3f}',f'{avg([v["persistence_mae"] for v in fstats["cells"] if v["series"]==series and v["horizon"]==horizon]):.3f}',f'{avg([v["median_persistence_mae"] for v in fstats["cells"] if v["series"]==series and v["horizon"]==horizon]):.3f}',f'{avg([v["coverage"] for v in fstats["cells"] if v["series"]==series and v["horizon"]==horizon])*100:.1f}%',f'{avg([v["interval_width"] for v in fstats["cells"] if v["series"]==series and v["horizon"]==horizon]):.3f}'] for series in ['jobs','cpu','memory'] for horizon in ['1–30s','31–60s','61–120s']])
    title='Capacity management: Jev × Toto study'; total_cost=sum(s['cost'] for s in summaries)
    by_policy={s['policy']:s for s in summaries}
    jev_s=by_policy['jev'];jt=by_policy['jev_toto'];reactive_s=by_policy['reactive'];hpa_s=by_policy['hpa'];fixed_s=by_policy['fixed6']
    ablation=next(c for c in comparisons if c['policy']=='jev_toto' and c['baseline']=='jev')
    delta,low,high=[v*100 for v in ablation['slo_difference']]
    takeaways=f'''<h2>Findings</h2><ul>
<li>Jev improved mean SLO success by {(jev_s['slo'][0]-reactive_s['slo'][0])*100:.2f} percentage points over reactive control, using {(jev_s['node_seconds'][0]/reactive_s['node_seconds'][0]-1)*100:.2f}% more capacity. Relative to HPA-inspired control, the differences were {(jev_s['slo'][0]-hpa_s['slo'][0])*100:+.2f} points and {(jev_s['node_seconds'][0]/hpa_s['node_seconds'][0]-1)*100:+.2f}% capacity. This is a service-quality/resource tradeoff, not evidence of greater efficiency at a matched resource budget.</li>
<li>Adding Toto to Jev changed SLO success by {delta:+.2f} points (95% interval {low:+.2f} to {high:+.2f}), changed capacity consumption by {(jt['node_seconds'][0]/jev_s['node_seconds'][0]-1)*100:+.2f}%, and increased Jev inference cost to {jt['cost']/jev_s['cost']:.2f} times the no-forecast arm. Toto's own cost is additional and unknown. This forecast representation did not improve Jev's completion SLO.</li>
<li>Fixed-six achieved {fixed_s['slo'][0]*100:.2f}% SLO success versus Jev's {jev_s['slo'][0]*100:.2f}%. Toto+reactive achieved {by_policy['toto_threshold']['slo'][0]*100:.2f}% while consuming nearly the same capacity as fixed-six. Its improvement over reactive control should therefore not be attributed to useful anticipation without this capacity reference.</li>
<li>These findings concern this synthetic capacity-control setup, the fixed prompt, and this forecast pipeline. The ablations are exploratory and do not establish production performance or isolate model-response randomness.</li></ul>'''
    body=f'''<h1>{title}</h1><p class="lede">{'Completed initial study' if complete else ('PRELIMINARY · remaining study blocked by payment' if blocked else 'INCOMPLETE interim report')} · {len(paths)} / {args.expected_traces} paired traces · {len(all_rows)} policy runs</p>
<p>Six workload patterns, moderate/heavy load, {len(complete_seeds)} complete held-out seeds per cell (10 planned). Each trace includes five minutes of warm-up and 30 minutes of measured arrivals. Primary endpoint: completion within estimated job duration + 20 seconds, counting every offered job. All policies use the same Reflex executor and FIFO placement.</p>
<p>Policy results use only complete, balanced seed blocks without billing/authentication failures. {len(candidate_paths)-len(paths)} completed traces are excluded from these aggregates because they are affected or belong to incomplete seed blocks. Forecast diagnostics cover all collected forecast traces. The cost supplement includes paid work from excluded and partial runs.</p><div class="cards"><div><b>{len(all_rows):,}</b> policy runs</div><div><b>{sum(s['calls'] for s in summaries):,}</b> Jev calls</div><div><b>{money(total_cost)}</b> known Jev cost</div><div><b>{fstats['calls']:,}</b> Toto calls · price unknown</div></div>
{takeaways}<h2>Service quality and capacity consumption</h2><img src="tradeoff.png" alt="Capacity cost versus SLO success">{main_table}
<p>Intervals resample entire seed blocks across workload cells, preserving paired dependence. Means weight each workload trace equally. Mean run p95 is an average of per-run percentiles, not a pooled p95. Fixed 6 is a capacity reference, not an optimal oracle.</p>
<h2>Paired ablations</h2><img src="ablations.png" alt="Paired ablation differences with confidence intervals">{pair_table}<p>Positive SLO difference is better; negative node-hours difference is cheaper. Intervals describe workload-seed uncertainty conditional on these model responses; they do not isolate model stochasticity. These are exploratory comparisons, without multiplicity correction.</p>
<h2>Workload breakdown</h2><img src="workloads.png" alt="Per-workload capacity comparisons"><div class="scroll">{cell_table}</div>
<h2>Resource utilization and stability</h2>{utilization_table}<p>Reservation utilization divides running jobs' reservations by active capacity, including starting/draining nodes. A rapid reversal means an opposite-direction applied capacity change within 120 seconds; reversal counts include warm-up. Lower utilization can be the cost of avoiding queueing and fragmentation.</p><h2>Inference cost and execution diagnostics</h2>{cost_table}<p>Prices: <a href="https://docs.typesafe.ai/models">Jev 1.13.0</a>, $0.042 per million input tokens, output free, checked September 20, 2026. Costs include warm-up calls. Missing-usage calls are excluded from the known estimate. No retries. Toto monetary cost is unknown. Node cost in the downloadable CSV uses an illustrative $0.10/node-hour, not a cloud quote. A fully inclusive Jev+Toto dollar comparison is therefore unavailable.</p>
<h2>Forecast diagnostics</h2><p>{fstats['calls']:,} forecast requests; {fstats['failures']} failures. RPC p50 {fstats['latency_p50_ms']:.1f} ms, p95 {fstats['latency_p95_ms']:.1f} ms.</p>{f_table}<p>Raw forecast values are scored here; the decision evidence clamps negative demand to zero. Pointwise p10–p90 coverage is empirical. Error units are jobs/s, CPU-seconds/s and GiB-seconds/s. Forecast outcomes overlap and are correlated; these rows are descriptive. The decision-quality experiment assigns all decisions and forecasts a one-second delay.</p>
<h2>Example trajectory</h2><img src="timeline.png" alt="Moderate spike timeline">
<h2>Additional diagnostics and full cost audit</h2><p>The <a href="SUPPLEMENT.html">supplement</a> reports measured-latency sensitivity, the known-future diagnostic, and separate costs for preflight and sensitivity calls. <a href="supplement.json">Download its JSON.</a></p><h2>Limits and reproducibility</h2><ul><li>CPU and memory requests are hard reservations. Packing and FIFO head-of-line blocking can limit service even when aggregate CPU appears underused.</li><li>Synthetic capacity control, not production scheduling. No preemption, migration, network effects, or CPU-sharing slowdown. Strict FIFO can create head-of-line blocking.</li><li>{len(complete_seeds)} completed seeds per workload cell and one Jev execution per arm per trace. Pattern timing is fixed. No claim of optimal tuning, production generality, or a universal winner.</li><li>Jev uses one fixed prompt; no prompt search was performed. Baseline parameters selected on separate seeds 11–13; held-out seeds begin at 1001. Forecast-enabled reactive uses the same parameters as its no-forecast ablation.</li><li>Toto's 256-second history is shorter than the wave workload's 300-second period. One-second offered-work series are sparse; the mean of pointwise medians is not a forecast of aggregate mean demand. This study does not optimize the forecasting representation.</li><li>Both Jev arms receive the same 256-second history, compressed to 16-second means. Toto sees raw one-second history, so the ablation evaluates the full forecast feature pipeline.</li><li>Incomplete jobs and rejections remain in the SLO denominator. Latency statistics condition on completed jobs.</li><li>Unknown Toto price and missing API usage prevent a complete monetary bill. All numbers are simulation observations and usage-based estimates, not invoiced charges.</li></ul>
<p><a href="runs.csv">Per-run CSV</a> · <a href="summary.json">Summary and confidence intervals</a> · <a href="PROTOCOL.md">Frozen protocol</a></p>'''
    (report/'explorer.html').write_text('<!doctype html><html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>'+title+'</title><style>body{font-family:system-ui,sans-serif;background:#f7f9fc;color:#18283b;max-width:1240px;margin:40px auto;padding:0 24px;line-height:1.6}h1{font-size:36px;letter-spacing:-1px}h2{margin-top:44px}p{max-width:1050px}.lede{font-size:19px;color:#52657d}.cards{display:flex;flex-wrap:wrap;gap:16px;margin:24px 0}.cards div{flex:1 1 200px;background:#fff;border:1px solid #dce4ee;border-radius:10px;padding:20px}.cards b{display:block;font-size:28px}img{max-width:100%;background:white;border-radius:8px}table{display:block;overflow-x:auto;border-collapse:collapse;width:100%;background:white;font-size:13px;margin:18px 0}th,td{text-align:right;padding:10px;border-bottom:1px solid #e3e9f1}th{background:#eaf0f7}th:first-child,td:first-child{text-align:left}.scroll{overflow:auto}a{color:#0369a1}</style><body>'+body+'</body></html>')
    md=[f'# {title}',f'{"PRELIMINARY — study blocked by payment. " if blocked else ""}{len(paths)} paired workload traces; {len(all_rows)} policy runs. Known Jev inference cost: {money(total_cost)}. Toto cost unknown.','| Policy | SLO success | Mean node-hours | Jev cost |','|---|---:|---:|---:|']
    md += [f'| {LABELS[s["policy"]]} | {s["slo"][0]*100:.2f}% | {s["node_seconds"][0]/3600:.3f} | {money(s["cost"])} |' for s in summaries]
    md+=['','Paired differences (95% seed-cluster bootstrap intervals):']+[f'- {r[0]}: {r[1]}; resources {r[2]}.' for r in pair_rows]
    md+=['','These are synthetic capacity-control results, with exact simulator observations and equal one-second decision delay. SLO counts every offered job. Costs include warm-up inference. Toto price is unknown. See the HTML report and frozen protocol for full limitations.']
    (report/'RESULTS.md').write_text('\n\n'.join(md[:2])+'\n\n'+'\n'.join(md[2:])+'\n')
    (report/'PROTOCOL.md').write_text((pathlib.Path(__file__).resolve().parent/'PROTOCOL.md').read_text())
    print(json.dumps({'traces':len(paths),'runs':len(all_rows),'cost':total_cost,'report':str(report/'explorer.html')}))
