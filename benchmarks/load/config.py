"""Scenario configuration loading and validation for the load harness."""

from __future__ import annotations

import copy
import json
import os
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from benchmarks.load.fixtures_support import FixtureError, resolve_fixture


HARNESS_ROOT = Path(__file__).resolve().parent
SCENARIOS_DIR = HARNESS_ROOT / "scenarios"
FIXTURES_DIR = HARNESS_ROOT / "fixtures"
REPORTS_DIR = HARNESS_ROOT / "reports"

SUPPORTED_SCENARIO_IDS = {
    "SCENARIO-01",
    "SCENARIO-02",
    "SCENARIO-03",
    "SCENARIO-04",
    "SCENARIO-05",
    "SCENARIO-06",
    "SCENARIO-07",
    "SCENARIO-08",
    "SCENARIO-09",
    "SCENARIO-10",
}
SUPPORTED_WORKLOADS = {
    "symbol_lookup_smoke",
    "mixed_read_smoke",
    "fixture_metadata_smoke",
    "watcher_mutation_prep",
    "core_api_smoke",
}
SUPPORTED_OPERATIONS = {
    "structure",
    "grep",
    "search_symbols",
    "read_implementation",
    "list_callers",
    "list_tests",
    "peek_file",
    "touch_file",
    "append_file",
    "replace_file",
}
CPU_PROFILES: dict[str, dict[str, Any]] = {
    "none": {
        "compose_profile": "default",
        "cpu_limit": None,
        "description": "No Compose CPU limit requested.",
    },
    "wall-clock": {
        "compose_profile": "default",
        "cpu_limit": None,
        "description": "Legacy wall-clock profiling label without a CPU limit.",
    },
    "flamegraph": {
        "compose_profile": "default",
        "cpu_limit": None,
        "description": "Legacy flamegraph profiling label without a CPU limit.",
    },
    "2cpu": {
        "compose_profile": "cpu-2",
        "cpu_limit": 2.0,
        "description": "Compose CPU-constrained run targeting two CPUs.",
    },
    "4cpu": {
        "compose_profile": "cpu-4",
        "cpu_limit": 4.0,
        "description": "Compose CPU-constrained run targeting four CPUs.",
    },
}
SUPPORTED_CPU_PROFILES = set(CPU_PROFILES)
SUPPORTED_PACING_MODES = {"fixed_rps"}

DEFAULTS: dict[str, Any] = {
    "agent_count": 1,
    "project_count": 1,
    "duration_seconds": 60,
    "request_pacing": {
        "mode": "fixed_rps",
        "requests_per_second": 1.0,
    },
    "server_options": {
        "host": "127.0.0.1",
        "bind": "127.0.0.1",
        "port": 3000,
        "max_file_size": 1_000_000,
        "max_projects": 5,
        "watcher_enabled": True,
        "log_level": "info",
        "reuse_existing": False,
    },
    "cpu_profile_label": "none",
    "output_dir": str(REPORTS_DIR / "local-dry-run"),
    "readiness_timeout_seconds": 30,
    "reliability_threshold": 0.99,
}

REQUIRED_FIELDS = {
    "scenario_id",
    "name",
    "workload_id",
    "fixture_id",
    "scenario_mix",
}


class ConfigValidationError(ValueError):
    """Raised when a scenario config cannot be normalized safely."""

    def __init__(self, errors: list[str]) -> None:
        self.errors = errors
        super().__init__("\n".join(errors))


@dataclass(frozen=True)
class ScenarioOverrides:
    agent_count: int | None = None
    duration_seconds: int | None = None
    output_dir: str | None = None
    cpu_profile_label: str | None = None
    readiness_timeout_seconds: int | None = None
    reliability_threshold: float | None = None
    requests_per_second: float | None = None
    server_host: str | None = None
    server_port: int | None = None

    def as_updates(self) -> dict[str, Any]:
        updates: dict[str, Any] = {}
        for field in (
            "agent_count",
            "duration_seconds",
            "output_dir",
            "cpu_profile_label",
            "readiness_timeout_seconds",
            "reliability_threshold",
        ):
            value = getattr(self, field)
            if value is not None:
                updates[field] = value
        if self.requests_per_second is not None:
            updates.setdefault("request_pacing", {})[
                "requests_per_second"
            ] = self.requests_per_second
        if self.server_host is not None:
            updates.setdefault("server_options", {})["host"] = self.server_host
        if self.server_port is not None:
            updates.setdefault("server_options", {})["port"] = self.server_port
        return updates


def load_scenario(path: Path, overrides: ScenarioOverrides | None = None) -> dict[str, Any]:
    """Load a JSON scenario file and return normalized metadata."""

    try:
        raw = json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError as exc:
        raise ConfigValidationError([f"scenario file does not exist: {path}"]) from exc
    except json.JSONDecodeError as exc:
        raise ConfigValidationError([f"scenario file is not valid JSON: {exc}"]) from exc

    if not isinstance(raw, dict):
        raise ConfigValidationError(["scenario file must contain a JSON object"])

    normalized = _merge_defaults(raw)
    if overrides is not None:
        _deep_update(normalized, overrides.as_updates())

    errors = _validate(normalized)
    if errors:
        raise ConfigValidationError(errors)

    normalized["fixture"] = resolve_fixture(normalized["fixture_id"]).as_dict()
    normalized["report_path"] = str(Path(normalized["output_dir"]) / "report.json")
    normalized["cpu_profile"] = _cpu_profile_metadata(normalized["cpu_profile_label"])
    return normalized


def _merge_defaults(raw: dict[str, Any]) -> dict[str, Any]:
    normalized = copy.deepcopy(DEFAULTS)
    _deep_update(normalized, raw)
    return normalized


def _deep_update(target: dict[str, Any], updates: dict[str, Any]) -> None:
    for key, value in updates.items():
        if isinstance(value, dict) and isinstance(target.get(key), dict):
            _deep_update(target[key], value)
        else:
            target[key] = value


def _validate(config: dict[str, Any]) -> list[str]:
    errors: list[str] = []

    missing = sorted(field for field in REQUIRED_FIELDS if field not in config)
    errors.extend(f"missing required field: {field}" for field in missing)

    _require_string(config, "scenario_id", errors)
    _require_string(config, "name", errors)
    _require_string(config, "workload_id", errors)
    _require_string(config, "fixture_id", errors)
    _require_positive_int(config, "agent_count", errors)
    _require_positive_int(config, "project_count", errors)
    _require_positive_int(config, "duration_seconds", errors)
    _require_positive_int(config, "readiness_timeout_seconds", errors)
    _require_unit_interval(config, "reliability_threshold", errors)
    _require_string(config, "cpu_profile_label", errors)
    _require_string(config, "output_dir", errors)

    scenario_id = config.get("scenario_id")
    if isinstance(scenario_id, str) and scenario_id not in SUPPORTED_SCENARIO_IDS:
        errors.append(
            "unknown scenario_id: "
            f"{scenario_id}; expected one of {sorted(SUPPORTED_SCENARIO_IDS)}"
        )

    workload_id = config.get("workload_id")
    if isinstance(workload_id, str) and workload_id not in SUPPORTED_WORKLOADS:
        errors.append(
            "unknown workload_id: "
            f"{workload_id}; expected one of {sorted(SUPPORTED_WORKLOADS)}"
        )

    cpu_profile_label = config.get("cpu_profile_label")
    if (
        isinstance(cpu_profile_label, str)
        and cpu_profile_label not in SUPPORTED_CPU_PROFILES
    ):
        errors.append(
            "invalid cpu_profile_label: "
            f"{cpu_profile_label}; expected one of {sorted(SUPPORTED_CPU_PROFILES)}"
        )

    fixture_id = config.get("fixture_id")
    if isinstance(fixture_id, str):
        try:
            resolve_fixture(fixture_id)
        except FixtureError as exc:
            errors.append(str(exc))

    _validate_pacing(config.get("request_pacing"), errors)
    _validate_server_options(config.get("server_options"), errors)
    _validate_scenario_mix(config.get("scenario_mix"), errors)

    return errors


def _require_string(config: dict[str, Any], field: str, errors: list[str]) -> None:
    value = config.get(field)
    if not isinstance(value, str) or not value.strip():
        errors.append(f"{field} must be a non-empty string")


def _require_positive_int(config: dict[str, Any], field: str, errors: list[str]) -> None:
    value = config.get(field)
    if not isinstance(value, int) or isinstance(value, bool) or value <= 0:
        errors.append(f"{field} must be a positive integer")


def _require_unit_interval(config: dict[str, Any], field: str, errors: list[str]) -> None:
    value = config.get(field)
    if not isinstance(value, (int, float)) or isinstance(value, bool) or not 0 < value <= 1:
        errors.append(f"{field} must be greater than 0 and less than or equal to 1")


def _validate_pacing(value: Any, errors: list[str]) -> None:
    if not isinstance(value, dict):
        errors.append("request_pacing must be an object")
        return

    mode = value.get("mode")
    if mode not in SUPPORTED_PACING_MODES:
        errors.append(
            f"request_pacing.mode must be one of {sorted(SUPPORTED_PACING_MODES)}"
        )
    requests_per_second = value.get("requests_per_second")
    if (
        not isinstance(requests_per_second, (int, float))
        or isinstance(requests_per_second, bool)
        or requests_per_second <= 0
    ):
        errors.append("request_pacing.requests_per_second must be a positive number")


def _validate_server_options(value: Any, errors: list[str]) -> None:
    if not isinstance(value, dict):
        errors.append("server_options must be an object")
        return

    host = value.get("host")
    bind = value.get("bind")
    port = value.get("port")
    max_file_size = value.get("max_file_size")
    max_projects = value.get("max_projects")
    watcher_enabled = value.get("watcher_enabled")
    log_level = value.get("log_level")
    reuse_existing = value.get("reuse_existing")
    if not isinstance(host, str) or not host.strip():
        errors.append("server_options.host must be a non-empty string")
    if not isinstance(bind, str) or not bind.strip():
        errors.append("server_options.bind must be a non-empty string")
    if not isinstance(port, int) or isinstance(port, bool) or not 1 <= port <= 65535:
        errors.append("server_options.port must be an integer between 1 and 65535")
    if (
        not isinstance(max_file_size, int)
        or isinstance(max_file_size, bool)
        or max_file_size <= 0
    ):
        errors.append("server_options.max_file_size must be a positive integer")
    if (
        not isinstance(max_projects, int)
        or isinstance(max_projects, bool)
        or max_projects <= 0
    ):
        errors.append("server_options.max_projects must be a positive integer")
    if not isinstance(watcher_enabled, bool):
        errors.append("server_options.watcher_enabled must be a boolean")
    if not isinstance(log_level, str) or not log_level.strip():
        errors.append("server_options.log_level must be a non-empty string")
    if not isinstance(reuse_existing, bool):
        errors.append("server_options.reuse_existing must be a boolean")


def _cpu_profile_metadata(label: str) -> dict[str, Any]:
    profile = dict(CPU_PROFILES[label])
    profile["label"] = label
    profile["raw_environment"] = {
        "CODERLM_CPU_PROFILE_LABEL": os.environ.get("CODERLM_CPU_PROFILE_LABEL"),
        "COMPOSE_PROFILES": os.environ.get("COMPOSE_PROFILES"),
    }
    return profile


def _validate_scenario_mix(value: Any, errors: list[str]) -> None:
    if not isinstance(value, list) or not value:
        errors.append("scenario_mix must be a non-empty list")
        return

    weight_total = 0
    for index, entry in enumerate(value):
        prefix = f"scenario_mix[{index}]"
        if not isinstance(entry, dict):
            errors.append(f"{prefix} must be an object")
            continue

        operation = entry.get("operation")
        if operation not in SUPPORTED_OPERATIONS:
            errors.append(
                f"{prefix}.operation is unknown: {operation}; "
                f"expected one of {sorted(SUPPORTED_OPERATIONS)}"
            )

        weight = entry.get("weight")
        if not isinstance(weight, int) or isinstance(weight, bool) or weight <= 0:
            errors.append(f"{prefix}.weight must be a positive integer")
        else:
            weight_total += weight

    if weight_total <= 0:
        errors.append("scenario_mix must include at least one positive weight")
