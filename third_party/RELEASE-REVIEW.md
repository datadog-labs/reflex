# Third-party release review

Owner: Datadog, Inc. Maintenance and dependency reviews: Systems Research
(`@datadog-labs/systems-research`).

## Release scope

The artifact reviewed here is the repository source tree, including the SDK,
simulation sources, UI sources, and research scripts. All five Rust packages
currently have `publish = false`. This review does not approve a compiled
simulator, UI bundle, container image, Python environment, or crates.io package.

The source tree references dependencies through its lockfiles. Installed
`node_modules`, Cargo's `target` directory, research environments, credentials,
and generated UI bundles are excluded from Git. The upstream notices retained
in this directory are evidence and attribution material.

A binary release needs a separate inventory of what that specific build embeds,
including platform-specific native libraries and fonts. Research wheel notices
in this inventory describe the inspected macOS environment; they cannot establish
what another platform's wheels contain.

## Evidence prepared

- Root Apache-2.0 `LICENSE`, Datadog `NOTICE`, and workspace license metadata.
- `LICENSE-3rdparty.csv`: exact package versions, origins, declared licenses,
  and attributed copyright notices, including vendor-listed and bundled code.
- `evidence.json`: lockfile hashes, package provenance, and retained notice hashes.
- `notices/` and `research-notices/`: upstream texts for review and attribution.
- `review-required.csv`: missing attribution, custom licenses, and copyleft
  expressions, including packages offering alternative licenses.
- `scripts/update_third_party.py --check`: verifies inventory coverage, input
  freshness, notice hashes, and the review queue without accessing the network.

## Decisions still required

| Item | Evidence and action needed |
| --- | --- |
| DRUIDS 0.1.0 | Its published package supplies a Datadog EULA, not an open-source license. Confirm the permitted public use and redistribution of this UI dependency and its bundled assets, or replace it. Root Apache-2.0 does not relicense it. |
| Missing attribution | Review each `NOASSERTION` copyright entry. An omitted notice is not evidence that a package is unlicensed. Package authors have not been substituted for copyright holders. Some public-domain components have no notice by design. |
| Custom license texts | Review Matplotlib, BaKoMa, Yorick, DejaVu/Arev, TextEncoding-derived material, and the Pillow consolidated LIBJPEG/LIBLZMA/AOM terms. `LicenseRef` labels identify texts; they are not approval. |
| Research native libraries | NumPy's inspected notice includes libquadmath under LGPL-2.1-or-later and the GCC runtime under GPL-3.0-or-later WITH GCC-exception-3.1. Determine obligations for any future redistributed research environment or native binary. |
| Alternative licenses | Record the selected alternative for each relevant distribution (for example, the FreeType license versus GPL), and include its required notices. Do not treat an `OR` expression as requiring every alternative. |
| Fonts | Noto Sans embedded in DRUIDS declares OFL-1.1; the inspected Roboto Mono copy declares Apache-2.0. Keep the exact-copy evidence and applicable texts when distributing embedded fonts. |
| Approval | Record the review decision for this revision and release scope. This inventory does not substitute for release approval or security checks. |

## Investigation notes

The collector examines published license/notice files, preserved vendor records,
and Rust source headers. For Rust packages without packaged license files, it
also checks root notices at the immutable Git commit recorded by Cargo. The
attempts and results are recorded in `evidence.json`. Missing or inaccessible
upstream files remain explicit; an attribution is never inferred from a
repository's owner or an author list.

The Yorick and DejaVu texts are retained under custom license identifiers instead
of assigning a superficially similar standard license. Tcl/Tk's attribution is
copied from its prose notice. npm aliases are resolved to the actual package
and version before collecting evidence. The review check includes custom terms
inside compound expressions, such as AOM's patent grant.

`review-required.csv` is a review queue, not a claim that every unflagged row has
received a legal compatibility review. Retained upstream texts remain the source
of truth; abbreviated CSV attribution is an index into those texts.

## Maintenance

For a dependency update, regenerate and review the inventory before merging.
Resolve added custom-license, attribution, and notice gaps with the release
reviewer. Recheck the actual distributed artifact whenever packaging or target
platforms change. Systems Research owns this process; see `MAINTAINERS.md`.
