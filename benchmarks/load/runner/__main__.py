"""Validate and normalize load harness scenario files."""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

from benchmarks.load.config import ConfigValidationError, ScenarioOverrides, load_scenario


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="python -m benchmarks.load.runner",
        description="Validate a load harness scenario and print normalized metadata.",
    )
    parser.add_argument(
        "scenario",
        type=Path,
        help="Path to a JSON scenario config, such as benchmarks/load/scenarios/starter.json.",
    )
    parser.add_argument(
        "--validate-only",
        action="store_true",
        help="Validate and normalize config without starting any load workload.",
    )
    parser.add_argument("--agents", type=int, dest="agent_count")
    parser.add_argument("--duration-seconds", type=int)
    parser.add_argument("--output-dir")
    parser.add_argument("--cpu-profile-label")
    parser.add_argument("--readiness-timeout-seconds", type=int)
    parser.add_argument("--reliability-threshold", type=float)
    parser.add_argument("--request-rate", type=float, dest="requests_per_second")
    return parser


def main(argv: list[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    overrides = ScenarioOverrides(
        agent_count=args.agent_count,
        duration_seconds=args.duration_seconds,
        output_dir=args.output_dir,
        cpu_profile_label=args.cpu_profile_label,
        readiness_timeout_seconds=args.readiness_timeout_seconds,
        reliability_threshold=args.reliability_threshold,
        requests_per_second=args.requests_per_second,
    )

    try:
        normalized = load_scenario(args.scenario, overrides)
    except ConfigValidationError as exc:
        for error in exc.errors:
            print(f"error: {error}", file=sys.stderr)
        return 2

    print(json.dumps(normalized, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
