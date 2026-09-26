// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

import React, {useSyncExternalStore} from 'react';
import {DruidsEnvironment} from '@datadog/druids/layout/DruidsEnvironment';

const key='reflex-theme', listeners=new Set();
let theme='dark';
try { if(localStorage.getItem(key)==='light')theme='light'; } catch {}
function apply(){document.documentElement.dataset.reflexTheme=theme;}
apply();
const subscribe=listener=>{listeners.add(listener);return()=>listeners.delete(listener);};
export function useTheme(){return useSyncExternalStore(subscribe,()=>theme);}
export function toggleTheme(){
  theme=theme==='light'?'dark':'light';
  try {localStorage.setItem(key,theme);} catch {}
  apply();listeners.forEach(listener=>listener());
}
export function ReflexEnvironment({children}){
  const current=useTheme();
  return <DruidsEnvironment defaultThemePreference={current}>{children}</DruidsEnvironment>;
}
