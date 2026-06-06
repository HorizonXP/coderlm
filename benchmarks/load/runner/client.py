"""Small public-API client for CodeRLM load-harness workers."""

from __future__ import annotations

import json
import socket
import time
from dataclasses import dataclass
from typing import Any
from urllib import error, parse, request


class CodeRLMClientError(RuntimeError):
    """Raised when a CodeRLM API request fails."""

    def __init__(
        self,
        message: str,
        *,
        method: str | None = None,
        url: str | None = None,
        status_code: int | None = None,
        detail: str | None = None,
    ) -> None:
        super().__init__(message)
        self.method = method
        self.url = url
        self.status_code = status_code
        self.detail = detail


class ReadinessTimeout(CodeRLMClientError):
    """Raised when a project root does not report ready before the deadline."""


@dataclass(frozen=True)
class CodeRLMSession:
    """Session identity returned by the server for a fixture project."""

    session_id: str
    project_root: str
    raw: dict[str, Any]


class CodeRLMClient:
    """Thin JSON-over-HTTP wrapper around the public CodeRLM API."""

    def __init__(self, base_url: str, *, request_timeout_seconds: float = 5.0) -> None:
        self.base_url = base_url.rstrip("/")
        self.request_timeout_seconds = request_timeout_seconds

    def health(self) -> dict[str, Any]:
        return self.request("GET", "/api/v1/health")

    def create_session(self, cwd: str) -> CodeRLMSession:
        payload = self.request("POST", "/api/v1/sessions", body={"cwd": cwd})
        session_id = payload.get("session_id")
        project = payload.get("project")
        if not isinstance(session_id, str) or not session_id:
            raise CodeRLMClientError(f"session creation returned no session_id: {payload!r}")
        if not isinstance(project, str) or not project:
            raise CodeRLMClientError(f"session creation returned no project root: {payload!r}")
        return CodeRLMSession(session_id=session_id, project_root=project, raw=payload)

    def wait_for_ready(
        self,
        session: CodeRLMSession,
        timeout_seconds: int,
        *,
        poll_interval_seconds: float = 0.25,
    ) -> dict[str, Any]:
        deadline = time.monotonic() + timeout_seconds
        last_payload: dict[str, Any] | None = None
        while time.monotonic() < deadline:
            payload = self.request("GET", "/api/v1/roots")
            last_payload = payload
            roots = payload.get("roots", [])
            if not isinstance(roots, list):
                raise CodeRLMClientError(
                    f"roots endpoint returned invalid roots payload: {payload!r}",
                    method="GET",
                    url=f"{self.base_url}/api/v1/roots",
                )
            for root in roots:
                if not isinstance(root, dict):
                    continue
                if root.get("path") == session.project_root and root.get("ready") is True:
                    return root
            remaining = deadline - time.monotonic()
            if remaining > 0:
                time.sleep(min(poll_interval_seconds, remaining))
        raise ReadinessTimeout(
            "project did not become ready after "
            f"{timeout_seconds}s for session {session.session_id}; "
            f"last roots payload: {last_payload!r}",
            method="GET",
            url=f"{self.base_url}/api/v1/roots",
        )

    def get(
        self,
        path: str,
        *,
        session_id: str | None = None,
        query: dict[str, str] | None = None,
    ) -> dict[str, Any]:
        return self.request("GET", path, session_id=session_id, query=query)

    def request(
        self,
        method: str,
        path: str,
        *,
        body: dict[str, Any] | None = None,
        session_id: str | None = None,
        query: dict[str, str] | None = None,
    ) -> dict[str, Any]:
        url = self._url(path, query)
        data = None
        headers = {"Accept": "application/json"}
        if session_id:
            headers["X-Session-Id"] = session_id
        if body is not None:
            data = json.dumps(body).encode("utf-8")
            headers["Content-Type"] = "application/json"

        req = request.Request(url, data=data, headers=headers, method=method)
        try:
            with request.urlopen(req, timeout=self.request_timeout_seconds) as response:
                payload = json.loads(response.read().decode("utf-8"))
                if not isinstance(payload, dict):
                    raise CodeRLMClientError(
                        f"{method} {url} returned non-object JSON: {payload!r}",
                        method=method,
                        url=url,
                        detail="non-object JSON response",
                    )
                return payload
        except error.HTTPError as exc:
            detail = exc.read().decode("utf-8", errors="replace")
            raise CodeRLMClientError(
                f"{method} {url} returned HTTP {exc.code}: {detail}",
                method=method,
                url=url,
                status_code=exc.code,
                detail=detail,
            ) from exc
        except error.URLError as exc:
            raise CodeRLMClientError(
                f"{method} {url} failed: {exc.reason}",
                method=method,
                url=url,
                detail=str(exc.reason),
            ) from exc
        except (TimeoutError, socket.timeout) as exc:
            raise CodeRLMClientError(
                f"{method} {url} timed out",
                method=method,
                url=url,
                detail="timeout",
            ) from exc
        except json.JSONDecodeError as exc:
            raise CodeRLMClientError(
                f"{method} {url} returned invalid JSON: {exc}",
                method=method,
                url=url,
                detail=str(exc),
            ) from exc

    def _url(self, path: str, query: dict[str, str] | None = None) -> str:
        if not path.startswith("/"):
            raise CodeRLMClientError(f"API path must start with '/': {path!r}")
        url = f"{self.base_url}{path}"
        if query:
            url = f"{url}?{parse.urlencode(query)}"
        return url
