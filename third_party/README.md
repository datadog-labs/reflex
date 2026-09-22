# Third-party inventory

[`LICENSE-3rdparty.csv`](../LICENSE-3rdparty.csv) lists dependency versions,
origins, declared licenses, and copyright notices for Rust, the simulation UI,
and research tooling. It includes transitive dependencies, DRUIDS vendor-listed
components, and identified bundled libraries and fonts. Vendor records may
include build/test dependencies that do not reach a distributed artifact.

`NOASSERTION` means the inspected sources did not establish a field.
`LicenseRef-*` identifies custom upstream terms that need review. Neither is an
approval or a conclusion that a component is unlicensed. The inventory does not
establish license compatibility; review it for the intended release scope.

## Validate and update

Run the offline coverage check with Python 3.11 or newer:

```sh
python3 scripts/update_third_party.py --check
```

The check validates CSV structure and coverage of locked Rust, npm and Python
packages and the manually recorded assets in `additional-components.json`.
It does not certify bundled/vendor inventory completeness or license compliance.

To regenerate, install the UI and pinned research dependencies and fetch Cargo's
locked packages:

```sh
npm ci --prefix crates/reflex-sim/ui
cargo fetch --locked
python3 -m venv output/license-inventory-venv
output/license-inventory-venv/bin/python -m pip install -r studies/capacity/report-requirements.txt
output/license-inventory-venv/bin/python scripts/update_third_party.py
```

Commit the updated CSV and any changed generator inputs. Collected notices,
provenance, and the generated review queue go to ignored
`output/third-party-review/`; downloads are cached in `output/third-party-cache/`.
Share that evidence with the release reviewer separately. When updating DRUIDS,
recheck the embedded fonts and update `additional-components.json`.

Research-wheel notices depend on the platform and build inspected. A source
release review does not approve a compiled application, UI bundle, container,
or Python environment. Any distributed artifact must include the license texts
and notices its dependencies require. Preserve notices for third-party code
actually included in the repository.

[Systems Research](../MAINTAINERS.md) owns dependency updates and review.
