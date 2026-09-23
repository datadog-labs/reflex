// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

import React, {useId, useMemo, useRef, useState} from 'react';
import {Button} from '@datadog/druids/form/Button';
import {Text} from '@datadog/druids/typography/Text';
import {StatusPill} from '@datadog/druids/pills/StatusPill';
import {UsersIcon} from '@datadog/druids/icons/Users';
import {ServerIcon} from '@datadog/druids/icons/Server';
import {GlobeIcon} from '@datadog/druids/icons/Globe';
import {PanelRightShrinkIcon} from '@datadog/druids/icons/PanelRightShrink';
import {PlusLightIcon} from '@datadog/druids/icons/PlusLight';
import {MinusLightIcon} from '@datadog/druids/icons/MinusLight';
import {ResetViewIcon} from '@datadog/druids/icons/ResetView';
import {PanelRightGrowIcon} from '@datadog/druids/icons/PanelRightGrow';

// Reflex-owned layout: explicit columns preserve the simulator's causal flow.
export function FlowMap({nodes, edges, running, speed=1, onSelect, panelOpen, setPanelOpen}) {
  const marker=useId().replaceAll(':',''), svg=useRef(null), drag=useRef(null), suppressClick=useRef(false);
  const [camera,setCamera]=useState({x:0,y:0,zoom:1});
  const layout=useMemo(()=>{
    const columns=new Map();
    nodes.forEach(n=>{const col=columns.get(n.column)||[];col.push(n);columns.set(n.column,col);});
    const nodeWidth=240, columnStep=columns.has(3)?312:352, rowGap=20, inset=20;
    const nodeHeight=n=>n.kind==='client'?100:56+(n.metrics?.length||0)*36+(n.subtext?11+16*((n.subtextLines||1)-1):0)+(Number.isFinite(n.ratio)?33:0)+(n.annotation?26:0)+(n.circuits?.length||0)*36;
    const columnHeight=col=>col.reduce((total,n)=>total+nodeHeight(n),0)+Math.max(0,col.length-1)*rowGap;
    const height=Math.max(400,...Array.from(columns.values(),columnHeight))+inset*2;
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
  return <div className="reflex-flow-map">
    <svg ref={svg} className="reflex-flow-svg" viewBox={`0 0 ${layout.width} ${layout.height}`} aria-label="Service topology"
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
      <defs><marker id={marker} markerWidth="8" markerHeight="8" refX="7" refY="4" orient="auto"><path d="M1 1 L7 4 L1 7" fill="none" stroke="context-stroke"/></marker></defs>
      <g transform={`translate(${camera.x} ${camera.y}) translate(${layout.width/2} ${layout.height/2}) scale(${camera.zoom}) translate(${-layout.width/2} ${-layout.height/2})`}>
        {edges.map(edge=>{
          const a=layout.positions.get(edge.sourceId),b=layout.positions.get(edge.targetId);if(!a||!b)return null;
          const same=a.column===b.column, x=a.x+a.width,y=a.y+a.height/2, tx=same?b.x+b.width:b.x,ty=b.y+b.height/2;
          const d=same?`M${x},${y} C${x+45},${y} ${tx+45},${ty} ${tx},${ty}`:`M${x},${y} C${(x+tx)/2},${y} ${(x+tx)/2},${ty} ${tx},${ty}`;
          const color=edge.status==='danger'?'#eb364b':edge.status==='warning'?'#f99d02':'rgba(94,118,141,.8)';
          return <g key={edge.key} data-flow-edge={edge.key}><path d={d} fill="none" stroke={color} strokeWidth="2" strokeDasharray={edge.active===false?'4 4':undefined} markerEnd={`url(#${marker})`}/>{running&&edge.active!==false&&Array.from({length:edge.count||3},(_,i)=><circle className="reflex-traffic-dot" key={i} r="3" fill={edge.status==='warning'?'#b77610':'#006bc2'}><animateMotion dur={`${3/Math.max(1,speed)}s`} begin={`${-i/Math.max(1,speed)}s`} repeatCount="indefinite" path={d}/></circle>)}</g>;
        })}
        {Array.from(layout.positions.values(),n=>{const Icon=n.kind==='client'?UsersIcon:n.kind==='hub'?GlobeIcon:ServerIcon;return <foreignObject key={n.key} x={n.x} y={n.y} width={n.width} height={n.height}><button type="button" className={`reflex-map-node ${n.kind==='client'?'reflex-map-client':''}`} data-node-kind={n.kind} data-node-status={n.status||'default'} aria-label={`Inspect ${n.name}`} aria-pressed={!!n.selected} onClick={()=>onSelect?.(n.key)}>
          {n.annotation&&<span className="reflex-map-annotation">{n.annotation}</span>}<span className="reflex-map-node-heading"><Icon/><span className="reflex-node-label"><Text className="reflex-node-name">{n.name}</Text>{n.subtext&&<Text size="sm" variant="secondary">{n.subtext}</Text>}</span>{n.phase&&<StatusPill className="reflex-node-phase" size="xs" level={n.status||'default'}>{n.phase}</StatusPill>}</span>
          {n.metrics?.length>0&&<span className="reflex-map-metrics">{n.metrics.map((value,i)=><Text key={i} size="sm" className="reflex-map-metric" data-level={n.metricLevels?.[i]||'default'}>{value}</Text>)}</span>}
          {n.circuits?.length>0&&<span className="reflex-gateway-circuits" aria-label="Gateway circuit breakers">{n.circuits.map(circuit=><span key={circuit.name} className="reflex-gateway-circuit" data-node-status={circuit.status}><Text size="sm">{circuit.name}</Text><StatusPill className="reflex-node-phase" size="xs" level={circuit.status}>{circuit.phase}</StatusPill></span>)}</span>}
          {Number.isFinite(n.ratio)&&<progress style={{'--progress-color':n.ratio>.65?'#eb364b':n.ratio>.3?'#f99d02':'#41c464'}} max="1" value={Math.max(0,Math.min(1,n.ratio))} aria-label={`${n.name} utilization`}/>}
        </button></foreignObject>;})}
      </g>
    </svg>
    <div className="reflex-map-tools" role="group" aria-label="Map controls">
      <Button isBorderless size="sm" icon={PlusLightIcon} ariaLabel="Zoom in" tooltip="Zoom in" onClick={()=>zoom(.2)}/>
      <Button isBorderless size="sm" icon={MinusLightIcon} ariaLabel="Zoom out" tooltip="Zoom out" onClick={()=>zoom(-.2)}/>
      <Button isBorderless size="sm" icon={ResetViewIcon} ariaLabel="Fit map" tooltip="Fit map" onClick={()=>setCamera({x:0,y:0,zoom:1})}/>
      <span className="reflex-map-tools-divider"/>
      <Button isBorderless={!panelOpen} isPrimary={panelOpen} size="sm" icon={panelOpen?PanelRightShrinkIcon:PanelRightGrowIcon} ariaLabel={panelOpen?'Hide details':'Show details'} tooltip={panelOpen?'Hide details':'Show details'} onClick={()=>setPanelOpen(!panelOpen)}/>
    </div>
  </div>;
}
