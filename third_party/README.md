# Third-party component inventory

[`LICENSE-3rdparty.csv`](../LICENSE-3rdparty.csv) uses the release-policy columns:
`Component,Origin,License,Copyright`. Component identifiers include an ecosystem
and exact package version. Embedded assets use the enclosing package version,
source path, or font metadata to identify the copy being used.

## Coverage

The inventory covers:

- All 212 third-party versions in `Cargo.lock`, including development and
  platform-specific dependencies, plus separately noticed bundled Rust sources.
- All 43 package versions in the UI's `package-lock.json`, including optional
  esbuild binaries for other platforms.
- All 489 rows in DRUIDS 0.1.0's upstream `LICENSES-3rdparty.csv`. Overlapping
  package/version entries are deduplicated. This is a conservative superset:
  upstream build and test tools are included even when they may not reach our UI.
- Noto Sans and Roboto Mono font assets embedded in DRUIDS styles. Their embedded
  name tables supply copyright, version, and license URLs; font hashes are saved
  in `additional-components.json`. This particular Roboto Mono copy declares
  Apache-2.0; Noto Sans declares the SIL Open Font License.
- The 12 pinned research/report packages in `report-requirements.txt`, plus
  bundled components identified in their installed license notices, including
  native libraries, fonts, and test assets. Wheel-specific notices were collected
  on macOS; other platforms and wheel builds can include different components.

The core Rust library does not depend on DRUIDS or the research Python packages.
System runtimes/toolchains, remote Jev/Toto/Datadog services, and unrelated packages
installed in the research environment are not repository dependencies in this list.
This is a dependency-and-notice inventory, not a binary composition scan.

## Sources and review

`evidence.json` records provenance and input hashes. `notices/` retains package
license texts by SHA-256, with their original paths recorded in that evidence.
The checker verifies each retained text against its hash. Cargo and npm rows use
package-declared license expressions; DRUIDS vendor credits are preserved as
supplied by its inventory. Where the vendor supplied only a URL, the generator
attempts to obtain attribution from the exact published package. A package's
author is not assumed to be its copyright holder.

`research-notices/` retains the original research package notices, including
consolidated notices for bundled native libraries. Nested component versions
that upstream does not specify are identified by their enclosing package version;
we do not invent a native-library version.

`review-required.csv` lists missing attribution, unresolved license expressions,
custom license references, and copyleft expressions (including alternatives). `NOASSERTION` means the inspected upstream files
did not establish the field. It does not mean a component is unlicensed or that
it is approved for release. Public-domain code may legitimately lack a copyright
notice. Nonempty vendor credit fields are also upstream assertions, not a legal
review of ownership.

Custom license references identify these upstream texts or combinations:

| Reference | Source / issue |
| --- | --- |
| `LicenseRef-Datadog-EULA` | DRUIDS `EULA.txt`; obtain release guidance for the UI dependency. |
| `LicenseRef-Yorick-Colormaps` | Gist/Yorick section of Matplotlib’s retained `LICENSE`. |
| `LicenseRef-DejaVu-Bundle` | Consolidated Bitstream Vera, Arev, and DejaVu terms in Matplotlib’s `LICENSE_DEJAVU`. |
| `LicenseRef-Matplotlib` | Matplotlib's own license agreement in its retained `LICENSE`. |
| `LicenseRef-BaKoMa` | BaKoMa fonts section of Matplotlib's retained `LICENSE`. |
| `LicenseRef-AOM-Patent` | AOM patent grant accompanying BSD-2-Clause in Pillow's retained `LICENSE`. |
| `LicenseRef-libjpeg-turbo-bundle` | Multiple component terms in Pillow's LIBJPEG section. |
| `LicenseRef-XZ-bundle` | Multiple component terms in Pillow's LIBLZMA section. |
| `LicenseRef-TextEncoding-Bundle` | `text-encoding-utf-8@1.0.2/LICENSE.md`: original code is under Unlicense; it also identifies material derived from the WHATWG Encoding Standard. Review the derived material's terms. |

Yorick colormaps and the consolidated DejaVu font notice are identified as
`LicenseRef-Yorick-Colormaps` and `LicenseRef-DejaVu-Bundle`. These labels point
to the retained upstream texts; they do not establish compatibility or approval. The previously unknown DRUIDS entry `odiff@1.4.2` declares
MIT in its exact-version registry metadata; its attribution is still unresolved.

This inventory does **not** close license-compatibility review or the package
redistribution checklist. Reviewers must resolve the flagged entries, select any
applicable alternative licenses, assess bundled notices, and assign dependency
maintenance owners before release. Do not distribute a binary or wheel on the
assumption that this CSV alone supplies every required license text.

## Verify and update

Coverage validation needs only Python 3.11+ and does not access the network:

```sh
python3 scripts/update_third_party.py --check
```

It checks every locked package version, the preserved DRUIDS inventory rows,
unique/nonempty CSV fields, provenance coverage, and input hashes. It detects
manifest/lock changes; it does not certify copyright completeness or compatibility.

To regenerate, first install the UI and pinned research dependencies and fetch
Cargo's locked dependencies:

```sh
npm ci --prefix crates/reflex-sim/ui
cargo fetch --locked
python3 -m venv output/license-inventory-venv
output/license-inventory-venv/bin/python -m pip install -r studies/capacity/report-requirements.txt
output/license-inventory-venv/bin/python scripts/update_third_party.py
```

Regeneration downloads exact-version npm tarballs for missing platform packages
and incomplete vendor attributions, verifies npm integrity where supplied, and
caches downloads under ignored `output/third-party-cache/`. No package install
scripts from those tarballs are executed. Record changes to research wheel
notices when regenerating on another platform.

When updating DRUIDS, re-examine its embedded font metadata and update
`additional-components.json`; those asset records are reviewed inputs rather
than automatically inferred licenses. Use fontTools with Brotli support to read
WOFF2 name tables. Retain the original license declarations even if a newer font
release uses a different license.

## Release review

See [release scope and open decisions](RELEASE-REVIEW.md). The
[Systems Research team](../MAINTAINERS.md) owns updates and follow-up.
