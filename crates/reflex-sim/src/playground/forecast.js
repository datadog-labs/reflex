// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

/* Compare frozen forecasts with subsequently observed values on the same time grid. */
(() => {
  const escape = s => String(s).replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
  const number = v => Number(v).toLocaleString('en-US', {maximumFractionDigits: 2});
  const selected = new Map();
  let signature = '', latestArgs;
  window.renderForecast = (view, title, policy, disabled = false) => {
    latestArgs = [view,title,policy,disabled];
    const panel = document.getElementById('forecast-panel');
    if (!panel) return;
    const runs = view?.comparisons || [];
    if (!runs.some(r => r.origin_ms === selected.get(title))) selected.set(title,runs[0]?.origin_ms);
    const c = runs.find(r => r.origin_ms === selected.get(title));
    const key = JSON.stringify([view,title,policy,disabled,selected.get(title)]);
    if (key === signature && panel.childElementCount) return;
    signature = key;
    const focus = panel.contains(document.activeElement) ? document.activeElement.id : null;
    const f = view?.forecast, remote = view?.source === 'datadog_observations';
    const originLabel = origin => remote ? new Date(1780000000000 + origin).toLocaleTimeString() : `${number(origin/1000)}s`;
    const chart = series => {
      const data = series.points;
      const top = Math.max(1,...data.map(b => Math.max(b.p90,b.actual ?? 0)));
      const x = b => 38 + ((b.from_ms+b.through_ms)/2-c.origin_ms)/c.horizon_ms*300;
      const y = v => 120 - v/top*92;
      const line = field => {let active=false;return data.map(b=>{if(b[field] == null){active=false;return '';}const command=active?'L':'M';active=true;return `${command}${x(b)},${y(b[field])}`;}).join(' ');};
      const band = line('p90') + data.slice().reverse().map(b=>`L${x(b)},${y(b.p10)}`).join(' ') + 'Z';
      const observed = data.filter(b => b.actual != null);
      const label = series.name.replaceAll('_',' ');
      const now = 38 + Math.max(0,Math.min(1,(view.now_ms-c.origin_ms)/c.horizon_ms))*300;
      const nowMarker = view.now_ms >= c.origin_ms && view.now_ms <= c.origin_ms+c.horizon_ms ? `<path d="M${now} 20V120" stroke="#999" stroke-dasharray="2 4"/><text x="${Math.min(305,Math.max(40,now+3))}" y="16">now</text>` : '';
      return `<figure><figcaption>${escape(label)}</figcaption><svg viewBox="0 0 365 163" role="img" aria-label="${escape(label)}: actual observations in blue versus the original forecast in dashed green"><path d="M38 20V120H340" fill="none" stroke="#d4ded0"/><text x="4" y="30">${number(top)}</text><text x="18" y="123">0</text><text x="38" y="145">+0s</text><text x="303" y="145">+${c.horizon_ms/1000}s</text><path d="${band}" fill="#779b73" opacity=".2"/><path d="${line('p50')}" fill="none" stroke="#56784d" stroke-width="2" stroke-dasharray="5 3"/><path d="${line('actual')}" fill="none" stroke="#367caa" stroke-width="2.5"/>${observed.map(b=>`<circle cx="${x(b)}" cy="${y(b.actual)}" r="2.5" fill="#367caa"><title>+${number((b.through_ms-c.origin_ms)/1000)}s: actual ${number(b.actual)}, forecast ${number(b.p50)}</title></circle>`).join('')}${nowMarker}</svg></figure>`;
    };
    panel.innerHTML = `<div class="forecast-heading"><div><span class="eyebrow">TOTO → JEV → REFLEX</span><h2>Looking ahead · ${escape(title)}</h2></div><label class="forecast-toggle"><input type="checkbox" role="switch" id="forecast-toggle" aria-label="Toto forecasting" ${view?.enabled?'checked':''} ${disabled||!view?.configured?'disabled':''}> Toto forecasting <strong>${view?.enabled?'On':'Off'}</strong></label></div><p class="forecast-status">${escape(view?.status || 'Collecting history')}</p>
      ${view?.enabled ? (c ? `<div class="forecast-comparison-controls"><label>Compare forecast from <select id="forecast-origin" aria-label="Forecast to compare">${runs.map(r=>`<option value="${r.origin_ms}" ${r===c?'selected':''}>${escape(originLabel(r.origin_ms))}</option>`).join('')}</select></label><span class="forecast-meta">Available ${number((c.available_at_ms-c.origin_ms)/1000)}s after origin · ${remote?'Datadog observations':'Simulator observations'}</span></div><div class="forecast-charts">${c.series.map(chart).join('')}</div><p class="forecast-meta"><span class="forecast-actual-key">━ Actual</span> · <span class="forecast-predicted-key">┄ Original forecast (p50)</span> · shaded: p10–p90 band. Time is relative to the selected forecast’s origin.</p><p class="forecast-meta">Actuals appear only after a complete matching ten-second bucket is observed${remote?' in Datadog; telemetry arrives with a delay':''}. The selected forecast stays fixed as new observations arrive.</p>` : `<p class="forecast-meta">Collecting history for the first forecast. This run needs ${view?.minimum_history_seconds || 64} ${remote ? "wall-clock seconds plus ingestion delay" : "simulated seconds"} of observed history before its first forecast.</p>`) : '<p class="forecast-meta">Toto calls are off. Jev uses observed state only. Switching this on does not reset the simulation.</p>'}
      ${view?.enabled && f ? `<details><summary>Current Jev forecast evidence</summary><p class="forecast-meta">${f.history_seconds}s history · ${f.horizon_seconds}s horizon · ${number(f.age_ms/1000)}s old. ${policy==='jev'?'Fresh forecasts accompany Jev evaluations.':'The deterministic policy does not use forecasts.'}</p><p>${escape(f.model_provenance)}</p><pre>${escape(JSON.stringify(f,null,2))}</pre></details>` : ''}
      <p class="forecast-note">Forecasts never override Reflex guards. Both lines use matching bucket means; the band averages pointwise quantiles. ${view?.calls||0} / ${view?.limit||60} refresh attempts this run.</p>`;
    const select=panel.querySelector('#forecast-origin');
    if(select)select.addEventListener('change',e=>{selected.set(title,Number(e.target.value));signature='';window.renderForecast(...latestArgs);});
    if(focus)panel.querySelector(`#${focus}`)?.focus({preventScroll:true});
  };
})();
