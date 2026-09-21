#!/usr/bin/env python3
"""Plot the configured workload shapes; these are rates, not sampled arrivals."""
from pathlib import Path
import numpy as np
import matplotlib
matplotlib.use('Agg')
import matplotlib.pyplot as plt

ROOT = Path(__file__).resolve().parents[2]
REPORT = ROOT / 'output/capacity-study/report'

def render(report=REPORT):
    t = np.arange(1801, dtype=float)
    patterns = {
        'Steady': np.ones_like(t),
        'Spike': np.where((t >= 600) & (t < 900), 2.5, 0.8),
        'Ramp': np.select([t < 300, t < 900, t < 1200, t < 1650],
                          [0.7, 0.7 + (t - 300) / 600 * 1.5, 2.2,
                           2.2 - (t - 1200) / 450 * 1.5], default=0.7),
        'Waves': 1.3 + 0.7 * np.sin(t / 300 * 2 * np.pi),
        'Bursts': np.where(t % 240 < 40, 3.0, 0.7),
    }
    # Expected offered work uses estimated durations, as in the controller evidence.
    rates, cpu, memory, duration = map(np.array, ([0.35,0.16,0.14],[1,4,1],[1,2,8],[6,12,18]))
    base_cpu = np.sum(rates * cpu * duration)
    base_memory = np.sum(rates * memory * duration)
    swapped = (t >= 600) & (t < 1200)
    cpu_ratio = np.where(swapped, base_memory / base_cpu, 1.)
    memory_ratio = np.where(swapped, base_cpu / base_memory, 1.)
    plt.rcParams.update({'font.family':'DejaVu Sans','font.size':11,'axes.spines.top':False,'axes.spines.right':False,'svg.fonttype':'none'})
    fig, axes = plt.subplots(3,2,figsize=(13.5,10.5),sharex=True,sharey=True)
    descriptions = ['Unchanging expected traffic','Five-minute surge, then recovery','Gradual rise, plateau, and decline','Smooth five-minute cycles','40-second surges every four minutes']
    blue, orange, gray = '#126B8A','#C76A18','#657684'
    for index, ((name,y),description,ax) in enumerate(zip(patterns.items(),descriptions,axes.flat)):
        ax.step(t/60,y,where='post',color=blue,lw=2.3)
        ax.fill_between(t/60,0,y,step='post',color=blue,alpha=0.07)
        ax.set_title(f'{chr(97+index)}. {name}',loc='left',fontweight='bold',fontsize=14,pad=27)
        ax.text(0,1.035,description,transform=ax.transAxes,color='#516372',fontsize=10)
        ax.set_ylabel('Arrival-rate multiplier')
    ax = axes.flat[-1]
    ax.axvspan(10,20,color=orange,alpha=.065)
    ax.step(t/60,cpu_ratio,where='post',color=blue,lw=2.3,label='CPU work')[0].set_gid('series--cpu--line')
    ax.step(t/60,memory_ratio,where='post',color=orange,lw=2.3,label='Memory work')[0].set_gid('series--memory--line')
    ax.plot(t/60,np.ones_like(t),color=gray,lw=1.8,ls='--',label='Arrival rate')[0].set_gid('series--arrival--line')
    ax.set_title('f. Resource mix',loc='left',fontweight='bold',fontsize=14,pad=27)
    ax.text(0,1.035,'Same traffic; different resource needs during minutes 10–20',transform=ax.transAxes,color='#516372',fontsize=10)
    ax.set_ylabel('Multiplier of each usual level')
    ax.legend(loc='upper right',fontsize=9,frameon=False)
    ax.annotate(f'{base_memory/base_cpu:.2f}× CPU',xy=(15,base_memory/base_cpu),xytext=(15,2.35),ha='center',color=blue,fontsize=10).set_gid('series--cpu--annotation')
    ax.annotate(f'{base_cpu/base_memory:.2f}× memory',xy=(15,base_cpu/base_memory),xytext=(15,.14),ha='center',color=orange,fontsize=10).set_gid('series--memory--annotation')
    for ax in axes.flat:
        ax.set_xlim(0,30); ax.set_ylim(0,3.35)
        ax.set_xticks(np.arange(0,31,5));ax.set_yticks([0,1,2,3],['0×','1×','2×','3×'])
        ax.grid(alpha=.17);ax.axhline(1,color=gray,lw=.7,ls=':',alpha=.6)
        ax.tick_params(labelbottom=True)
        ax.set_xlabel('Minutes after warm-up')
    fig.suptitle('The six workload patterns',fontsize=22,fontweight='bold',x=.07,ha='left',y=.995)
    fig.text(.07,.951,'Configured expected demand over the 30-minute measurement period',fontsize=12,color='#516372')
    fig.text(.07,.02,'1× traffic = 0.65 requests/s at moderate load, or 0.975 requests/s at heavy load.\nActual arrivals are random. Resource work is normalized to its own usual CPU or memory level.',fontsize=10,color='#516372',va='bottom')
    fig.tight_layout(rect=[0,.07,1,.94],h_pad=2.5,w_pad=3)
    report.mkdir(parents=True,exist_ok=True)
    for extension in ['png','svg']:
        if extension=='svg':axes.flat[-1].get_legend().remove()
        fig.savefig(report/f'workload-patterns.{extension}',dpi=180,bbox_inches='tight',facecolor='white')
    plt.close(fig)

if __name__ == '__main__':
    render()
    print(REPORT/'workload-patterns.png')
