#!/usr/bin/env python3
"""Run the frozen performance-feedback follow-up on the original paired traces."""
import argparse,concurrent.futures,hashlib,json,pathlib,shutil,subprocess,datetime,os,shlex
ROOT=pathlib.Path(__file__).resolve().parents[2]
p=argparse.ArgumentParser();p.add_argument('--workers',type=int,default=6);p.add_argument('--replay',action='store_true');p.add_argument('--env-file',type=pathlib.Path);a=p.parse_args()
if a.env_file and not a.replay:
 for line in a.env_file.read_text().splitlines():
  if line.strip().startswith('export '):line=line.strip()[7:]
  key,sep,value=line.partition('=')
  if sep and key.strip()=='TYPESAFE_API_KEY':
   words=shlex.split(value,comments=True);assert len(words)==1;os.environ[key.strip()]=words[0]
if not a.replay:assert os.environ.get('TYPESAFE_API_KEY'),'Supply TYPESAFE_API_KEY or --env-file'
os.environ.setdefault('REFLEX_STUDY_MIN_CALL_MS','400')
base=ROOT/'output/capacity-study';out=ROOT/'output/capacity-study-feedback';out.mkdir(exist_ok=True)
prompt=ROOT/'studies/capacity/prompts/feedback-v1.txt'
config=dict(prompt=prompt.read_text(),prompt_sha256=hashlib.sha256(prompt.read_bytes()).hexdigest(),baseline=str(base),seeds=list(range(1001,1011)),policies=['jev','jev_toto'],timing='equal',new_toto_requests=0,note='Post-hoc performance-feedback follow-up; two adjacent 60-second observed windows, same efficiency objective, traces, cached forecasts, model and classical baselines.')
config_path=out/'followup-config.json'
if config_path.exists():assert json.loads(config_path.read_text())==config
else:config_path.write_text(json.dumps(config,indent=2))
if not a.replay:
 for src in sorted(base.glob('*/forecasts.json')):
  dst=out/src.parent.name/src.name;dst.parent.mkdir(exist_ok=True)
  if dst.exists():assert hashlib.sha256(src.read_bytes()).digest()==hashlib.sha256(dst.read_bytes()).digest()
  else:shutil.copy2(src,dst)
def run(seed):
 cmd=[str(ROOT/'target/release/examples/capacity_study'),'--output',str(out),'--seeds','1','--first-seed',str(seed),'--forecasts','cache','--policies','jev,jev_toto','--prompt-file',str(prompt),'--performance-feedback','--tuning',str(ROOT/'output/capacity-study-tuning/selected.json')]
 if a.replay:cmd.append('--replay')
 with (out/f'{"replay" if a.replay else "worker"}-{seed}.log').open('w') as log:subprocess.run(cmd,cwd=ROOT,stdout=log,stderr=subprocess.STDOUT,check=True)
 print('REPLAYED' if a.replay else 'FINISHED','SEED',seed,flush=True)
with concurrent.futures.ThreadPoolExecutor(max_workers=a.workers) as pool:list(pool.map(run,range(1001,1011)))
