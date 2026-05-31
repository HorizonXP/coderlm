# Load Harness

This directory defines the stable scenario contract for future load benchmark
work. The current runner validates JSON scenario files, applies defaults and
command-line overrides, and prints normalized metadata without starting Docker,
CodeRLM, or any workload process.

## Layout

- `runner/` - `python -m benchmarks.load.runner` validation entry point.
- `scenarios/` - checked-in JSON scenario definitions.
- `fixtures/` - deterministic fixture repositories and `metadata.json`.
- `reports/` - generated report root. A run should write
  `benchmarks/load/reports/<run-id>/report.json`; generated reports are ignored
  by default.

## Validation

```bash
python -m benchmarks.load.runner --validate-only benchmarks/load/scenarios/starter.json
```

The command exits zero and prints normalized JSON for a valid scenario. It exits
non-zero with `error:` lines for malformed files, missing fields, unknown
scenario IDs, unsupported workloads or operations, unknown fixture identifiers,
invalid CPU profile labels, invalid reliability thresholds, or contradictory
command-line overrides.

## Scenario Contract

Every normalized scenario includes these workload knobs:

- `scenario_id`
- `name`
- `workload_id`
- `agent_count`
- `project_count`
- `duration_seconds`
- `request_pacing.mode`
- `request_pacing.requests_per_second`
- `fixture_id`
- `scenario_mix`
- `server_options.host`
- `server_options.port`
- `server_options.reuse_existing`
- `cpu_profile_label`
- `output_dir`
- `readiness_timeout_seconds`
- `reliability_threshold`
- `report_path`

The workload extension point is `workload_id`. Add new workload IDs to
`SUPPORTED_WORKLOADS` and implement the workload behind that ID in a later
runner step; scenario file field names do not need to change.

## Fixture Contract

Fixture IDs are resolved through `benchmarks/load/fixtures/metadata.json`.
Current stable IDs:

- `starter-project` - small Python fixture with known symbols, callers, tests,
  grep target, and watcher mutation files.
- `mixed-language-project` - larger deterministic mixed-language fixture
  covering Rust, Python, TypeScript, JavaScript, and Go.

Use `prepare_fixture_working_copy()` from `benchmarks.load.fixtures_support`
before watcher churn workloads. Mutation helpers accept the prepared working
copy object and mutate only that run-specific copy. Checked-in fixture sources
are validated as immutable inputs and should remain byte-for-byte comparable
between runs. Generated-heavy directories such as `target`, `node_modules`,
`__pycache__`, and `.coderlm` are documented as excluded fixture copy inputs.
