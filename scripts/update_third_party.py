#!/usr/bin/env python3
"""Build the third-party inventory from locked dependencies and upstream notices.

Run with the Python interpreter containing report-requirements.txt.
Network downloads are cached under ignored output/third-party-cache.
"""
import argparse
import base64
import concurrent.futures
import csv
import hashlib
import importlib.metadata
import io
import json
from pathlib import Path
import re
import subprocess
import tarfile
import sys
import urllib.parse
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
CACHE = ROOT / 'output/third-party-cache'
ROWS = {}
EVIDENCE = {}


def download(url, integrity=None):
    CACHE.mkdir(parents=True, exist_ok=True)
    path = CACHE / hashlib.sha256(url.encode()).hexdigest()
    data = path.read_bytes() if path.exists() else urllib.request.urlopen(url, timeout=60).read()
    if integrity:
        algorithm, expected = integrity.split('-', 1)
        assert base64.b64encode(hashlib.new(algorithm, data).digest()).decode() == expected, url
    if not path.exists():
        path.write_bytes(data)
    return data


def credits(text):
    """Preserve attribution text; never substitute package authors for copyright holders."""
    lines = text.splitlines()
    found = []
    for i, line in enumerate(lines):
        # Require an actual assertion, not clauses about "copyright holders".
        joined = line + (' ' + lines[i+1] if i+1 < len(lines) else '')
        named_notice = re.match(r'^\s*(?:[/*#]\s*)*Copyright (?!Holder|Holders|Statement|License|Notice|Owner|Law|\[)[A-Z][a-zA-Z]+', line)
        if not named_notice and not re.search(r'(?:copyright\s*(?:[©:]|\(c\))?\s*(?:19|20)\d{2}|©\s*(?:19|20)\d{2}|\(c\)\s*(?:19|20)\d{2}|copyright\s*\(c\)\s*[A-Z]|copyright Node\.js contributors)', joined, re.I):
            continue
        if not re.search(r'copyright|©|\(c\)', line, re.I):
            continue
        if any(x in line.lower() for x in ['[yyyy]', '[name of', '<year>']):
            continue
        value = line.strip().strip('/*# ').strip()
        if len(value) < 180 and i+1 < len(lines) and re.search(r'(?:\d|,|and|by|\(c\))\s*$', value):
            value += ' ' + lines[i+1].strip().strip('/*# ')
        if value and value not in found:
            found.append(value)
    return ' | '.join(found)


def retain_notices(component, notices):
    """Keep exact notice texts with content hashes, deduplicated across packages."""
    result = []
    for name, text in notices:
        digest = hashlib.sha256(text.encode()).hexdigest()
        relative = 'third_party/notices/' + digest + '.txt'
        destination = ROOT / relative
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_text(text)
        result.append({'upstream_path': name, 'file': relative, 'sha256': digest})
    EVIDENCE.setdefault(component, {'sources': [], 'scopes': []})['notices'] = result


def infer_license(text):
    text = re.sub(r'\s+', ' ', text)
    if 'creativecommons.org/publicdomain/zero/1.0/' in text:
        return 'CC0-1.0'
    if 'zlib License' in text or ('Altered source versions must be plainly marked' in text and 'This notice may not be removed' in text):
        return 'Zlib'
    if 'SIL OPEN FONT LICENSE Version 1.1' in text:
        return 'OFL-1.1'
    if 'Permission is hereby granted, free of charge' in text and 'THE SOFTWARE IS PROVIDED' in text:
        return 'MIT'
    if 'Permission to use, copy, modify, and/or distribute' in text:
        return 'ISC'
    if 'Redistribution and use in source and binary forms' in text:
        return 'BSD-3-Clause' if ('Neither the name' in text or 'neither the name' in text) else 'BSD-2-Clause'
    if 'Apache License' in text and 'Version 2.0' in text:
        return 'Apache-2.0'
    return 'NOASSERTION'


def add(component, origin, license_id, copyright_text, source, scope):
    license_id = {'MIT/Apache-2.0': 'MIT OR Apache-2.0', 'UNKNOWN': 'NOASSERTION', 'BaKoMa Fonts Licence': 'LicenseRef-BaKoMa'}.get(license_id, license_id or 'NOASSERTION')
    copyright_text = copyright_text or 'NOASSERTION — no copyright notice identified in supplied files'
    previous = ROWS.get(component)
    if previous:
        if previous['License'] == 'NOASSERTION' and license_id != 'NOASSERTION':
            previous['License'] = license_id
        if previous['Copyright'].startswith('NOASSERTION') and not copyright_text.startswith('NOASSERTION'):
            previous['Copyright'] = copyright_text
    else:
        ROWS[component] = dict(Component=component, Origin=origin, License=license_id, Copyright=copyright_text)
    evidence = EVIDENCE.setdefault(component, {'sources': [], 'scopes': []})
    if source not in evidence['sources']:
        evidence['sources'].append(source)
    if scope not in evidence['scopes']:
        evidence['scopes'].append(scope)


def license_files(root):
    # Include nested notices: crates such as ring contain separately licensed source.
    result = []
    for f in sorted(root.rglob('*')):
        if f.is_file() and re.match(r'^(licen[cs]e|copying|copyright|notice)(?:[-_.]|$)', f.name, re.I):
            if f.stat().st_size < 1_000_000:
                result.append((str(f.relative_to(root)), f.read_text(errors='replace')))
    return result


def npm_source(name, version, resolved=None, integrity=None):
    if '@' in version:
        name, version = version.rsplit('@', 1)
    if not resolved:
        info = json.loads(download('https://registry.npmjs.org/' + urllib.parse.quote(name, safe='') + '/' + version))
        resolved = info['dist']['tarball']
        integrity = info['dist'].get('integrity')
    blob = download(resolved, integrity)
    files = []
    with tarfile.open(fileobj=io.BytesIO(blob), mode='r:gz') as archive:
        for member in archive.getmembers():
            if member.isfile() and member.size < 1_000_000 and re.match(r'^(licen[cs]e|copying|copyright|notice)(?:[-_.]|$)', Path(member.name).name, re.I):
                files.append((member.name, archive.extractfile(member).read().decode('utf-8', errors='replace')))
    return resolved, files


def collect_rust():
    metadata = json.loads(subprocess.check_output(['cargo', 'metadata', '--locked', '--offline', '--format-version', '1'], cwd=ROOT))
    for p in metadata['packages']:
        if not p['source']:
            continue
        root = Path(p['manifest_path']).parent
        notices = license_files(root)
        upstream_attempts = []
        if not notices and p.get('repository', '').startswith('https://github.com/'):
            vcs_path = root / '.cargo_vcs_info.json'
            if vcs_path.exists():
                vcs = json.loads(vcs_path.read_text())
                repo = '/'.join(p['repository'].split('/')[3:5]).removesuffix('.git')
                for filename in ['LICENSE', 'LICENSE-MIT', 'LICENSE-APACHE', 'COPYRIGHT']:
                    url = f"https://raw.githubusercontent.com/{repo}/{vcs['git']['sha1']}/{filename}"
                    try:
                        text = download(url).decode()
                        notices.append((url, text))
                        upstream_attempts.append({'url': url, 'found': True})
                    except Exception as exc:
                        upstream_attempts.append({'url': url, 'error': str(exc)})
        attribution = credits('\n'.join(t for _, t in notices))
        if not attribution:
            # Some Apache-only crates put attribution in source headers.
            attribution = credits('\n'.join(f.read_text(errors='replace')[:2500] for f in sorted(root.rglob('*.rs'))))
        retain_notices(f"cargo:{p['name']}@{p['version']}", notices)
        origin = f"https://crates.io/crates/{p['name']}/{p['version']}"
        add(f"cargo:{p['name']}@{p['version']}", origin, p['license'], attribution,
            {'manifest': 'Cargo.lock', 'package': p['name'], 'version': p['version'], 'license_files': [n for n, _ in notices], 'upstream_checks': upstream_attempts}, 'Rust: all locked targets and development dependencies')
        # Track separately attributed vendored code, not just the parent crate.
        for path, text in notices:
            if path.startswith('https://'):
                continue
            if ('/' not in path and path != 'LICENSE-BoringSSL') or not credits(text):
                continue
            add(f"bundled:cargo:{p['name']}@{p['version']}/{path}", origin,
                infer_license(text), credits(text), {'package_license_file': path}, 'Rust bundled source notices')


def collect_npm():
    lock = json.loads((ROOT / 'crates/reflex-sim/ui/package-lock.json').read_text())
    for path, p in lock['packages'].items():
        if not path:
            continue
        name = path.split('node_modules/')[-1]
        root = ROOT / 'crates/reflex-sim/ui' / path
        if root.exists():
            notices = license_files(root)
            # Never accidentally collect notices belonging to another nested package.
            notices = [(n, t) for n, t in notices if 'node_modules/' not in n]
        else:
            _, notices = npm_source(name, p['version'], p['resolved'], p.get('integrity'))
        if name.startswith('@esbuild/') and not credits('\n'.join(t for _, t in notices)):
            notices += [('esbuild@'+p['version']+'/LICENSE.md', (ROOT/'crates/reflex-sim/ui/node_modules/esbuild/LICENSE.md').read_text())]
        if name == '@datadog/druids':
            notices.append(('EULA.txt', (root/'EULA.txt').read_text()))
            notices.append(('LICENSES-3rdparty.csv', (root/'LICENSES-3rdparty.csv').read_text()))
        retain_notices(f"npm:{name}@{p['version']}", notices)
        license_id = 'LicenseRef-Datadog-EULA' if name == '@datadog/druids' else p.get('license')
        add(f"npm:{name}@{p['version']}", p['resolved'], license_id, credits('\n'.join(t for _, t in notices)),
            {'manifest': 'crates/reflex-sim/ui/package-lock.json', 'path': path, 'integrity': p.get('integrity'), 'license_files': [n for n, _ in notices]}, 'UI: locked runtime, build, and optional platform packages')
    vendor = ROOT / 'crates/reflex-sim/ui/node_modules/@datadog/druids/LICENSES-3rdparty.csv'
    vendor_rows = list(csv.DictReader(vendor.open()))
    # Keep the complete upstream inventory: it includes tooling not necessarily in the final bundle.
    def enrich(row):
        holder = row['Copyright']
        license_id = row['Licence']
        evidence = {'vendor_file': '@datadog/druids@0.1.0/LICENSES-3rdparty.csv', 'vendor_sha256': hashlib.sha256(vendor.read_bytes()).hexdigest(), 'upstream_row': row}
        version = row['Reference'].removeprefix('npm:')
        origin = 'https://www.npmjs.com/package/' + row['Component'] + '/v/' + version
        notices = []
        try:
            _, notices = npm_source(row['Component'], version)
        except Exception as exc:
            evidence['notice_lookup_error'] = str(exc)
        # Upstream author fields are not automatically copyright notices.
        if holder.startswith('(') or license_id == 'UNKNOWN':
            try:
                origin, notices = npm_source(row['Component'], version)
                holder = credits('\n'.join(t for _, t in notices)) or 'NOASSERTION — upstream inventory supplies only a project URL'
                if license_id == 'UNKNOWN':
                    license_id = next((v for _, t in notices if (v := infer_license(t)) != 'NOASSERTION'), 'NOASSERTION')
                    if license_id == 'NOASSERTION':
                        metadata_url = 'https://registry.npmjs.org/' + urllib.parse.quote(row['Component'], safe='') + '/' + version
                        metadata = json.loads(download(metadata_url))
                        declared = metadata.get('license')
                        if isinstance(declared, dict):
                            declared = declared.get('type') or declared.get('name')
                        if declared:
                            license_id = declared
                            evidence['registry_license'] = {'url': metadata_url, 'value': declared}
                        elif row['Component'] == 'text-encoding-utf-8':
                            license_id = 'LicenseRef-TextEncoding-Bundle'
                            evidence['license_note'] = 'LICENSE.md dedicates original code under Unlicense but separately identifies material derived from the WHATWG Encoding Standard; review that material.'
                evidence['license_files'] = [n for n, _ in notices]
            except Exception as exc:
                evidence['lookup_error'] = str(exc)
                holder = 'NOASSERTION — upstream attribution needs review: ' + row['Copyright']
        return row, origin, license_id, holder, evidence, version, notices
    with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
        for row, origin, license_id, holder, evidence, version, notices in pool.map(enrich, vendor_rows):
            component = f"npm:{row['Component']}@{version}"
            if component not in EVIDENCE:
                retain_notices(component, notices)
            # Vendor credits are retained verbatim, with their provenance in evidence.json.
            add(f"npm:{row['Component']}@{version}", origin, license_id, holder, evidence, 'DRUIDS upstream component inventory (conservative superset)')


def collect_python():
    requirements = ROOT / 'studies/capacity/report-requirements.txt'
    mappings = {'cycler': 'BSD-3-Clause', 'kiwisolver': 'BSD-3-Clause', 'python-dateutil': 'Apache-2.0 OR BSD-3-Clause', 'matplotlib': 'LicenseRef-Matplotlib'}
    for line in requirements.read_text().splitlines():
        if not line.strip() or line.startswith('#'):
            continue
        name, version = line.split('==')
        d = importlib.metadata.distribution(name)
        assert d.version == version, f'Install pinned research requirements: {line}'
        notices = [(str(f), d.locate_file(f).read_text(errors='replace')) for f in d.files or []
                   if re.match(r'^(licen[cs]e|copying|copyright|notice)(?:[-_.]|$)', f.name, re.I) and not str(f).endswith(('.py', '.pyc'))]
        license_id = d.metadata.get('License-Expression') or mappings.get(name.lower())
        if not license_id:
            value = d.metadata.get('License', '')
            license_id = value if re.fullmatch(r'[A-Za-z0-9. -]+', value) and len(value)<80 else 'NOASSERTION'
        retain_notices(f'pypi:{name}@{version}', notices)
        origin = f'https://pypi.org/project/{name}/{version}/'
        add(f'pypi:{name}@{version}', origin, license_id, credits('\n'.join(t for _, t in notices)),
            {'manifest': 'studies/capacity/report-requirements.txt', 'license_files': [n for n, _ in notices]}, 'Research/report tooling (not core SDK runtime)')
        for path, text in notices:
            # Pillow publishes a consolidated license split by component headings.
            if name.lower() == 'pillow':
                declared = {'AOM': 'BSD-2-Clause AND LicenseRef-AOM-Patent', 'BZIP2': 'bzip2-1.0.6',
                            'FREETYPE2': 'FTL OR GPL-2.0-only', 'HARFBUZZ': 'MIT-Modern-Variant',
                            'LIBJPEG': 'LicenseRef-libjpeg-turbo-bundle', 'LIBLZMA': 'LicenseRef-XZ-bundle',
                            'LIBPNG': 'libpng-2.0', 'LIBTIFF': 'libtiff', 'TCL_TK': 'TCL',
                            'XAU': 'MIT-open-group', 'XDMCP': 'MIT-open-group', 'ZLIB': 'Zlib'}
                for part in text.split('\n----\n')[1:]:
                    title = part.strip().splitlines()[0]
                    add(f'bundled:pypi:{name}@{version}/{title}', origin,
                        declared.get(title, infer_license(part)), ('This software is copyrighted by the Regents of the University of California, Sun Microsystems, Inc., Scriptics Corporation, and other parties.' if title == 'TCL_TK' else credits(part)),
                        {'package_license_file': path, 'section': title}, 'Research wheel bundled components (platform-specific)')
            if path.endswith('LICENSE.external') and name.lower() == 'fonttools':
                for heading, component, license_id in [
                    ('FontTools includes Adobe AGL & AGLFN', 'Adobe AGL & AGLFN', 'BSD-3-Clause'),
                    ('FontTools includes cu2qu', 'cu2qu', 'Apache-2.0'),
                    ('FontTools includes code in `fontTools.misc.filesystem`', 'PyFilesystem2-derived code', 'MIT'),
                ]:
                    part = next(part for part in text.split('=====') if heading in part)
                    add(f'bundled:pypi:{name}@{version}/{component}', origin, license_id, credits(part),
                        {'package_license_file': path, 'section': heading}, 'Research bundled source notices')
                for part in text.split('\n\n'):
                    title = part.splitlines()[0] if part else ''
                    if title in ['Lobster', 'Noto Fonts', 'XITS font project', 'Iosevka']:
                        add(f'bundled:pypi:{name}@{version}/{title}', origin, 'OFL-1.1', credits(part),
                            {'package_license_file': path, 'section': title}, 'Research fonts declared in upstream test-asset notices')
            # Wheels contain native libraries, fonts, and separately licensed embedded sources.
            sections = list(re.finditer(r'^Name: (.+)$', text, re.M))
            for i, match in enumerate(sections):
                section = text[match.start():sections[i+1].start() if i+1<len(sections) else len(text)]
                declared = re.search(r'^License:[ \t]*(.*)$', section, re.M)
                add(f'bundled:pypi:{name}@{version}/{match[1]}', origin,
                    ('LicenseRef-Yorick-Colormaps' if match[1] == 'Yorick Colormaps' else (declared[1].strip() if declared else infer_license(section))), credits(section),
                    {'package_license_file': path, 'section': match[1]}, 'Research wheel bundled components (platform-specific)')
            if not sections and len(path.split('/')) > 3 and not path.endswith('LICENSE.external'):
                add(f'bundled:pypi:{name}@{version}/{path.split("/licenses/")[-1]}', origin,
                    ('(Apache-2.0 OR BSD-3-Clause) AND CC0-1.0' if '/highway/' in path else ('LicenseRef-DejaVu-Bundle' if path.endswith('LICENSE_DEJAVU') else infer_license(text))), credits(text), {'package_license_file': path}, 'Research bundled source/font notices')
    # Retain complete notices for research wheels: multiple subcomponents can share a notice.
    # This evidence is needed to review entries that have no SPDX expression upstream.
    out = ROOT / 'third_party/research-notices'
    out.mkdir(parents=True, exist_ok=True)
    for line in requirements.read_text().splitlines():
        if '==' not in line:
            continue
        name, version = line.split('==')
        d = importlib.metadata.distribution(name)
        for f in d.files or []:
            if re.match(r'^(licen[cs]e|copying|copyright|notice)(?:[-_.]|$)', f.name, re.I) and not str(f).endswith(('.py', '.pyc')):
                target = out / name / str(f)
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(d.locate_file(f).read_bytes())


def collect_additional():
    for row in json.loads((ROOT / 'third_party/additional-components.json').read_text()):
        add(row['Component'], row['Origin'], row['License'], row['Copyright'], row['Evidence'], row['Scope'])


def check():
    import tomllib
    rows = list(csv.DictReader((ROOT / 'LICENSE-3rdparty.csv').open()))
    assert rows and list(rows[0]) == ['Component', 'Origin', 'License', 'Copyright']
    names = {r['Component'] for r in rows}
    assert len(names) == len(rows), 'Duplicate components'
    for r in rows:
        assert all(r.values()), r
    expected = {f"cargo:{p['name']}@{p['version']}" for p in tomllib.loads((ROOT/'Cargo.lock').read_text())['package'] if 'source' in p}
    lock = json.loads((ROOT/'crates/reflex-sim/ui/package-lock.json').read_text())
    expected |= {f"npm:{path.split('node_modules/')[-1]}@{p['version']}" for path,p in lock['packages'].items() if path}
    expected |= {f"pypi:{n}@{v}" for line in (ROOT/'studies/capacity/report-requirements.txt').read_text().splitlines() if '==' in line for n,v in [line.split('==')]}
    evidence = json.loads((ROOT/'third_party/evidence.json').read_text())
    for item in evidence['components'].values():
        for source in item['sources']:
            if 'upstream_row' in source:
                row = source['upstream_row']
                expected.add(f"npm:{row['Component']}@{row['Reference'].removeprefix('npm:')}")
    assert expected <= names, f'Missing components: {sorted(expected-names)}'
    evidence = json.loads((ROOT/'third_party/evidence.json').read_text())
    for path, digest in evidence['inputs'].items():
        assert hashlib.sha256((ROOT/path).read_bytes()).hexdigest() == digest, f'Inventory stale: {path}'
    assert names == set(evidence['components'])
    for component, item in evidence['components'].items():
        for notice in item.get('notices', []):
            path = ROOT / notice['file']
            assert path.is_file(), f'Missing notice: {component}: {path}'
            assert hashlib.sha256(path.read_bytes()).hexdigest() == notice['sha256'], f'Changed notice: {path}'
    expected_review = {r['Component'] for r in rows if r['License'] == 'NOASSERTION' or r['Copyright'].startswith('NOASSERTION') or 'LicenseRef-' in r['License'] or 'GPL' in r['License']}
    actual_review = {r['Component'] for r in csv.DictReader((ROOT/'third_party/review-required.csv').open())}
    assert actual_review == expected_review, 'Review queue is stale'

    print(f'Validated {len(rows)} unique rows; all {len(expected)} locked and vendor-listed package/version entries covered.')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--check', action='store_true', help='Check coverage without network, Cargo, or installed dependencies (Python 3.11+)')
    args = parser.parse_args()
    if args.check:
        check()
        return
    for label, fn in [('Rust',collect_rust),('npm and DRUIDS',collect_npm),('Python',collect_python),('additional assets',collect_additional)]:
        print('Collecting '+label, flush=True)
        fn()
    with (ROOT/'LICENSE-3rdparty.csv').open('w', newline='') as f:
        writer = csv.DictWriter(f, fieldnames=['Component','Origin','License','Copyright'], lineterminator='\n')
        writer.writeheader()
        writer.writerows(ROWS[k] for k in sorted(ROWS))
    inputs = ['Cargo.lock', 'crates/reflex-sim/ui/package-lock.json', 'studies/capacity/report-requirements.txt', 'third_party/additional-components.json']
    report = {'python_environment': {'platform': sys.platform, 'python': sys.version.split()[0]}, 'inputs': {p:hashlib.sha256((ROOT/p).read_bytes()).hexdigest() for p in inputs}, 'components': {k:EVIDENCE[k] for k in sorted(EVIDENCE)}}
    (ROOT/'third_party/evidence.json').write_text(json.dumps(report,indent=2)+'\n')
    unresolved = [r for r in ROWS.values() if r['License']=='NOASSERTION' or r['Copyright'].startswith('NOASSERTION') or 'LicenseRef-' in r['License'] or 'GPL' in r['License']]
    with (ROOT/'third_party/review-required.csv').open('w',newline='') as f:
        writer=csv.DictWriter(f,fieldnames=['Component','Origin','License','Copyright'],lineterminator='\n');writer.writeheader();writer.writerows(sorted(unresolved,key=lambda r:r['Component']))
    check()
    print(f'{len(unresolved)} rows require license or attribution review; see third_party/review-required.csv.')

if __name__=='__main__':
    main()
