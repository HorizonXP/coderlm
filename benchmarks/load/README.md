# Load Harness

This directory defines the stable scenario contract for future load benchmark
work. The current runner validates JSON scenario files, applies defaults and
command-line overrides, polls a CodeRLM server, creates configured fixture
sessions, waits for readiness, executes the configured core API smoke workflow,
and writes a report.

## Layout

- `runner/` - `python -m benchmarks.load.runner` validation and smoke entry
  point.
- `scenarios/` - checked-in JSON scenario definitions.
- `fixtures/` - deterministic fixture repositories and `metadata.json`.
- `reports/` - generated report root. A run should write
  `benchmarks/load/reports/<run-id>/report.json`; generated reports are ignored
  by default.
- `compose.yaml` - Docker Compose services for constrained CPU smoke runs.
- `Dockerfile` - server and load-runner image targets built from this checkout.

## Validation

```bash
python -m benchmarks.load.runner --validate-only benchmarks/load/scenarios/starter.json
```

The command exits zero and prints normalized JSON for a valid scenario. It exits
non-zero with `error:` lines for malformed files, missing fields, unknown
scenario IDs, unsupported workloads or operations, unknown fixture identifiers,
invalid CPU profile labels, invalid reliability thresholds, or contradictory
command-line overrides.

## Compose Smoke Runs

From a clean checkout with Docker Compose available:

```bash
docker compose -f benchmarks/load/compose.yaml --profile cpu-2 up \
  --build --abort-on-container-exit --exit-code-from load-runner-2cpu

docker compose -f benchmarks/load/compose.yaml --profile cpu-4 up \
  --build --abort-on-container-exit --exit-code-from load-runner-4cpu
```

The `cpu-2` profile runs `compose-2cpu-smoke.json` and records normalized
`2cpu` profile metadata in `/reports/compose-2cpu-smoke/report.json` inside the
runner container, mapped to
`benchmarks/load/reports/compose-2cpu-smoke/report.json` on the host. The
`cpu-4` profile does the same for `4cpu`.

Compose `cpus` limits are supported by current Docker Compose engines, but they
can be advisory or implemented differently across host operating systems and
container runtimes. Reports include the selected profile and raw environment
values such as `CODERLM_CPU_PROFILE_LABEL`, `COMPOSE_PROFILES`, and `HOSTNAME`
so cross-host behavior remains visible instead of hidden.

To exercise the health timeout failure path without starting a server:

```bash
python -m benchmarks.load.runner benchmarks/load/scenarios/compose-health-timeout.json
```

The command exits non-zero with an `error:` line when `/api/v1/health` is
unreachable before `readiness_timeout_seconds`.

## Scenario Contract

Every normalized scenario includes these workload knobs:

- `scenario_id`
- `name`
- `workload_id`
- `agent_count`
- `project_count`
- `max_concurrency`
- `duration_seconds`
- `request_pacing.mode`
- `request_pacing.requests_per_second`
- `fixture_id`
- `scenario_mix`
- `server_options.host`
- `server_options.bind`
- `server_options.port`
- `server_options.max_file_size`
- `server_options.max_projects`
- `server_options.watcher_enabled`
- `server_options.log_level`
- `server_options.reuse_existing`
- `cpu_profile_label`
- `cpu_profile`
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
