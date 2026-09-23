# Reflex interface

The interface uses only the public `@datadog/druids@0.1.0` package, React, and
Reflex-owned presentation code. Every React root uses the public
`@datadog/druids/layout/DruidsEnvironment` export.

## Build

From the repository root, with Node.js/npm installed:

```sh
npm ci --prefix crates/reflex-sim/ui
npm run build --prefix crates/reflex-sim/ui
cargo run -p reflex-sim --locked -- --playground --port 8742 --no-open
```

Run the npm build before Cargo builds/tests, including on a fresh CI checkout.
The build generates `controls.js/css`, `scenario-ui.js/css`, and `typography.css`
under `src/`. These outputs are ignored by Git and embedded into the Rust binary.
Rebuild the browser assets and restart Rust after UI changes. No Node server or
CDN is required at runtime; exported reports retain their embedded fonts.

## Dependency alignment

Keep React and React DOM on the same version. DRUIDS 0.1.0 declares support for
React 18 and 19. Its `react-popper` and `react-table` dependencies still declare
peer ranges ending at React 18, so the UI uses npm overrides scoped to DRUIDS to
bind those peers to the application's React version. Their published package
contents remain unchanged. Remove the overrides when upstream peer ranges cover
React 19. Check clean installation, `npm ls --all`, the production build, and
table, select, tooltip, and dialog interactions when updating this stack.

## Components and behavior

- `header.jsx` composes the public ProminentTabList, Text, Button, and icons into
  the compact application header. Navigation pauses the current simulation.
- `controls.jsx` owns the Circuit Breaker inspector and playback controls. The
  existing API controller and Rust simulation remain authoritative.
- `scenarios.jsx` supplies the same shell for Scheduler and Recovery,
  retaining the existing forms, validation, dialogs, and API controllers.
- `flow-map.jsx` is a Reflex-owned SVG map with explicit horizontal columns,
  public DRUIDS node content, pan/zoom/fit, keyboard-accessible node buttons,
  and traffic animation. It respects reduced motion and paused/blocked flows.
- `time-chart.jsx` is a Reflex-owned SVG chart with simulation-time axes,
  per-series units, dual scales, and pointer/keyboard value inspection.
  `pressure.jsx` and `scenario-trend.jsx` adapt simulator data to it.
- `policy-comparison.jsx` supplies the policy dropdown and baseline summaries.
- `shared.css` and `typography.css` hold local presentation styles and typography
  values for the header, node metrics/statuses, zoom controls, and chart tooltips.
  Fonts are extracted from the public package by `scripts/build-typography.cjs`.

Check all simulator pages, desktop/mobile layouts, header navigation, map selection,
pan/zoom, pause/start/step/reset, fault and form controls, chart hover/keyboard
values, policy selection, and remote-evidence control restrictions. Live model
inference requires separate server credentials; local policies work without them.
