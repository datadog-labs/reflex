// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

import React, {useEffect, useId, useMemo, useRef, useState} from 'react';
import {Text} from '@datadog/druids/typography/Text';

const palette={purple:'var(--r-ai)',orange:'var(--r-warn)',blue:'var(--r-ai)'};
const color=s=>palette[s.color]||s.color||palette.purple;
const fmt=value=>Number(value).toLocaleString('en-US',{maximumFractionDigits:2});
const axisFmt=value=>Math.abs(value)>=1000?`${fmt(value/1000)}k`:fmt(value);
export const simulationTime=ms=>`${Math.floor(ms/60000)}:${String(Math.floor(ms/1000)%60).padStart(2,'0')}`;
// Round upward to readable intervals without displaying fractional job/replica counts.
const scaleFor=(maximum,integer)=>{
  const raw=Math.max(maximum,1)/4,power=10**Math.floor(Math.log10(raw));
  const step=Math.max(integer?1:0,[1,2,2.5,5,10].find(n=>n*power>=raw)*power);
  return {max:Math.ceil(Math.max(maximum,1)/step)*step,step};
};

// Reflex-owned SVG chart. Coordinates are CSS pixels so type and strokes do not shrink.
export function TimeChart({series,bands=[],markers=[],start,end,leftLabel,rightLabel,leftMax,rightMax,leftInteger=false,rightInteger=false,label,timeFormat=simulationTime,emptyMessage="Start or step traffic to collect history."}) {
  const id=useId().replaceAll(':',''),container=useRef(null),[width,setWidth]=useState(440),[hover,setHover]=useState(null),[pointerY,setPointerY]=useState(null);
  useEffect(()=>{
    const observer=new ResizeObserver(([entry])=>{if(entry.contentRect.width>0)setWidth(entry.contentRect.width);});
    observer.observe(container.current);return ()=>observer.disconnect();
  },[]);
  const height=210,left=42,right=Math.max(left+40,width-(rightLabel?44:14)),top=28,bottom=180;
  const maxima=useMemo(()=>[false,true].map(axis=>Math.max(1,...series.filter(s=>!!s.right===axis).flatMap(s=>s.points.filter(([t,v])=>t>=start&&t<=end&&Number.isFinite(v)).map(([,v])=>v)),...bands.filter(s=>!!s.right===axis).flatMap(s=>s.points.filter(([t,lo,hi])=>t>=start&&t<=end&&Number.isFinite(lo)&&Number.isFinite(hi)).map(([,lo,hi])=>hi)))),[series,bands,start,end]);
  const scales=[scaleFor(leftMax??maxima[0],leftInteger),scaleFor(rightMax??maxima[1],rightInteger)];
  const x=t=>left+(t-start)/Math.max(1,end-start)*(right-left);
  const y=(v,axis)=>bottom-v/scales[axis?1:0].max*(bottom-top);
  const times=useMemo(()=>[...new Set(series.flatMap(s=>s.points.filter(([t,v])=>t>=start&&t<=end&&Number.isFinite(v)).map(([t])=>t)))].sort((a,b)=>a-b),[series,start,end]);
  const nearest=t=>times.reduce((best,v)=>Math.abs(v-t)<Math.abs(best-t)?v:best,times[0]);
  const active=hover==null||!times.length?null:nearest(hover);
  const values=active==null?[]:series.map(s=>({...s,value:s.points.find(([t])=>t===active)?.[1]}));
  const candidates=values.filter(s=>Number.isFinite(s.value));
  const highlighted=candidates.length?candidates.reduce((best,s)=>pointerY!=null&&Math.abs(y(s.value,!!s.right)-pointerY)<Math.abs(y(best.value,!!best.right)-pointerY)?s:best):null;
  const tooltipWidth=Math.min(220,width-16),activeX=active==null?left:x(active);
  const onLeft=activeX>width/2;
  const tooltipLeft=Math.max(8,Math.min(width-tooltipWidth-8,onLeft?activeX-tooltipWidth-8:activeX+8));
  const tooltipTop=highlighted?Math.max(top,Math.min(bottom-44,y(highlighted.value,!!highlighted.right)-40)):top;
  const tickCount=width<360?2:4;
  return <div ref={container} className="reflex-time-chart" data-chart-engine="reflex-svg">
    <svg viewBox={`0 0 ${width} ${height}`} style={{height}} role="img" aria-label={label} aria-describedby={`${id}-hint`} tabIndex="0"
      onPointerMove={e=>{const rect=e.currentTarget.getBoundingClientRect(),px=e.clientX-rect.left,py=e.clientY-rect.top;if(px<left||px>right||py<top||py>bottom){setHover(null);return;}setPointerY(py);setHover(start+(px-left)/(right-left)*(end-start));}}
      onPointerLeave={()=>setHover(null)} onFocus={()=>{setPointerY(null);setHover(times.at(-1)??null);}} onBlur={()=>setHover(null)}
      onKeyDown={e=>{if(!['ArrowLeft','ArrowRight','Escape'].includes(e.key))return;e.preventDefault();if(e.key==='Escape'){setHover(null);return;}const i=active==null?times.length-1:times.indexOf(active);setHover(times[Math.max(0,Math.min(times.length-1,i+(e.key==='ArrowLeft'?-1:1)))]??null);}}>
      <defs><clipPath id={id}><rect x={left-1} y={top-3} width={right-left+2} height={bottom-top+6}/></clipPath></defs>
      <text x={left} y="14" className="reflex-chart-axis-title">{leftLabel}</text>{rightLabel&&<text x={right} y="14" textAnchor="end" className="reflex-chart-axis-title">{rightLabel}</text>}
      {Array.from({length:Math.round(scales[0].max/scales[0].step)+1},(_,i)=>i*scales[0].step).map(v=><g key={v}><line x1={left} x2={right} y1={y(v,false)} y2={y(v,false)} className="reflex-chart-grid"/><text x={left-8} y={y(v,false)+4} textAnchor="end">{axisFmt(v)}</text></g>)}
      {rightLabel&&Array.from({length:Math.round(scales[1].max/scales[1].step)+1},(_,i)=>i*scales[1].step).map(v=><text key={v} x={right+8} y={y(v,true)+4}>{axisFmt(v)}</text>)}
      {Array.from({length:tickCount+1},(_,i)=>i/tickCount).map(t=><text key={t} x={left+(right-left)*t} y={bottom+22} textAnchor="middle">{timeFormat(start+(end-start)*t)}</text>)}
      <g clipPath={`url(#${id})`}>{bands.map(band=>{
        const segments=[];let segment=[];
        for(const point of band.points){
          if(point.slice(1).every(Number.isFinite))segment.push(point);
          else if(segment.length){segments.push(segment);segment=[];}
        }
        if(segment.length)segments.push(segment);
        return <g key={band.name}>{segments.map((points,i)=>{
          const upper=points.map(([t,lo,hi],j)=>`${j?'L':'M'}${x(t)},${y(hi,!!band.right)}`).join(' ');
          const lower=points.slice().reverse().map(([t,lo])=>`L${x(t)},${y(lo,!!band.right)}`).join(' ');
          return <path key={i} d={`${upper} ${lower} Z`} fill={color(band)} fillOpacity=".15"><title>{band.name}</title></path>;
        })}</g>;
      })}{markers.filter(m=>m.at>=start&&m.at<=end).map((m,i)=><line key={`marker-${i}`} x1={x(m.at)} x2={x(m.at)} y1={top} y2={bottom} stroke={m.color||'var(--r-ink-3)'} strokeDasharray="3 5" opacity=".6"><title>{m.label}</title></line>)}{series.map(s=>{
        let move=true;
        const d=s.points.map(([t,v])=>{if(!Number.isFinite(v)){move=true;return '';}const command=move?`M${x(t)},${y(v,!!s.right)}`:s.step?`H${x(t)}V${y(v,!!s.right)}`:`L${x(t)},${y(v,!!s.right)}`;move=false;return command;}).join(' ');
        const finitePoints=s.points.filter(([,v])=>Number.isFinite(v));
        return <g key={s.name} opacity={highlighted&&highlighted.name!==s.name?0.3:1}><path d={d} fill="none" stroke={color(s)} strokeWidth="2" strokeLinejoin="round" strokeDasharray={s.dashed?'5 4':undefined}/>{finitePoints.length===1&&<circle cx={x(finitePoints[0][0])} cy={y(finitePoints[0][1],!!s.right)} r="3" fill={color(s)}/>}</g>;
      })}
      {active!=null&&<g><line x1={activeX} x2={activeX} y1={top} y2={bottom} stroke="var(--r-ink)" strokeWidth="1"/>{highlighted&&<circle cx={activeX} cy={y(highlighted.value,!!highlighted.right)} r="3" fill="var(--r-ink)"/>}</g>}
      </g>
    </svg>
    {active!=null&&<>
      {highlighted&&<div className={`reflex-chart-tooltip${onLeft?' reflex-chart-tooltip-left':''}`} role="status" style={{left:tooltipLeft,top:tooltipTop,width:tooltipWidth}}>
        <span className="reflex-chart-series-name" style={{background:color(highlighted),color:'var(--r-on-accent)'}}>{highlighted.name}</span>
        <span className="reflex-chart-value">{fmt(highlighted.value)}{highlighted.unit?` ${highlighted.unit}`:''}</span>
      </div>}
      <span className="reflex-chart-time" aria-label={`Time ${timeFormat(active)}`} style={{left:Math.max(32,Math.min(width-32,activeX)),top:bottom+7}}>{timeFormat(active)}</span>
    </>}
    <div className="reflex-chart-legend">{series.map(s=><span key={s.name}><i style={{borderColor:color(s),borderTopStyle:s.dashed?'dashed':'solid'}}/><Text size="sm" variant="secondary">{s.name}</Text></span>)}{bands.map(b=><span key={b.name}><i className="reflex-chart-band-key" style={{background:color(b),borderColor:color(b)}}/><Text size="sm" variant="secondary">{b.name}</Text></span>)}</div>
    <span id={`${id}-hint`} className="reflex-chart-sr-only">Use left and right arrow keys to inspect samples. Escape dismisses the tooltip.</span>
    {!times.length&&<Text size="xs" variant="secondary">{emptyMessage}</Text>}
  </div>;
}
