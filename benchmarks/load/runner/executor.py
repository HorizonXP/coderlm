"""Session lifecycle and core API operation executor for load scenarios."""

from __future__ import annotations

import os
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from benchmarks.load.fixtures_support import PreparedFixture, prepare_fixture_working_copy
from benchmarks.load.runner.client import CodeRLMClient, CodeRLMSession
from benchmarks.load.runner.events import (
    OperationContext,
    UnsupportedOperationTarget,
    begin_operation,
    error_event,
    success_event,
)


SYMBOL_DEPENDENT_OPERATIONS = {
    "search_symbols",
    "read_implementation",
    "list_callers",
    "list_tests",
}


@dataclass(frozen=True)
class AgentSession:
    agent_id: str
    project_id: str
    prepared_fixture: PreparedFixture
    session: CodeRLMSession
    readiness: dict[str, Any]


def create_agent_sessions(
    client: CodeRLMClient,
    config: dict[str, Any],
) -> list[AgentSession]:
    """Create fixture sessions for the configured agent/project matrix."""

    output_dir = Path(config["output_dir"])
    run_root = output_dir / "fixture-worktrees" / f"run-{time.time_ns()}"
    project_count = config["project_count"]
    agent_count = config["agent_count"]
    sessions: list[AgentSession] = []
    prepared_by_project: dict[str, PreparedFixture] = {}

    for project_index in range(project_count):
        project_id = f"project-{project_index + 1}"
        prepared_by_project[project_id] = prepare_fixture_working_copy(
            config["fixture_id"],
            run_root,
            project_id,
        )

    for agent_index in range(agent_count):
        agent_id = f"agent-{agent_index + 1}"
        project_id = f"project-{(agent_index % project_count) + 1}"
        prepared = prepared_by_project[project_id]
        session = client.create_session(os.fspath(prepared.worktree_path))
        readiness = client.wait_for_ready(
            session,
            config["readiness_timeout_seconds"],
        )
        sessions.append(
            AgentSession(
                agent_id=agent_id,
                project_id=project_id,
                prepared_fixture=prepared,
                session=session,
                readiness=readiness,
            )
        )

    return sessions


def run_agent_operations(
    client: CodeRLMClient,
    config: dict[str, Any],
    agent_session: AgentSession,
) -> list[dict[str, Any]]:
    events: list[dict[str, Any]] = []
    context = OperationContext(
        scenario_id=config["scenario_id"],
        fixture_id=config["fixture_id"],
        agent_id=agent_session.agent_id,
        project_id=agent_session.project_id,
        session_id=agent_session.session.session_id,
        project_root=agent_session.session.project_root,
    )
    for item in config["scenario_mix"]:
        operation = item["operation"]
        started = begin_operation()
        try:
            response = run_operation(
                client,
                agent_session.session,
                agent_session.prepared_fixture.fixture.as_dict(),
                operation,
                readiness_timeout_seconds=config["readiness_timeout_seconds"],
            )
            events.append(success_event(operation, context, started, response))
        except Exception as exc:
            events.append(error_event(operation, context, started, exc))
    return events


def run_operation(
    client: CodeRLMClient,
    session: CodeRLMSession | str,
    fixture: dict[str, Any],
    operation: str,
    *,
    readiness_timeout_seconds: int | None = None,
) -> dict[str, Any]:
    targets = fixture["known_targets"]
    session_id = session.session_id if isinstance(session, CodeRLMSession) else session
    if operation in SYMBOL_DEPENDENT_OPERATIONS and isinstance(session, CodeRLMSession):
        client.wait_for_ready(session, readiness_timeout_seconds or 30)

    if operation == "structure":
        target = _target(targets, "structure", operation)
        return client.get(
            "/api/v1/structure",
            session_id=session_id,
            query={"depth": "2", "detail": "1", "file": target["path"]},
        )
    if operation == "search_symbols":
        target = _target(targets, "search_symbols", operation)
        return client.get(
            "/api/v1/symbols/search",
            session_id=session_id,
            query={"q": target["query"], "limit": "20"},
        )
    if operation == "read_implementation":
        target = _target(targets, "read_implementation", operation)
        return client.get(
            "/api/v1/symbols/implementation",
            session_id=session_id,
            query={"symbol": target["symbol"], "file": target["path"]},
        )
    if operation == "list_callers":
        target = _target(targets, "list_callers", operation)
        return client.get(
            "/api/v1/symbols/callers",
            session_id=session_id,
            query={"symbol": target["symbol"], "file": target["path"], "limit": "20"},
        )
    if operation == "list_tests":
        target = _target(targets, "list_tests", operation)
        return client.get(
            "/api/v1/symbols/tests",
            session_id=session_id,
            query={"symbol": target["symbol"], "file": target["path"], "limit": "20"},
        )
    if operation == "grep":
        target = _target(targets, "grep", operation)
        return client.get(
            "/api/v1/grep",
            session_id=session_id,
            query={
                "pattern": target["pattern"],
                "file": target["path"],
                "file_match": "exact",
                "max_matches": "20",
            },
        )
    if operation == "peek_file":
        target = _target(targets, "structure", operation)
        return client.get(
            "/api/v1/peek",
            session_id=session_id,
            query={"file": target["path"], "start": "1", "end": "40"},
        )
    if operation in {"touch_file", "append_file", "replace_file"}:
        target = _target(targets, "structure", operation)
        return client.get(
            "/api/v1/peek",
            session_id=session_id,
            query={"file": target["path"], "start": "1", "end": "20"},
        )
    raise UnsupportedOperationTarget(f"unsupported runtime operation: {operation}")


def _target(targets: dict[str, Any], target_name: str, operation: str) -> dict[str, Any]:
    target = targets.get(target_name)
    if not isinstance(target, dict):
        raise UnsupportedOperationTarget(
            f"fixture target '{target_name}' is unavailable for operation {operation}"
        )
    return target
