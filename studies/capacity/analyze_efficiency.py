#!/usr/bin/env python3
"""Delay-first analysis of original policies and the efficiency-prompt follow-up."""
import argparse,collections,csv,hashlib,json,math,pathlib,statistics
import numpy as np
import matplotlib
matplotlib.use('Agg')
import matplotlib.pyplot as plt
ROOT=pathlib.Path(__file__).resolve().parents[2]
BASE=ROOT/'output/capacity-study';NEW=ROOT/'output/capacity-study-efficiency';REPORT=NEW/'report'
PATTERNS=['steady','spike','ramp','waves','bursts','mix']
LABELS={'fixed2':'Fixed 2','fixed6':'Fixed 6','reactive':'Reactive','hpa':'HPA-inspired','ewma':'EWMA','persistence':'Persistence','toto_threshold':'Toto + reactive','jev':'Jev · original','jev_toto':'Jev + Toto · original','jev_efficiency':'Jev · efficiency','jev_toto_efficiency':'Jev + Toto · efficiency'}
COLORS={'fixed2':'#94a3b8','fixed6':'#475569','reactive':'#0891b2','hpa':'#64748b','ewma':'#8b5cf6','persistence':'#38bdf8','toto_threshold':'#0d9488','jev':'#e87924','jev_toto':'#d85a7d','jev_efficiency':'#126b8a','jev_toto_efficiency':'#693ca3'}

def distribution(values):
 if not values:return dict(mean=None,p25=None,p50=None,p99=None)
 return dict(mean=float(np.mean(values)),**{f'p{q}':float(np.quantile(values,q/100,method='higher')) for q in [25,50,99]})

def collect(root,efficiency=False):
 rows=[]; pooled=collections.defaultdict(lambda:dict(wait=[],completion=[]))
 paths=sorted(root.glob('*/results-equal.json'))
 for path in paths:
  results=json.loads(path.read_text())
  for result in results:
   original_policy=result['policy'];policy=original_policy+('_efficiency' if efficiency else '')
   jobs=[j for j in json.loads((path.parent/f'jobs-{original_policy}-equal.json').read_text()) if 300000<j['arrived_at']<=2100000]
   assert len(jobs)==result['offered']
   completed=[j for j in jobs if j['phase']=='completed']
   wait=[(j['started_at']-j['arrived_at'])/1000 for j in completed]
   completion=[w+j['arrival']['actual_s'] for w,j in zip(wait,completed)]
   assert distribution(wait)['p50']==result['wait_p50_s'] and distribution(wait)['p99']==result['wait_p99_s']
   pooled[policy]['wait'].extend(wait);pooled[policy]['completion'].extend(completion)
   rejected=sum(j['phase']=='rejected' for j in jobs);unfinished=len(jobs)-len(completed)-rejected
   assert (len(completed),rejected,unfinished)==(result['completed'],result['rejected'],result['unfinished'])
   timeline=json.loads((path.parent/f'timeline-{original_policy}-equal.json').read_text())
   changes=[x for x in timeline if x.get('applied') and x['action']!='hold' and x['at']>300]
   drains=sum(x['action']=='drain_one' for x in changes)
   starts=sum(x['action'] in ['start_one','start_two'] for x in changes)
   reversals=0;last=None
   for x in changes:
    direction='down' if x['action']=='drain_one' else 'up'
    if last and direction!=last[1] and x['at']-last[0]<=120:reversals+=1
    last=(x['at'],direction)
   errors=collections.Counter()
   if original_policy.startswith('jev'):
    for line in (path.parent/f'decisions-{original_policy}-equal.jsonl').read_text().splitlines():
     error=json.loads(line)['result']['error']
     if error:errors[error]+=1
     assert not any(code in (error or '') for code in ['typesafe_http_401','typesafe_http_402','typesafe_http_403'])
   rows.append(dict(pattern=result['pattern'],load=result['load'],seed=result['seed'],policy=policy,offered=len(jobs),completed=len(completed),rejected=rejected,unfinished=unfinished,reject_rate=rejected/len(jobs),node_hours=result['node_seconds']/3600,node_seconds=result['node_seconds'],used_cpu_seconds=result['used_cpu_seconds'],used_memory_gib_seconds=result['used_memory_gib_seconds'],drains=drains,starts=starts,reversals=reversals,guard_rejections=result['guard_rejections'],errors=dict(errors),jev_cost=result['jev_cost'],**{'wait_'+k:v for k,v in distribution(wait).items()},**{'completion_'+k:v for k,v in distribution(completion).items()}))
 return rows,pooled

def paired(rows,policy,baseline,field):
 lookup={(r['pattern'],r['load'],r['seed'],r['policy']):r for r in rows};blocks=collections.defaultdict(list)
 for r in rows:
  if r['policy']!=policy:continue
  b=lookup[(r['pattern'],r['load'],r['seed'],baseline)]
  if r[field] is None or b[field] is None:continue
  blocks[r['seed']].append(r[field]-b[field])
 means=np.array([np.mean(v) for _,v in sorted(blocks.items())]);rng=np.random.default_rng(917)
 draws=means[rng.integers(0,len(means),size=(10000,len(means)))].mean(axis=1)
 return [float(means.mean()),*map(float,np.quantile(draws,[.025,.975]))]

def summarize(rows,pooled):
 summaries=[]
 for policy in LABELS:
  data=[r for r in rows if r['policy']==policy]
  if not data:continue
  offered=sum(r['offered'] for r in data);completed=sum(r['completed'] for r in data);node_seconds=sum(r['node_seconds'] for r in data)
  errors=collections.Counter()
  for r in data:errors.update(r['errors'])
  summaries.append(dict(policy=policy,traces=len(data),offered=offered,completed=completed,rejected=sum(r['rejected'] for r in data),unfinished=sum(r['unfinished'] for r in data),rejection_rate=sum(r['rejected'] for r in data)/offered,wait=distribution(pooled[policy]['wait']),completion=distribution(pooled[policy]['completion']),macro_mean_wait=statistics.mean(r['wait_mean'] for r in data),macro_mean_completion=statistics.mean(r['completion_mean'] for r in data),mean_node_hours=statistics.mean(r['node_hours'] for r in data),cpu_utilization=sum(r['used_cpu_seconds'] for r in data)/(8*node_seconds),memory_utilization=sum(r['used_memory_gib_seconds'] for r in data)/(16*node_seconds),drains=sum(r['drains'] for r in data),starts=sum(r['starts'] for r in data),reversals=sum(r['reversals'] for r in data),calls=sum(r['jev_cost']['calls'] for r in data),input_tokens=sum(r['jev_cost']['input_tokens'] for r in data),known_jev_usd=sum(r['jev_cost']['estimated_usd'] for r in data),missing_usage_calls=sum(r['jev_cost']['missing_usage_calls'] for r in data),unpriced_calls=sum(r['jev_cost']['unpriced_calls'] for r in data),errors=dict(errors)))
 return summaries

def plots(rows,summaries):
 plt.rcParams.update({'font.family':'DejaVu Sans','font.size':10,'axes.spines.top':False,'axes.spines.right':False,'svg.fonttype':'none'})
 def save(fig,name):
  overlay_axes=[ax for ax in fig.axes if (ax.get_gid() or '').startswith('overlay--')]
  for ax in overlay_axes:ax.set_visible(ax.get_gid().startswith('overlay--traffic--'))
  fig.savefig(REPORT/(name+'.png'),dpi=180)
  for ax in overlay_axes:ax.set_visible(True)
  for legend in list(fig.legends):legend.remove()
  for ax in fig.axes:
   legend=ax.get_legend()
   if legend:legend.remove()
  fig.savefig(REPORT/(name+'.svg'))
  plt.close(fig)
 fig,(a,b)=plt.subplots(1,2,figsize=(13,5),gridspec_kw={'width_ratios':[1,1.5]})
 for s in summaries:
  p=s['policy']
  for ax in [a,b]:
   if ax is b and p=='fixed2':continue
   ax.scatter(s['mean_node_hours'],s['macro_mean_wait'],color=COLORS[p],label=LABELS[p],s=60,marker='*' if 'efficiency' in p else 'o').set_gid(f'series--{p}--{0 if ax is a else 1}')
 for ax in [a,b]:ax.grid(alpha=.15);ax.set_xlabel('Mean active node-hours per trace');ax.set_ylabel('Mean per-trace queue wait (seconds)')
 a.set_title('Full scale');b.set_title('Expanded view, excluding Fixed 2');a.legend(fontsize=7,loc='best')
 fig.suptitle('Queueing delay versus capacity · lower on both axes is preferable')
 fig.tight_layout();save(fig,'delay-capacity')
 selected=['hpa','fixed6','jev','jev_efficiency','jev_toto','jev_toto_efficiency']
 fig,axes=plt.subplots(1,2,figsize=(13,5))
 for ax,key,title in zip(axes,['wait','completion'],['Queueing delay','Arrival-to-completion time']):
  for i,p in enumerate(selected):
   s=next(s for s in summaries if s['policy']==p);x=np.arange(4)+(i-2.5)*.12
   bars=ax.bar(x,[s[key][k] for k in ['mean','p25','p50','p99']],width=.115,color=COLORS[p],label=LABELS[p])
   for j,bar in enumerate(bars):bar.set_gid(f'series--{p}--{key}-{j}')
  ax.set_xticks(np.arange(4),['Mean','p25','p50','p99']);ax.set_ylabel('Seconds');ax.set_title(title);ax.grid(axis='y',alpha=.15)
 handles,labels=axes[0].get_legend_handles_labels();fig.legend(handles,labels,loc='lower center',ncol=3,fontsize=9)
 fig.suptitle('Completed-job delay distributions · pooled jobs, with rejection reported separately')
 fig.tight_layout(rect=[0,.13,1,.94]);save(fig,'delay-distributions')
 for load in ['moderate','heavy']:
  demand={}
  for pattern in PATTERNS:
   buckets=np.zeros((60,3))
   for seed in range(1001,1011):
    workload=json.loads((BASE/f'{pattern}-{load}-{seed}'/'workload.json').read_text())
    measured=[b for b in workload if 300000<b['at_ms']<=2100000]
    assert len(measured)==1800
    for b in measured:buckets[(b['at_ms']-300001)//30000]+=np.array(b['values'])/300
   demand[pattern]=buckets
  demand_max=np.max(np.stack(list(demand.values())),axis=(0,1))*1.15
  fig,axes=plt.subplots(3,2,figsize=(13,9),sharex=True,sharey=True)
  for pattern,ax in zip(PATTERNS,axes.flat):
   for k,(key,label) in enumerate([('traffic','Requests / s'),('cpu','CPU-seconds / s'),('memory','GiB-seconds / s')]):
    overlay=ax.twinx();overlay.set_gid(f'overlay--{key}--{pattern}')
    values=demand[pattern][:,k]
    overlay.step(np.arange(61)/2,np.r_[values,values[-1]],where='post',color='#657684',lw=1.6,ls=':',alpha=.85)
    overlay.fill_between(np.arange(61)/2,0,np.r_[values,values[-1]],step='post',color='#657684',alpha=.07)
    overlay.set_ylim(0,max(1,demand_max[k]));overlay.set_ylabel(label,color='#657684',fontsize=8)
    overlay.tick_params(axis='y',labelcolor='#657684',labelsize=8)
    overlay.yaxis.set_major_locator(plt.MaxNLocator(4))
    overlay.set_zorder(0)
   ax.set_zorder(1);ax.patch.set_visible(False)
   for p in ['hpa','jev','jev_toto','jev_efficiency','jev_toto_efficiency']:
    root=NEW if p.endswith('_efficiency') else BASE;original=p.removesuffix('_efficiency');samples=collections.defaultdict(list)
    for seed in range(1001,1011):
     timeline=json.loads((root/f'{pattern}-{load}-{seed}'/f'timeline-{original}-equal.json').read_text())
     for t in timeline:
      if 'ready' in t and 300<=t['at']<=2100:samples[t['at']].append(t['ready']+t['starting']+t['draining'])
    ax.step([(x-300)/60 for x in sorted(samples)],[np.mean(samples[x]) for x in sorted(samples)],where='post',color=COLORS[p],label=LABELS[p],lw=1.7,ls='--' if p in ['jev','jev_toto'] else '-')[0].set_gid(f'series--{p}--{pattern}')
   ax.set_title(pattern.capitalize());ax.set_ylim(.8,6.2);ax.set_xlim(0,30);ax.set_ylabel('Active nodes');ax.set_xlabel('Minutes after warm-up');ax.grid(alpha=.15)
  handles,labels=axes[0,0].get_legend_handles_labels();fig.legend(handles,labels,loc='lower center',ncol=3,fontsize=9)
  fig.suptitle(f'Capacity decisions over time · {load} load · mean across ten seeds')
  fig.tight_layout(rect=[0,.08,1,.96]);save(fig,f'capacity-{load}')

def main():
 p=argparse.ArgumentParser();p.add_argument('--baseline-only',action='store_true');args=p.parse_args()
 REPORT.mkdir(exist_ok=True)
 rows,pooled=collect(BASE)
 if args.baseline_only:
  output=REPORT/'original-delay-summary.json';output.write_text(json.dumps(summarize(rows,pooled),indent=2));print(output);return
 assert len(list(NEW.glob('*/results-equal.json')))==120,'Follow-up is incomplete'
 extra,epool=collect(NEW,True);rows.extend(extra);pooled.update(epool)
 summaries=summarize(rows,pooled)
 comparisons=[dict(policy=p,baseline=b,**{k:paired(rows,p,b,k) for k in ['wait_mean','wait_p99','completion_mean','completion_p99','node_hours','reject_rate','drains']}) for p,b in [('jev_efficiency','jev'),('jev_toto_efficiency','jev_toto'),('jev_toto_efficiency','jev_efficiency'),('jev_efficiency','hpa'),('jev_efficiency','reactive')]]
 old_audit=json.loads((BASE/'report/supplement.json').read_text());new_cost=sum(s['known_jev_usd'] for s in summaries if s['policy'].endswith('_efficiency'))
 preflight=sum(r['jev_cost']['estimated_usd'] for p in (ROOT/'output/capacity-study-efficiency-preflight').glob('*/results-equal.json') for r in json.loads(p.read_text()))
 out=dict(traces=120,new_policy_runs=240,total_comparison_runs=len(rows),delay_scope='Completed measured-period jobs; pooled distributions are request-weighted. Paired intervals use equally weighted trace means or per-trace quantiles, clustered by seed.',policies=summaries,comparisons=comparisons,new_known_jev_usd=new_cost,preflight_known_jev_usd=preflight,prior_all_stages_known_jev_usd=old_audit['all_stages_known_usd'],all_stages_known_jev_usd=old_audit['all_stages_known_usd']+new_cost+preflight,new_toto_calls=0)
 (REPORT/'summary.json').write_text(json.dumps(out,indent=2))
 (REPORT/'per-trace.json').write_text(json.dumps(rows,indent=2))
 with (REPORT/'per-trace.csv').open('w') as f:
  flat=[{k:v for k,v in r.items() if not isinstance(v,dict)} for r in rows];w=csv.DictWriter(f,fieldnames=list(flat[0]));w.writeheader();w.writerows(flat)
 plots(rows,summaries)
 print(json.dumps({'new_runs':240,'comparison_runs':len(rows),'new_jev_cost':new_cost}))
if __name__=='__main__':main()
