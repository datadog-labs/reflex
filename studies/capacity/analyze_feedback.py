#!/usr/bin/env python3
# Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
# This product includes software developed at Datadog (https://www.datadoghq.com/).
# Copyright 2026-present Datadog, Inc.

"""Compare the frozen performance-feedback arms with both earlier studies."""
import collections,csv,json,pathlib,statistics
import numpy as np
import analyze_efficiency as a
ROOT=a.ROOT;BASE=a.BASE;EFF=a.NEW;NEW=ROOT/'output/capacity-study-feedback';REPORT=NEW/'report'
LABELS={**a.LABELS,'jev_feedback':'Jev · feedback','jev_toto_feedback':'Jev + Toto · feedback'}
COLORS={**a.COLORS,'jev_feedback':'#c34424','jev_toto_feedback':'#307833'}
SELECTED=['hpa','jev_efficiency','jev_toto_efficiency','jev_feedback','jev_toto_feedback']

def save(fig,name):
 overlays=[ax for ax in fig.axes if (ax.get_gid() or '').startswith('overlay--')]
 for ax in overlays:ax.set_visible(ax.get_gid().startswith('overlay--traffic--'))
 fig.savefig(REPORT/(name+'.png'),dpi=170)
 for ax in overlays:ax.set_visible(True)
 for legend in list(fig.legends):legend.remove()
 for ax in fig.axes:
  if ax.get_legend():ax.get_legend().remove()
 fig.savefig(REPORT/(name+'.svg'));a.plt.close(fig)

def plots(summaries):
 plt=a.plt;lookup={s['policy']:s for s in summaries}
 plt.rcParams.update({'font.family':'DejaVu Sans','font.size':10,'axes.spines.top':False,'axes.spines.right':False,'svg.fonttype':'none'})
 fig,(left,right)=plt.subplots(1,2,figsize=(13,5))
 for ax,keys in [(left,list(LABELS)),(right,[p for p in LABELS if p!='fixed2'])]:
  for p in keys:
   s=lookup[p];ax.scatter(s['mean_node_hours'],s['macro_mean_wait'],color=COLORS[p],label=LABELS[p],s=65,marker='*' if p.endswith('feedback') else 'o').set_gid(f'series--{p}--{0 if ax is left else 1}')
  ax.set_xlabel('Mean active node-hours per trace');ax.set_ylabel('Mean per-trace queue wait (seconds)');ax.grid(alpha=.15)
 left.set_title('All policies');right.set_title('Expanded view without Fixed 2')
 handles,labels=left.get_legend_handles_labels();fig.legend(handles,labels,loc='lower center',ncol=4,fontsize=8)
 fig.suptitle('Delay and capacity: lower on both axes is preferable');fig.tight_layout(rect=[0,.19,1,.95]);save(fig,'delay-capacity')
 selected=['fixed6',*SELECTED]
 fig,axes=plt.subplots(1,2,figsize=(13,5))
 for ax,kind,title in zip(axes,['wait','completion'],['Queue wait','Arrival-to-completion']):
  for i,p in enumerate(selected):
   bars=ax.bar(np.arange(4)+(i-(len(selected)-1)/2)*.12,[lookup[p][kind][k] for k in ['mean','p25','p50','p99']],width=.115,color=COLORS[p],label=LABELS[p])
   for j,bar in enumerate(bars):bar.set_gid(f'series--{p}--{kind}-{j}')
  ax.set_xticks(np.arange(4),['Mean','p25','p50','p99']);ax.set_ylabel('Seconds');ax.set_title(title);ax.grid(axis='y',alpha=.15)
 handles,labels=axes[0].get_legend_handles_labels();fig.legend(handles,labels,loc='lower center',ncol=3,fontsize=8)
 fig.suptitle('Completed-job latency distributions; rejected jobs are reported separately');fig.tight_layout(rect=[0,.13,1,.95]);save(fig,'delay-distributions')
 for load in ['moderate','heavy']:
  demand={}
  for pattern in a.PATTERNS:
   bins=np.zeros((60,3))
   for seed in range(1001,1011):
    workload=json.loads((BASE/f'{pattern}-{load}-{seed}/workload.json').read_text())
    for b in workload:
     if 300000<b['at_ms']<=2100000:bins[(b['at_ms']-300001)//30000]+=np.array(b['values'])/300
   demand[pattern]=bins
  maxima=np.max(np.stack(list(demand.values())),axis=(0,1))*1.15
  fig,axes=plt.subplots(3,2,figsize=(13,9),sharex=True,sharey=True)
  for pattern,ax in zip(a.PATTERNS,axes.flat):
   for k,(key,label) in enumerate([('traffic','Requests / s'),('cpu','CPU-seconds / s'),('memory','GiB-seconds / s')]):
    overlay=ax.twinx();overlay.set_gid(f'overlay--{key}--{pattern}');values=demand[pattern][:,k]
    overlay.step(np.arange(61)/2,np.r_[values,values[-1]],where='post',color='#657684',lw=1.5,ls=':',alpha=.85)
    overlay.fill_between(np.arange(61)/2,0,np.r_[values,values[-1]],step='post',color='#657684',alpha=.07)
    overlay.set_ylim(0,max(1,maxima[k]));overlay.set_ylabel(label,color='#657684',fontsize=8)
    overlay.tick_params(axis='y',labelcolor='#657684',labelsize=8);overlay.yaxis.set_major_locator(plt.MaxNLocator(4));overlay.set_zorder(0)
   ax.set_zorder(1);ax.patch.set_visible(False)
   for p in SELECTED:
    root=NEW if p.endswith('_feedback') else EFF if p.endswith('_efficiency') else BASE
    original=p.removesuffix('_feedback').removesuffix('_efficiency');samples=collections.defaultdict(list)
    for seed in range(1001,1011):
     for t in json.loads((root/f'{pattern}-{load}-{seed}/timeline-{original}-equal.json').read_text()):
      if 'ready' in t and 300<=t['at']<=2100:samples[t['at']].append(t['ready']+t['starting']+t['draining'])
    ax.step([(t-300)/60 for t in sorted(samples)],[np.mean(samples[t]) for t in sorted(samples)],where='post',color=COLORS[p],label=LABELS[p],lw=1.7,ls='--' if p.endswith('efficiency') else '-')[0].set_gid(f'series--{p}--{pattern}')
   ax.set_title(pattern.capitalize());ax.set_ylim(.8,6.2);ax.set_xlim(0,30);ax.set_ylabel('Active nodes');ax.set_xlabel('Minutes after warm-up');ax.grid(alpha=.15)
  handles,labels=axes[0,0].get_legend_handles_labels();fig.legend(handles,labels,loc='lower center',ncol=3,fontsize=9)
  fig.suptitle(f'Capacity and offered workload · {load} load · mean across ten seeds');fig.tight_layout(rect=[0,.08,1,.96]);save(fig,f'capacity-{load}')

def main():
 assert len(list(NEW.glob('*/results-equal.json')))==120,'Feedback run incomplete'
 REPORT.mkdir(exist_ok=True)
 rows,pool=a.collect(BASE);er,ep=a.collect(EFF,True);rows.extend(er);pool.update(ep)
 nr,npool=a.collect(NEW)
 for r in nr:r['policy']+='_feedback'
 rows.extend(nr);pool.update({k+'_feedback':v for k,v in npool.items()})
 a.LABELS=LABELS;summaries=a.summarize(rows,pool)
 comparisons=[dict(policy=p,baseline=b,**{key:a.paired(rows,p,b,key) for key in ['wait_mean','wait_p99','completion_mean','completion_p99','node_hours','reject_rate','drains','reversals']}) for p,b in [('jev_feedback','jev_efficiency'),('jev_toto_feedback','jev_toto_efficiency'),('jev_toto_feedback','jev_feedback'),('jev_feedback','hpa'),('jev_toto_feedback','hpa')]]
 newcost=sum(s['known_jev_usd'] for s in summaries if s['policy'].endswith('_feedback'))
 previous=json.loads((EFF/'report/summary.json').read_text())['all_stages_known_jev_usd']
 out=dict(traces=120,new_policy_runs=240,total_comparison_runs=len(rows),policies=summaries,comparisons=comparisons,new_known_jev_usd=newcost,prior_all_stages_known_jev_usd=previous,all_stages_known_jev_usd=previous+newcost,new_toto_calls=0,delay_scope='Completed measured-period jobs; pooled request-weighted distributions, paired equally weighted trace statistics with seed-block bootstrap.')
 (REPORT/'summary.json').write_text(json.dumps(out,indent=2));(REPORT/'per-trace.json').write_text(json.dumps(rows,indent=2))
 with (REPORT/'per-trace.csv').open('w') as f:
  flat=[{k:v for k,v in r.items() if not isinstance(v,dict)} for r in rows];w=csv.DictWriter(f,fieldnames=list(flat[0]));w.writeheader();w.writerows(flat)
 plots(summaries);print(json.dumps(dict(new_runs=240,comparison_runs=len(rows),new_known_jev_usd=newcost)))
if __name__=='__main__':main()
