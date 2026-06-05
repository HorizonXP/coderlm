"""Start the CodeRLM server from a normalized load scenario."""

from __future__ import annotations

import argparse
import os
import subprocess
import sys
from pathlib import Path

from benchmarks.load.config import ConfigValidationError, load_scenario


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="python -m benchmarks.load.compose_server",
        description="Start coderlm-server using server_options from a scenario file.",
    )
    parser.add_argument("scenario", type=Path)
    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    try:
        scenario = load_scenario(args.scenario)
    except ConfigValidationError as exc:
        for message in exc.errors:
            print(f"error: {message}", file=sys.stderr)
        return 2

    server_options = scenario["server_options"]
    env = os.environ.copy()
    env["RUST_LOG"] = server_options["log_level"]
    if server_options["watcher_enabled"]:
        env.pop("CODERLM_DISABLE_WATCHER", None)
    else:
        env["CODERLM_DISABLE_WATCHER"] = "1"

    binary = env.get("CODERLM_SERVER_BIN", "/usr/local/bin/coderlm-server")
    project_path = env.get(
        "CODERLM_SERVER_PROJECT_PATH",
        scenario["fixture"]["source_path"],
    )
    command = [
        binary,
        "serve",
        project_path,
        "--bind",
        server_options["bind"],
        "--port",
        str(server_options["port"]),
        "--max-file-size",
        str(server_options["max_file_size"]),
        "--max-projects",
        str(server_options["max_projects"]),
    ]
    print("starting coderlm-server:", " ".join(command), flush=True)
    try:
        completed = subprocess.run(command, env=env, check=False)
    except FileNotFoundError:
        print(f"error: server binary does not exist: {binary}", file=sys.stderr)
        return 127
    return completed.returncode


if __name__ == "__main__":
    raise SystemExit(main())
