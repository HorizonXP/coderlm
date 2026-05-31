# Load Harness

This directory defines the stable scenario contract for future load benchmark
work. The current runner validates JSON scenario files, applies defaults and
command-line overrides, and prints normalized metadata without starting Docker,
CodeRLM, or any workload process.

## Layout

- `runner/` - `python -m benchmarks.load.runner` validation entry point.
- `scenarios/` - checked-in JSON scenario definitions.
- `fixtures/` - fixture identifiers referenced by scenario configs.
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
