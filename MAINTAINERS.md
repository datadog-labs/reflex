# Ownership and maintenance

Reflex is owned by **Datadog, Inc.**

The [Systems Research team](https://github.com/orgs/datadog-labs/teams/systems-research)
(`@datadog-labs/systems-research`) owns ongoing maintenance, dependency updates,
and code reviews. The team is assigned across the repository in
[CODEOWNERS](.github/CODEOWNERS).

Dependency changes must update `LICENSE-3rdparty.csv`. Collected review evidence
is generated locally under ignored `output/third-party-review/`.
Run `python3 scripts/update_third_party.py --check` before submitting a change;
see [the inventory instructions](third_party/README.md) for regeneration and review.
