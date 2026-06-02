"""Deterministic fixture repository metadata and mutation helpers."""

from __future__ import annotations

import hashlib
import json
import shutil
import tempfile
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Iterable


HARNESS_ROOT = Path(__file__).resolve().parent
FIXTURES_DIR = HARNESS_ROOT / "fixtures"
METADATA_PATH = FIXTURES_DIR / "metadata.json"


class FixtureError(ValueError):
    """Raised when fixture metadata or working-copy operations are invalid."""


@dataclass(frozen=True)
class FixtureMetadata:
    fixture_id: str
    source_path: Path
    description: str
    size_class: str
    languages: tuple[str, ...]
    known_targets: dict[str, Any]
    excluded_generated_dirs: tuple[str, ...]

    def as_dict(self) -> dict[str, Any]:
        return {
            "fixture_id": self.fixture_id,
            "source_path": str(self.source_path),
            "description": self.description,
            "size_class": self.size_class,
            "languages": list(self.languages),
            "known_targets": self.known_targets,
            "excluded_generated_dirs": list(self.excluded_generated_dirs),
        }


@dataclass(frozen=True)
class PreparedFixture:
    fixture: FixtureMetadata
    worktree_path: Path

    def path_for_target(self, target_name: str) -> Path:
        mutation_targets = self.fixture.known_targets.get("watcher_mutation", {})
        relative = mutation_targets.get(target_name)
        if not isinstance(relative, str) or not relative:
            raise FixtureError(
                f"unknown mutation target for {self.fixture.fixture_id}: {target_name}"
            )
        return self._require_worktree_relative(relative)

    def _require_worktree_relative(self, relative: str) -> Path:
        if Path(relative).is_absolute() or ".." in Path(relative).parts:
            raise FixtureError(f"mutation target must be fixture-relative: {relative}")
        path = (self.worktree_path / relative).resolve()
        worktree = self.worktree_path.resolve()
        if path != worktree and worktree not in path.parents:
            raise FixtureError(f"mutation target escapes prepared fixture: {relative}")
        source_path = self.fixture.source_path.resolve()
        if source_path in path.parents or path == source_path:
            raise FixtureError("refusing to mutate checked-in fixture source")
        return path


def list_fixture_ids() -> tuple[str, ...]:
    """Return stable fixture identifiers from metadata."""

    return tuple(sorted(_load_raw_metadata()))


def resolve_fixture(fixture_id: str) -> FixtureMetadata:
    """Resolve a fixture ID to validated source path and known targets."""

    raw_by_id = _load_raw_metadata()
    try:
        raw = raw_by_id[fixture_id]
    except KeyError as exc:
        raise FixtureError(f"unknown fixture_id: {fixture_id}") from exc

    source_path = (FIXTURES_DIR / raw["path"]).resolve()
    if not source_path.is_dir():
        raise FixtureError(f"fixture source path does not exist: {source_path}")

    known_targets = raw.get("known_targets")
    if not isinstance(known_targets, dict) or not known_targets:
        raise FixtureError(f"fixture {fixture_id} must define known_targets")

    _validate_target_files(fixture_id, source_path, _iter_target_paths(known_targets))

    return FixtureMetadata(
        fixture_id=fixture_id,
        source_path=source_path,
        description=_require_string(raw, "description", fixture_id),
        size_class=_require_string(raw, "size_class", fixture_id),
        languages=tuple(_require_string_list(raw, "languages", fixture_id)),
        known_targets=known_targets,
        excluded_generated_dirs=tuple(raw.get("excluded_generated_dirs", [])),
    )


def prepare_fixture_working_copy(
    fixture_id: str,
    run_root: Path | None = None,
    copy_name: str | None = None,
) -> PreparedFixture:
    """Copy a checked-in fixture into a run-specific working directory."""

    fixture = resolve_fixture(fixture_id)
    if run_root is None:
        run_root = Path(tempfile.mkdtemp(prefix="coderlm-fixture-"))
    run_root = Path(run_root)
    run_root.mkdir(parents=True, exist_ok=True)

    destination = run_root / (copy_name or fixture_id)
    if destination.exists():
        raise FixtureError(f"fixture working copy already exists: {destination}")

    shutil.copytree(
        fixture.source_path,
        destination,
        ignore=shutil.ignore_patterns("__pycache__", "*.pyc", ".coderlm"),
    )
    return PreparedFixture(fixture=fixture, worktree_path=destination.resolve())


def touch_known_file(prepared: PreparedFixture, target_name: str = "touch") -> Path:
    """Update mtime for a known file in the prepared working copy."""

    path = prepared.path_for_target(target_name)
    if not path.is_file():
        raise FixtureError(f"cannot touch missing fixture file: {path}")
    path.touch()
    return path


def append_to_known_file(
    prepared: PreparedFixture,
    text: str,
    target_name: str = "append",
) -> Path:
    """Append text to a known file in the prepared working copy."""

    path = prepared.path_for_target(target_name)
    if not path.is_file():
        raise FixtureError(f"cannot append to missing fixture file: {path}")
    with path.open("a", encoding="utf-8") as handle:
        handle.write(text)
    return path


def replace_in_known_file(
    prepared: PreparedFixture,
    old: str,
    new: str,
    target_name: str = "replace",
) -> Path:
    """Replace known text in a prepared working-copy file."""

    path = prepared.path_for_target(target_name)
    if not path.is_file():
        raise FixtureError(f"cannot replace text in missing fixture file: {path}")
    content = path.read_text(encoding="utf-8")
    if old not in content:
        raise FixtureError(f"replacement text not found in fixture file: {path}")
    path.write_text(content.replace(old, new, 1), encoding="utf-8")
    return path


def fixture_digest(root: Path) -> str:
    """Return a deterministic digest for all checked-in fixture bytes under root."""

    digest = hashlib.sha256()
    for path in sorted(Path(root).rglob("*")):
        if _is_generated_path(path):
            continue
        if not path.is_file():
            continue
        relative = path.relative_to(root).as_posix()
        digest.update(relative.encode("utf-8"))
        digest.update(b"\0")
        digest.update(path.read_bytes())
        digest.update(b"\0")
    return digest.hexdigest()


def _is_generated_path(path: Path) -> bool:
    generated_names = {"__pycache__", ".coderlm", "target", "node_modules"}
    if any(part in generated_names for part in path.parts):
        return True
    return path.suffix == ".pyc"


def _load_raw_metadata() -> dict[str, dict[str, Any]]:
    try:
        raw = json.loads(METADATA_PATH.read_text(encoding="utf-8"))
    except FileNotFoundError as exc:
        raise FixtureError(f"fixture metadata does not exist: {METADATA_PATH}") from exc
    except json.JSONDecodeError as exc:
        raise FixtureError(f"fixture metadata is not valid JSON: {exc}") from exc
    if not isinstance(raw, dict):
        raise FixtureError("fixture metadata must contain a JSON object")
    return raw


def _require_string(raw: dict[str, Any], key: str, fixture_id: str) -> str:
    value = raw.get(key)
    if not isinstance(value, str) or not value.strip():
        raise FixtureError(f"fixture {fixture_id} field must be a string: {key}")
    return value


def _require_string_list(raw: dict[str, Any], key: str, fixture_id: str) -> list[str]:
    value = raw.get(key)
    if not isinstance(value, list) or not value:
        raise FixtureError(f"fixture {fixture_id} field must be a non-empty list: {key}")
    if not all(isinstance(item, str) and item.strip() for item in value):
        raise FixtureError(f"fixture {fixture_id} field must contain strings: {key}")
    return value


def _iter_target_paths(known_targets: dict[str, Any]) -> Iterable[str]:
    for value in known_targets.values():
        if isinstance(value, str):
            yield value
        elif isinstance(value, dict):
            path = value.get("path")
            if isinstance(path, str):
                yield path
            for nested in value.values():
                if isinstance(nested, str) and "/" in nested:
                    yield nested
                elif isinstance(nested, dict) and isinstance(nested.get("path"), str):
                    yield nested["path"]
        elif isinstance(value, list):
            for entry in value:
                if isinstance(entry, dict) and isinstance(entry.get("path"), str):
                    yield entry["path"]


def _validate_target_files(
    fixture_id: str,
    source_path: Path,
    relative_paths: Iterable[str],
) -> None:
    for relative in relative_paths:
        path = Path(relative)
        if path.is_absolute() or ".." in path.parts:
            raise FixtureError(
                f"fixture {fixture_id} target must be fixture-relative: {relative}"
            )
        if not (source_path / relative).exists():
            raise FixtureError(
                f"fixture {fixture_id} target path does not exist: {relative}"
            )
