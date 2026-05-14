# coderlm-server

A Rust-based code-aware REPL server for the CoderLM Recursive Language Model system. It indexes codebases on-demand, extracts symbols via tree-sitter, and exposes a JSON API that agents query for targeted context — structure, symbols, source, callers, tests, grep, and more.

Zero files are created inside the target repository. The server runs externally, watches the filesystem for changes, and supports multiple simultaneous agent sessions across multiple projects.

## Prerequisites

- **Rust toolchain** (rustc 1.70+). Install via [rustup](https://rustup.rs/):
  ```
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
  ```
- **A C compiler** (gcc/clang) — required by tree-sitter's native code generation.
  ```
  # Ubuntu/Debian
  sudo apt install build-essential

  # macOS (Xcode command line tools)
  xcode-select --install
  ```

## Building

```bash
cd server/
cargo build --release
```

The binary is at `target/release/coderlm-server`.

To install it into your PATH:

```bash
cargo install --path .
```

## Quick start

```bash
# Start the server (no project path required — projects are registered on-demand)
coderlm-server serve

# Or pre-index a specific project at startup
coderlm-server serve /path/to/your/project

# Verify it's running
curl http://127.0.0.1:3000/api/v1/health
```

Output:

```json
{
  "status": "ok",
  "projects": 0,
  "active_sessions": 0,
  "max_projects": 5
}
```

Create a session to start working with a project:

```bash
curl -X POST http://127.0.0.1:3000/api/v1/sessions \
  -H "Content-Type: application/json" \
  -d '{"cwd":"/path/to/your/project"}'
```

The server indexes the project directory (if not already known) and returns a `session_id`. All subsequent API calls use this session ID via the `X-Session-Id` header to scope queries to that project.

## CLI options

```
coderlm-server serve [PATH] [OPTIONS]

Arguments:
  [PATH]  Optional path to pre-index at startup

Options:
  -p, --port <PORT>                  Port to listen on [default: 3000]
  -b, --bind <ADDR>                  Bind address [default: 127.0.0.1]
      --max-file-size <BYTES>        Skip files larger than this [default: 1000000]
      --max-projects <N>             Maximum concurrent indexed projects [default: 5]
```

## Logging

Control log verbosity with the `RUST_LOG` environment variable:

```bash
# Default (info)
coderlm-server serve

# Debug logging (shows per-file symbol extraction)
RUST_LOG=debug coderlm-server serve

# Quiet (warnings and errors only)
RUST_LOG=warn coderlm-server serve
```

## Managing multiple projects

A single server instance supports multiple projects simultaneously. Projects are registered automatically when an agent creates a session with a `cwd` pointing to that project. No manual setup is needed.

### How it works

1. Agent creates a session: `POST /sessions` with `{ "cwd": "/home/user/myproject" }`
2. Server indexes the directory (file tree scan + background symbol extraction + filesystem watcher)
3. Session is scoped to that project — all queries only see that project's files and symbols
4. Multiple agents can connect to different projects on the same server

### Example: two repos, one server

```bash
# Start the server
coderlm-server serve --port 3000

# Agent A connects to the backend
SESSION_A=$(curl -s -X POST localhost:3000/api/v1/sessions \
  -H "Content-Type: application/json" \
  -d '{"cwd":"/home/user/backend"}' | jq -r .session_id)

# Agent B connects to the frontend
SESSION_B=$(curl -s -X POST localhost:3000/api/v1/sessions \
  -H "Content-Type: application/json" \
  -d '{"cwd":"/home/user/frontend"}' | jq -r .session_id)

# Each agent only sees its own project
curl -H "X-Session-Id: $SESSION_A" localhost:3000/api/v1/structure  # backend files
curl -H "X-Session-Id: $SESSION_B" localhost:3000/api/v1/structure  # frontend files
```

### Capacity and LRU eviction

The server keeps at most `--max-projects` projects indexed at once (default: 5). When a new project would exceed this limit, the **least recently used** project is evicted — its file tree, symbols, and filesystem watcher are dropped, and all sessions pointing to it are cleaned up. Agents using those sessions will receive `410 Gone` responses and can simply create a new session to re-index.

```bash
# Allow more concurrent projects
coderlm-server serve --max-projects 10
```

### Admin visibility

Check which projects are currently indexed:

```bash
curl localhost:3000/api/v1/roots
```

Returns each project's path, file count, symbol count, readiness state, watcher
state, last active time, session count, and cheap caller-cache statistics. The
caller-cache statistics are a read-only snapshot with entry, hit, miss, and
invalidation counts; misses include unsupported files rather than exposing
separate per-reason counters.

Example response:

```json
{
  "count": 1,
  "roots": [
    {
      "path": "/home/user/backend",
      "file_count": 142,
      "symbol_count": 1038,
      "last_active": "2026-02-07T19:05:00Z",
      "session_count": 1,
      "readiness": "ready",
      "ready": true,
      "extraction_complete": true,
      "last_indexed_at": "2026-02-07T19:04:58Z",
      "watcher_enabled": true,
      "watcher_state": "enabled",
      "caller_cache_stats": {
        "entry_count": 12,
        "hit_count": 34,
        "miss_count": 5,
        "invalidation_count": 2
      }
    }
  ]
}
```

`readiness` is `"indexing"` until initial background symbol extraction finishes,
then `"ready"`; `ready` mirrors that state as a boolean and
`extraction_complete` reports the same initial extraction completion flag.
During `"indexing"`, file-tree operations can already return the cold-scan file
index, but symbol-dependent endpoints may return partial results. Clients that
need complete symbol, caller, test, or variable results should poll `/roots`
until the matching root reports `"readiness": "ready"`. `last_indexed_at`
updates after initial extraction completes and after watcher-driven re-indexes.

List all active sessions:

```bash
curl localhost:3000/api/v1/sessions
```

View command history across all sessions (no `X-Session-Id` needed):

```bash
curl localhost:3000/api/v1/history
```

### Recommendations

- **One server is enough.** Projects are auto-registered on session creation. No need to run separate instances per repo.
- **Annotations are per-project.** File definitions, symbol definitions, and marks set by one session are visible to all sessions on the same project. This lets a swarm of agents build shared understanding.
- **Filesystem watcher is automatic.** When you edit files in a project, the server detects changes within ~500ms and re-indexes. No restart needed.
- **Annotations can be persisted.** Use `save-annotations` to write definitions and marks to `.coderlm/annotations.json` in the project root. Annotations are auto-loaded when a new session is created for that project. The `Stop` hook also auto-saves annotations before cleanup.

## File path filtering

All file paths accepted by data endpoints are project-relative indexed paths,
not absolute paths. `GET /peek` and `GET /chunk_indices` require an exact
`file` value such as `src/main.rs`.

`GET /grep` accepts `file` plus an optional `file_match` mode:

- `file_match=exact` searches only the indexed path equal to `file`.
- `file_match=suffix` searches the one indexed path that ends with `file`.
- `file_match=contains` searches the one indexed path that contains `file`.

When `file_match` is set, the filter must resolve to exactly one indexed file.
Zero matches return a no-match error, and multiple matches return an ambiguity
error listing the matched paths. When `file_match` is omitted, grep preserves
legacy compatibility behavior and searches any indexed path where `file` is an
exact, contains, or suffix match, which can intentionally cover multiple files.

## Indexing and watcher tuning

- `--max-file-size <BYTES>` skips files larger than the configured size during cold indexing and removes them from the live index if an edit pushes them over the limit. The default is `1,000,000` bytes.
- `--max-projects <N>` bounds the number of simultaneously indexed project roots. When the limit is reached, the least recently used project is evicted with its watcher and symbols.
- `CODERLM_DISABLE_WATCHER=1` starts projects without filesystem watchers. Use this for generated-heavy workspaces when manual session recreation is preferable to live re-indexing.
- Built-in ignored directories include dependency, build, VCS, cache, coverage, and Journey runtime directories such as `node_modules`, `vendor`, `target`, `.git`, `.cache`, and `.journey`. These ignores are applied in addition to `.gitignore`.
- Watcher updates are debounced for roughly 500 ms and coalesce duplicate events for the same path before updating the file tree or reparsing symbols.

## Supported languages (tree-sitter)

Symbol extraction (functions, classes, structs, methods, etc.) is available for:

| Language   | Extensions                    | Support      |
|------------|-------------------------------|--------------|
| Rust       | `.rs`                         | tree-sitter  |
| Python     | `.py`, `.pyi`                 | tree-sitter  |
| TypeScript | `.ts`, `.tsx`                 | tree-sitter  |
| JavaScript | `.js`, `.jsx`, `.mjs`, `.cjs` | tree-sitter  |
| Go         | `.go`                         | tree-sitter  |
| Java       | `.java`                       | tree-sitter  |
| Scala      | `.scala`, `.sc`               | tree-sitter  |
| Ruby       | `.rb`, `.rake`                | tree-sitter  |
| PHP        | `.php`, `.phtml`              | tree-sitter  |
| Zig        | `.zig`, `.zon`                | tree-sitter  |
| SQL        | `.sql`                        | regex        |

Languages with tree-sitter support produce full symbol tables (functions, classes, methods, callers, variables). SQL uses regex fallbacks for variable and definition detection. All other file types are indexed in the file tree and available for peek/grep/chunk operations, but do not produce symbols.

## API overview

All endpoints are under `/api/v1`. Data endpoints require `X-Session-Id` header to scope queries to a project.

| Method | Endpoint                    | Session required | Purpose                              |
|--------|-----------------------------|------------------|--------------------------------------|
| GET    | `/health`                   | No               | Server status (project/session counts) |
| GET    | `/roots`                    | No               | List all registered projects (admin) |
| GET    | `/sessions`                 | No               | List all active sessions (admin)     |
| POST   | `/sessions`                 | No               | Create session with `{ "cwd": "..." }` (response includes L1 structure) |
| GET    | `/sessions/:id`             | No               | Get session info                     |
| DELETE | `/sessions/:id`             | No               | Delete a session                     |
| GET    | `/structure`                | Yes              | File tree (`?detail=0..3` for symbol summaries / signatures / source) |
| POST   | `/structure/define`         | Yes              | Set file definition                  |
| POST   | `/structure/redefine`       | Yes              | Update file definition               |
| POST   | `/structure/mark`           | Yes              | Mark file type (test, docs, etc.)    |
| GET    | `/symbols`                  | Yes              | List symbols (filter by kind/file)   |
| GET    | `/symbols/search`           | Yes              | Search symbols by name (`?file=` to scope to one file) |
| POST   | `/symbols/define`           | Yes              | Set symbol definition                |
| POST   | `/symbols/redefine`         | Yes              | Update symbol definition             |
| GET    | `/symbols/implementation`   | Yes              | Get full source of a symbol          |
| GET    | `/symbols/callers`          | Yes              | Find call sites for a symbol         |
| GET    | `/symbols/tests`            | Yes              | Find tests that reference a symbol   |
| GET    | `/symbols/variables`        | Yes              | List local variables in a function   |
| GET    | `/peek`                     | Yes              | Read a line range from a file        |
| GET    | `/grep`                     | Yes              | Regex search (`?scope=code` skips comments/strings; `?file=` filters paths; `?file_match=exact|suffix|contains` requires one unambiguous match) |
| GET    | `/chunk_indices`            | Yes              | Compute byte-range chunks for a file |
| GET    | `/history`                  | Optional         | With session: session history. Without: all sessions (admin) |
| POST   | `/annotations/save`         | Yes              | Persist annotations to `.coderlm/annotations.json` |
| POST   | `/annotations/load`         | Yes              | Load annotations from disk           |
| GET    | `/buffers`                  | Yes              | List named scratch buffers           |
| POST   | `/buffers`                  | Yes              | Create buffer from raw content       |
| POST   | `/buffers/from-file`        | Yes              | Create buffer from a file (or line range) |
| POST   | `/buffers/from-symbol`      | Yes              | Create buffer from a symbol's source |
| GET    | `/buffers/:name`            | Yes              | Get a buffer's full content          |
| GET    | `/buffers/:name/peek`       | Yes              | Read a line range from a buffer      |
| DELETE | `/buffers/:name`            | Yes              | Delete a buffer                      |
| GET    | `/vars`                     | Yes              | List project-scoped JSON variables   |
| POST   | `/vars`                     | Yes              | Set a JSON variable                  |
| GET    | `/vars/:name`               | Yes              | Get a JSON variable                  |
| DELETE | `/vars/:name`               | Yes              | Delete a JSON variable               |
| GET    | `/subcall_results`          | Yes              | List sub-agent call results          |
| POST   | `/subcall_results`          | Yes              | Append a sub-agent call result       |
| DELETE | `/subcall_results`          | Yes              | Clear all sub-agent call results     |

See `REPL_to_API.md` for the full mapping from REPL operations to curl commands, including request/response shapes for each endpoint.
