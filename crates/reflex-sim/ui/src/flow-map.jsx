// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

import React, {useEffect, useId, useMemo, useRef, useState} from 'react';
import {Button} from '@datadog/druids/form/Button';
import {Text} from '@datadog/druids/typography/Text';
import {StatusPill} from '@datadog/druids/pills/StatusPill';
import {UsersIcon} from '@datadog/druids/icons/Users';
import {ServerIcon} from '@datadog/druids/icons/Server';
import {GlobeIcon} from '@datadog/druids/icons/Globe';
import {PlusLightIcon} from '@datadog/druids/icons/PlusLight';
import {MinusLightIcon} from '@datadog/druids/icons/MinusLight';
import {ResetViewIcon} from '@datadog/druids/icons/ResetView';

// Reflex-owned layout: explicit columns preserve the simulator's causal flow.
export function FlowMap({nodes, edges, running, speed=1, onSelect}) {
  const marker=useId().replaceAll(':',''), svg=useRef(null), drag=useRef(null), suppressClick=useRef(false);
  const viewport=useRef(null), [size,setSize]=useState({width:1000,height:650});
  useEffect(()=>{const observer=new ResizeObserver(([entry])=>setSize({width:entry.contentRect.width,height:entry.contentRect.height}));observer.observe(viewport.current);return()=>observer.disconnect();},[]);
  const [camera,setCamera]=useState({x:0,y:0,zoom:1});
  const layout=useMemo(()=>{
    const columns=new Map();
    nodes.forEach(n=>{const col=columns.get(n.column)||[];col.push(n);columns.set(n.column,col);});
    const nodeWidth=208, columnStep=columns.has(3)?232:264, rowGap=14, inset=24;
    const nodeHeight=n=>61+(n.metrics?.length?47:0)+(Number.isFinite(n.ratio)?3:0)+(n.annotation?22:0)+(n.circuits?.length||0)*28;
    const columnHeight=col=>col.reduce((total,n)=>total+nodeHeight(n),0)+Math.max(0,col.length-1)*rowGap;
    const height=Math.max(320,...Array.from(columns.values(),columnHeight))+inset*2;
    const width=Math.max(0,...columns.keys())*columnStep+nodeWidth+inset*2;
    const positions=new Map();
    columns.forEach((col,index)=>{
      let y=(height-columnHeight(col))/2;
      col.forEach(n=>{const h=nodeHeight(n);positions.set(n.key,{...n,x:inset+index*columnStep,y,width:nodeWidth,height:h});y+=h+rowGap;});
    });
    return {positions,width,height};
  },[nodes]);
  // Keep part of the graph visible, including after zooming out from a panned view.
  const constrain=c=>({ ...c,
    x:Math.max(-layout.width*.35,Math.min(layout.width*.35,c.x)),
    y:Math.max(-layout.height*.35,Math.min(layout.height*.35,c.y)),
  });
  const zoom=delta=>setCamera(c=>constrain({...c,zoom:Math.max(.5,Math.min(2.5,c.zoom+delta))}));
  const endDrag=e=>{
    if(drag.current?.id!==e.pointerId)return;
    drag.current=null;
    if(e.currentTarget.hasPointerCapture(e.pointerId))e.currentTarget.releasePointerCapture(e.pointerId);
  };
  return <div className="reflex-flow-map" ref={viewport}>
    <svg ref={svg} className="reflex-flow-svg" viewBox={`0 0 ${size.width} ${size.height}`} aria-label="Service topology"
      onDragStart={e=>e.preventDefault()}
      onPointerDown={e=>{
        if(!e.isPrimary||e.button!==0)return;
        const matrix=svg.current.getScreenCTM();
        if(!matrix)return;
        suppressClick.current=false;
        // Snapshot the coordinate conversion so live layout updates cannot jump the drag.
        drag.current={id:e.pointerId,x:e.clientX,y:e.clientY,camera,inverse:matrix.inverse()};
      }}
      onPointerMove={e=>{
        const d=drag.current;
        if(!d||d.id!==e.pointerId)return;
        if(e.buttons===0){endDrag(e);return;}
        const dx=e.clientX-d.x,dy=e.clientY-d.y;
        if(!suppressClick.current&&Math.hypot(dx,dy)<4)return;
        suppressClick.current=true;
        e.preventDefault();
        if(!e.currentTarget.hasPointerCapture(e.pointerId))e.currentTarget.setPointerCapture(e.pointerId);
        setCamera(constrain({...d.camera,
          x:d.camera.x+dx*d.inverse.a+dy*d.inverse.c,
          y:d.camera.y+dx*d.inverse.b+dy*d.inverse.d,
        }));
      }}
      onPointerUp={endDrag} onPointerCancel={endDrag} onLostPointerCapture={endDrag}
      onClickCapture={e=>{if(suppressClick.current&&e.detail!==0){e.preventDefault();e.stopPropagation();}}}>
      <defs><marker id={marker} markerWidth="7" markerHeight="7" refX="6" refY="3.5" orient="auto"><path d="M1 1 L6 3.5 L1 6" fill="none" stroke="context-stroke"/></marker></defs>
      <g transform={`translate(${(size.width-layout.width)/2+camera.x} ${(size.height-layout.height)/2+camera.y}) translate(${layout.width/2} ${layout.height/2}) scale(${camera.zoom}) translate(${-layout.width/2} ${-layout.height/2})`}>
        {edges.map(edge=>{
          const a=layout.positions.get(edge.sourceId),b=layout.positions.get(edge.targetId);if(!a||!b)return null;
          const same=a.column===b.column, x=a.x+a.width,y=a.y+a.height/2, tx=same?b.x+b.width:b.x,ty=b.y+b.height/2;
          const d=same?`M${x},${y} C${x+45},${y} ${tx+45},${ty} ${tx},${ty}`:`M${x},${y} C${(x+tx)/2},${y} ${(x+tx)/2},${ty} ${tx},${ty}`;
          const color=edge.status==='danger'?'var(--r-bad)':edge.status==='warning'?'var(--r-warn)':'var(--r-edge)';
          return <g key={edge.key} data-flow-edge={edge.key}><path d={d} fill="none" stroke={color} strokeWidth="1.5" opacity={edge.active===false?.5:.85} strokeDasharray={edge.active===false?'3 4':undefined} markerEnd={`url(#${marker})`}/>{running&&edge.active!==false&&Array.from({length:edge.count||3},(_,i)=><circle className="reflex-traffic-dot" key={i} r="2.5" fill={edge.status==='warning'?'var(--r-warn)':'var(--r-ai)'}><animateMotion dur={`${2.6/Math.max(1,speed)}s`} begin={`${-i*.9/Math.max(1,speed)}s`} repeatCount="indefinite" path={d}/></circle>)}</g>;
        })}
        {Array.from(layout.positions.values(),n=>{const Icon=n.kind==='client'?UsersIcon:n.kind==='hub'?GlobeIcon:ServerIcon;const selectable=n.selectable!==false,Tag=selectable?'button':'div';return <foreignObject key={n.key} x={n.x} y={n.y} width={n.width} height={n.height}><Tag type={selectable?"button":undefined} className={`reflex-map-node ${n.kind==='client'?'reflex-map-client':''}`} data-node-kind={n.kind} data-node-status={n.status||'default'} aria-label={selectable?`Inspect ${n.name}`:n.name} aria-pressed={selectable?!!n.selected:undefined} onClick={selectable?()=>onSelect?.(n.key):undefined}>
          {n.annotation&&<span className="reflex-map-annotation">{n.annotation}</span>}<span className="reflex-map-node-heading"><span className="reflex-node-icon"><Icon/></span><span className="reflex-node-label"><Text className="reflex-node-name">{n.name}</Text>{n.subtext&&<Text size="sm" variant="secondary">{n.subtext}</Text>}</span>{n.phase&&<StatusPill className="reflex-node-phase" isSoft size="xs" level={n.status||'default'}>{n.phase}</StatusPill>}</span>
          {n.metrics?.length>0&&<span className="reflex-map-metrics">{n.metrics.map((value,i)=><span key={i} className="reflex-map-metric" data-level={n.metricLevels?.[i]||'default'}><span>{value.label}</span><strong>{value.value}</strong></span>)}</span>}
          {n.circuits?.length>0&&<span className="reflex-gateway-circuits" aria-label="Gateway circuit breakers">{n.circuits.map(circuit=><span key={circuit.name} className="reflex-gateway-circuit" data-node-status={circuit.status}><Text size="sm">{circuit.name}</Text><StatusPill className="reflex-node-phase" isSoft size="xs" level={circuit.status}>{circuit.phase}</StatusPill></span>)}</span>}
          {Number.isFinite(n.ratio)&&<progress style={{'--progress-color':n.ratio>.65?'var(--r-bad)':n.ratio>.3?'var(--r-warn)':'var(--r-ok)'}} max="1" value={Math.max(0,Math.min(1,n.ratio))} aria-label={`${n.name} utilization`}/>}
        </Tag></foreignObject>;})}
      </g>
    </svg>
    <div className="reflex-map-tools" role="group" aria-label="Map controls">
      <Button isBorderless size="sm" icon={PlusLightIcon} ariaLabel="Zoom in" tooltip="Zoom in" onClick={()=>zoom(.2)}/>
      <Button isBorderless size="sm" icon={MinusLightIcon} ariaLabel="Zoom out" tooltip="Zoom out" onClick={()=>zoom(-.2)}/>
      <Button isBorderless size="sm" icon={ResetViewIcon} ariaLabel="Fit map" tooltip="Fit map" onClick={()=>setCamera({x:0,y:0,zoom:Math.min(1,(size.width-48)/layout.width,(size.height-120)/layout.height)})}/>

    </div>
  </div>;
}
