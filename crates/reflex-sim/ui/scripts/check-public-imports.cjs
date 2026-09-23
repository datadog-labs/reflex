// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

const fs = require('node:fs');
const path = require('node:path');
const assert = require('node:assert/strict');
const root = path.resolve(__dirname, '..');
const packageRoot = path.resolve(path.dirname(require.resolve('@datadog/druids/styles.css')), '..');
const publicExports = Object.keys(JSON.parse(fs.readFileSync(path.join(packageRoot, 'package.json'), 'utf8')).exports);
for (const name of fs.readdirSync(path.join(root, 'src'))) {
  if (!/\.[cm]?[jt]sx?$/.test(name)) continue;
  const source = fs.readFileSync(path.join(root, 'src', name), 'utf8');
  for (const [, specifier] of source.matchAll(/(?:from\s*|import\s*)['"]([^'"]+)['"]/g)) {
    if (specifier.startsWith('@datadog/druids/')) {
      const subpath = './' + specifier.slice('@datadog/druids/'.length);
      assert(publicExports.some(key => key === subpath || (key.endsWith('/*') && subpath.startsWith(key.slice(0, -1)))), `${name}: not a public export: ${specifier}`);
    } else {
      assert(specifier.startsWith('./') || ['react', 'react-dom/client'].includes(specifier), `${name}: unexpected UI dependency: ${specifier}`);
    }
  }
}
assert(!fs.existsSync(path.join(root, 'vendor')), 'Vendored UI code is not allowed');
console.log('Public DRUIDS imports verified.');
