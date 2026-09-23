#!/usr/bin/env python3
# Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
# This product includes software developed at Datadog (https://www.datadoghq.com/).
# Copyright 2026-present Datadog, Inc.

"""Replay completed follow-up seeds while paid collection continues independently."""
import concurrent.futures,pathlib,subprocess,time
ROOT=pathlib.Path(__file__).resolve().parents[2];out=ROOT/'output/capacity-study-feedback'
def replay(seed):
 log=out/f'replay-{seed}.log'
 if log.exists() and log.read_text().count('REPLAY VERIFIED')==12:return
 cmd=[str(ROOT/'target/release/examples/capacity_study'),'--output',str(out),'--seeds','1','--first-seed',str(seed),'--forecasts','cache','--policies','jev,jev_toto','--prompt-file',str(ROOT/'studies/capacity/prompts/feedback-v1.txt'),'--tuning',str(ROOT/'output/capacity-study-tuning/selected.json'),'--replay','--performance-feedback']
 with log.open('w') as f:subprocess.run(cmd,cwd=ROOT,stdout=f,stderr=subprocess.STDOUT,check=True)
 print('REPLAYED',seed,flush=True)
remaining=set(range(1001,1011));futures=[]
with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
 while remaining:
  if (out/'STOP.json').exists():raise RuntimeError('Study stopped; no more replays submitted')
  for seed in sorted(remaining.copy()):
   if len(list(out.glob(f'*-{seed}/results-equal.json')))==12:
    futures.append(pool.submit(replay,seed));remaining.remove(seed)
  if remaining:time.sleep(20)
 for f in futures:f.result()
