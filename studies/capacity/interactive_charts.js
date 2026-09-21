(() => {
  document.querySelectorAll('[data-chart]').forEach(chart => {
    const buttons = Array.from(chart.querySelectorAll('[data-pick]'));
    const keys = buttons.map(button => button.dataset.pick);
    // Keep thin lines easy to select without changing their visible width.
    chart.querySelectorAll('[data-series] path').forEach(path => {
      if (!(path.getAttribute('style') || '').includes('fill: none')) return;
      const hit = path.cloneNode(false);
      hit.removeAttribute('id');
      hit.setAttribute('style', 'fill:none;stroke:transparent;stroke-width:10px;pointer-events:stroke');
      hit.setAttribute('vector-effect', 'non-scaling-stroke');
      hit.setAttribute('aria-hidden', 'true');
      path.before(hit);
    });
    let visible = new Set(keys);
    let focused = false;
    const overlaySelect = chart.querySelector('[data-workload-select]');
    const draw = () => {
      if (overlaySelect) chart.querySelectorAll('[data-workload]').forEach(group => {
        group.toggleAttribute('data-muted', group.dataset.workload !== overlaySelect.value);
      });
      chart.querySelectorAll('[data-series]').forEach(group => {
        group.toggleAttribute('data-muted', !visible.has(group.dataset.series));
      });
      buttons.forEach(button => button.setAttribute('aria-pressed', String(visible.has(button.dataset.pick))));
      chart.querySelector('.chart-status').textContent = visible.size === keys.length
        ? 'Showing all series'
        : 'Showing: ' + buttons.filter(button => visible.has(button.dataset.pick)).map(button => button.textContent.trim()).join(', ');
    };
    if (overlaySelect) overlaySelect.addEventListener('change', draw);
    chart.addEventListener('click', event => {
      if (event.target.closest('[data-reset]')) {
        visible = new Set(keys);
        focused = false;
      } else {
        const target = event.target.closest('[data-pick], [data-series]');
        if (!target) return;
        const key = target.dataset.pick || target.dataset.series;
        if (!focused) {
          visible = new Set([key]);
          focused = true;
        } else {
          if (visible.has(key)) visible.delete(key); else visible.add(key);
          if (!visible.size) { visible = new Set(keys); focused = false; }
        }
      }
      draw();
    });
    draw();
    chart.hidden = false;
    chart.parentElement.classList.add('chart-enhanced');
  });
})();
