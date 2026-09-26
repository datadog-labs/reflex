// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

import React from 'react';
import {createRoot} from 'react-dom/client';
import {SoftToggle} from '@datadog/druids/form/SoftToggle';
import {Button} from '@datadog/druids/form/Button';
import {NetworkIcon} from '@datadog/druids/icons/Network';
import {ContainerImageIcon} from '@datadog/druids/icons/ContainerImage';
import {ConnectionIcon} from '@datadog/druids/icons/Connection';
import {SunIcon} from '@datadog/druids/icons/Sun';
import {MoonIcon} from '@datadog/druids/icons/Moon';
import {HelpIcon} from '@datadog/druids/icons/Help';
import {DownloadIcon} from '@datadog/druids/icons/Download';
import {ReflexEnvironment,useTheme,toggleTheme} from './theme.jsx';
import './shared.css';

// Do not paint the legacy shell, intermediate React mounts, or fallback-font
// tab widths. The static HTML reserves header space while the page initializes.
let revealScheduled=false;
function revealLayout(){
  if(revealScheduled)return;
  revealScheduled=true;
  requestAnimationFrame(()=>{
    document.fonts.ready.then(()=>requestAnimationFrame(()=>requestAnimationFrame(()=>{
      document.documentElement.removeAttribute('data-ui-loading');
    })));
  });
}
const root=createRoot(document.getElementById('product-header'));
function Header({disabled,connected,navigate,onHelp}){
 const theme=useTheme();
 return <ReflexEnvironment><header className="reflex-product-header">
  <div className="reflex-brand"><NetworkIcon/><span>Reflex</span></div>
  <nav className="reflex-product-tabs" aria-label="Simulation scenarios"><SoftToggle ariaLabel="Simulation screen" value={location.pathname} options={[
   {value:'/',label:'Circuit breaker',icon:ConnectionIcon,isDisabled:disabled},
   {value:'/scheduler',label:'Resource scheduler',icon:ContainerImageIcon,isDisabled:disabled},
  ]} onChange={path=>{if(path!==location.pathname)navigate?.(path);}}/></nav>
  <div className="product-header-actions">
   <span className="engine-status" data-connected={connected} role="status">{connected?'Engine connected':'Engine disconnected'}</span>
   <Button isBorderless icon={theme==='dark'?SunIcon:MoonIcon} ariaLabel={`Switch to ${theme==='dark'?'light':'dark'} theme`} onClick={toggleTheme}/>
   <Button className="help-action" isBorderless icon={HelpIcon} label="How it works" isTitleCased={false} onClick={onHelp}/>
   <Button isPrimary icon={DownloadIcon} label="Export run" isTitleCased={false} isDisabled={!connected} onClick={()=>{const a=document.createElement('a');a.href=location.pathname==='/scheduler'?'/api/scheduler/export':'/api/export';a.download='reflex-run.json';a.click();}}/>
  </div>
 </header></ReflexEnvironment>;
}
export function renderHeader({disabled=true,connected=false,navigate,onHelp}={}){
 root.render(<Header {...{disabled,connected,navigate,onHelp}}/>);revealLayout();
}
