// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

import React from 'react';
import { createRoot } from 'react-dom/client';
import { DruidsEnvironment } from '@datadog/druids/layout/DruidsEnvironment';
import { ProminentTabList } from '@datadog/druids/nav/ProminentTabList';
import { Text } from '@datadog/druids/typography/Text';
import './shared.css';
import { NetworkIcon } from '@datadog/druids/icons/Network';

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
const root = createRoot(document.getElementById('product-header'));
const SCENARIOS = [
  { value: '/', label: 'Circuit Breaker' },
  { value: '/scheduler', label: 'Resource Scheduler' },
];

function Header({ disabled, navigate }) {
  const tabs = React.useMemo(() => SCENARIOS.map(tab => ({
    ...tab,
    isDisabled: disabled,
    dataAttrs: { 'data-scenario': tab.value, 'data-selected': tab.value === location.pathname },
  })), [disabled]);
  const onTabChange = React.useCallback(path => {
    if (path !== location.pathname) navigate?.(path);
  }, [navigate]);
  return <DruidsEnvironment defaultThemePreference="light">
    <header className="reflex-product-header">
      <div className="reflex-brand"><NetworkIcon/><Text weight="bold" size="xl">Reflex</Text></div>
      <nav className="reflex-product-tabs" aria-label="Simulation scenarios"><ProminentTabList impact="low" hasBorder={false} hasRoundedTabs={false} tabs={tabs} selectedTab={location.pathname} onTabChange={onTabChange}/></nav>
    </header>
  </DruidsEnvironment>;
}

export function renderHeader({ disabled = true, navigate } = {}) {
  root.render(<Header disabled={disabled} navigate={navigate} />);
  revealLayout();
}
