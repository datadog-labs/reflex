#!/usr/bin/env python3
# Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
# This product includes software developed at Datadog (https://www.datadoghq.com/).
# Copyright 2026-present Datadog, Inc.

"""Run tuning and paired studies. Credentials remain only in child environments."""
import argparse, concurrent.futures, json, os, pathlib, subprocess, sys, time
ROOT = pathlib.Path(__file__).resolve().parents[2]
BIN = ROOT / 'target/release/examples/capacity_study'

def launch(args, log):
    log.parent.mkdir(parents=True, exist_ok=True)
    with log.open('w') as out:
        p = subprocess.run([str(BIN), *map(str,args)], cwd=ROOT, stdout=out, stderr=subprocess.STDOUT)
    if p.returncode: raise RuntimeError(f'Run failed ({p.returncode}); inspect {log}')

def tune(out):
    configs=[(target,wait,alpha) for target in [0.6,0.8,0.9] for wait in [30,120] for alpha in [0.2,0.6]]
    def one(c):
        target,wait,alpha=c; d=out/f't{target}-w{wait}-a{alpha}'
        launch(['--output',d,'--seeds',3,'--first-seed',11,'--policies','reactive,hpa,ewma','--target',target,'--downscale-wait',wait,'--ewma-alpha',alpha],d/'run.log')
        rows=[r for p in d.glob('*/results-equal.json') for r in json.loads(p.read_text())]
        print(f'TUNED {c}: {len(rows)} policy runs', flush=True)
        return c,rows
    with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool: results=list(pool.map(one, configs))
    chosen={}; grid=[]
    for policy in ['reactive','hpa','ewma']:
        candidates=[]
        for c, rows in results:
            rows=[r for r in rows if r['policy']==policy]
            slo=sum(r['slo_rate'] for r in rows)/len(rows); cost=sum(r['node_seconds'] for r in rows)/len(rows)
            v=dict(target=c[0],downscale_wait=c[1],ewma_alpha=c[2],development_mean_slo=slo,development_mean_node_seconds=cost)
            candidates.append(v); grid.append(dict(policy=policy,**v))
        # Predefined selection: meet 99% aggregate SLO at lowest resource cost;
        # if no configuration meets it, maximize SLO, then minimize resources.
        eligible=[v for v in candidates if v['development_mean_slo']>=.99]
        chosen[policy]=min(eligible,key=lambda v:v['development_mean_node_seconds']) if eligible else min(candidates,key=lambda v:(-v['development_mean_slo'],v['development_mean_node_seconds']))
    (out/'selected.json').write_text(json.dumps(chosen,indent=2));(out/'grid.json').write_text(json.dumps(grid,indent=2))
    print(json.dumps(chosen,indent=2),flush=True)

def study(out,tuning,seeds,first,workers):
    def one(seed):
        launch(['--output',out,'--seeds',1,'--first-seed',seed,'--forecasts','cache',*(['--tuning',tuning] if tuning else [])],out/f'worker-{seed}.log')
        print(f'FINISHED SEED {seed}',flush=True)
    with concurrent.futures.ThreadPoolExecutor(max_workers=workers) as pool:list(pool.map(one,range(first,first+seeds)))

if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('mode',choices=['tune','study']);p.add_argument('--output',type=pathlib.Path,required=True);p.add_argument('--tuning',type=pathlib.Path);p.add_argument('--seeds',type=int,default=10);p.add_argument('--first-seed',type=int,default=1001);p.add_argument('--workers',type=int,default=4);a=p.parse_args()
    a.output=a.output.resolve()
    if a.mode=='tune':tune(a.output)
    else: study(a.output,a.tuning.resolve() if a.tuning else None,a.seeds,a.first_seed,a.workers)
