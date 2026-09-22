import React from 'react';
import { createRoot } from 'react-dom/client';
import { DruidsEnvironment } from '@datadog/druids/layout/DruidsEnvironment';
import { ProminentTabList } from '@datadog/druids/nav/ProminentTabList';
import { Text } from '@datadog/druids/typography/Text';
import './shared.css';
import { Button } from '@datadog/druids/form/Button';
import { NetworkIcon } from '@datadog/druids/icons/Network';
import { HelpIcon } from '@datadog/druids/icons/Help';
import { DownloadIcon } from '@datadog/druids/icons/Download';

const compactQuery = matchMedia('(max-width: 600px)');
const subscribeCompact = callback => {
  compactQuery.addEventListener('change', callback);
  return () => compactQuery.removeEventListener('change', callback);
};
const getCompact = () => compactQuery.matches;

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
  { value: '/recovery', label: 'Recovery' },
];

function Header({ disabled, navigate, onHelp, exportHref, exportLabel }) {
  const compact = React.useSyncExternalStore(subscribeCompact, getCompact);
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
      <div className="product-header-actions">
        <Button id="product-help" label={compact ? undefined : "How it works"} ariaLabel="How it works" icon={HelpIcon} isBorderless isTitleCased={false} onClick={onHelp}/>
        <Button label={compact ? undefined : exportLabel} ariaLabel={exportLabel} icon={DownloadIcon} isTitleCased={false} href={exportHref}/>
      </div>
    </header>
  </DruidsEnvironment>;
}

export function renderHeader({ disabled = true, navigate, onHelp, exportHref="/api/export", exportLabel="Export incident" } = {}) {
  root.render(<Header disabled={disabled} navigate={navigate} onHelp={onHelp} exportHref={exportHref} exportLabel={exportLabel} />);
  revealLayout();
}
