---
name: coderlm
description: "Structural codebase navigation for supported languages (Rust, Python, TypeScript, JavaScript, Go, Java, Scala, Elixir, Ruby, PHP, Zig, SQL). Provides tree-sitter-backed indexing: find symbols by name, get exact function bodies, follow caller chains, discover tests, and trace execution paths. Works best combined with targeted file reads: use coderlm CLI (`search`, `impl`, `callers`, `grep`) to locate the right code across a codebase, then read identified files directly when full context is needed. Use for: answering 'what calls X', 'how does X work', 'where is X defined', and 'where does this error come from'. Avoid raw bash grep/find — use the coderlm `grep` command instead."
allowed-tools:
  - Bash
  - Read
  - Agent
---

# CodeRLM — Structural Codebase Exploration

You have access to a tree-sitter-backed index server. Use it instead of guessing with grep.

## FIRST: Choose Your Mode

**Is the question "how does X work", "trace X", or "follow the execution path of X"?**

→ **Delegate immediately to a subcall agent. Do not do any CLI searches first.**

```
Agent(coderlm-subcall, "Trace how <X> works. Find the entrypoint, follow callers, and show the full execution path with code extracts.")
```

The subcall agent has a structured workflow for tracing and will return findings with code evidence. Using it keeps the full exploration out of your context window and produces more complete results than piecemeal CLI calls.

**Is the question a targeted lookup ("where is X defined", "what calls X", "show me X")?**

→ Use CLI commands directly (see below).

**When to just Read a file directly.** After 1-2 `search` or `structure` calls, if the relevant code is clearly concentrated in 1-3 files, **Read those files directly** instead of reconstructing them with granular CLI calls. Read is cheaper than 10+ `impl`/`symbols`/`peek` calls for a focused file. Only keep using CLI lookups when tracing across many files (5+) or when you need symbol-level precision that Read can't give you.

**NEVER fall back to raw bash grep/find/cat.** If a coderlm CLI command fails, use the coderlm fallback rules — not shell tools.

---

## Setup

```bash
CLI=".claude/coderlm_state/coderlm_cli.py"
```

Session is auto-created by the plugin. If not, run `python3 $CLI init`.

## Quick Lookups: One-Off Commands

For a single targeted lookup:

```bash
python3 $CLI search MyFunction
python3 $CLI impl MyFunction --file path/to/file.py
python3 $CLI callers MyFunction --file path/to/file.py
```

## Multiple Lookups: Batch Mode

For 2-5 targeted lookups, use batch to minimize tool calls:

```bash
python3 $CLI batch --commands "
search MyFunction
impl MyFunction --file path/to/file.py
callers MyFunction --file path/to/file.py
"
```

## Multiple Lookups: Python Exec Mode

For tracing a full execution path in one shot — search, then follow callers up the chain:

```bash
python3 $CLI exec --code "
# Find entrypoint, read its implementation, then walk callers up the chain
hits = search('serialize_response')
if hits.get('symbols'):
    sym = hits['symbols'][0]
    impl_(sym['name'], sym['file'])                    # read the function
    c = callers(sym['name'], sym['file'])               # find who calls it
    if c.get('callers'):
        caller = c['callers'][0]
        impl_(caller['name'], caller['file'])           # read the caller
        callers(caller['name'], caller['file'])         # one more hop up
"
```

Use exec mode when you need to: follow a call chain across 3+ hops, or make later queries depend on earlier results. One exec call replaces many individual Bash calls and keeps the full trace in a single result.

Available helpers in exec mode: `search()`, `impl_()`, `callers()`, `tests()`, `grep()`, `symbols()`, `peek_file()`, `structure()`, `variables_list()`.

## Commands

| Command | Purpose |
|---------|---------|
| `search QUERY [--file FILE]` | Find symbols by name |
| `impl SYMBOL --file FILE` | Get full source code |
| `callers SYMBOL --file FILE` | Find call sites |
| `tests SYMBOL --file FILE` | Find tests referencing symbol |
| `grep PATTERN [--scope code] [--file FILE]` | Regex search (`--scope code` skips comments) |
| `peek FILE --start N --end N` | Read a line range |
| `symbols [--file FILE] [--kind KIND]` | List all symbols |
| `structure [--depth N]` | Project file tree |

## Fallback Rules (coderlm only — not raw bash)

- `search` returns 0 → use `grep` for the name
- `impl` fails (404) → use `peek` with the file path
- `callers` returns nothing → `grep` for the symbol name
- Don't retry failures — pivot to an alternative

## Output Requirements

1. **Code extracts** — Include actual source from `impl` or `peek` as fenced code blocks
2. **File:line references** — Cite specific locations for every claim
3. **Chain of evidence** — Show each hop when tracing execution paths

## Inputs

This skill reads `$ARGUMENTS`. Accepted patterns:
- `query=<question>` (required): what to find or understand
- `cwd=<path>` (optional): project directory, defaults to cwd
- `port=<N>` (optional): server port, defaults to 3000

If no query is provided, ask what the user wants to find or understand about the codebase.

## Workflow

1. **Init** — `cli init` to create a session and index the project.
2. **Orient** — `cli structure` to see the project layout. Identify likely starting points.
3. **Find the entrypoint** — `cli search` or `cli grep` to locate the starting symbol or pattern.
4. **Retrieve** — `cli impl` to read the exact implementation. Not the file. The function.
5. **Trace** — `cli callers` to see what calls it. `cli impl` on those callers. Follow the chain.
6. **Widen** — `cli tests` to find test coverage. `cli grep` for related patterns discovered during tracing.
7. **Annotate** — `cli define-symbol` and `cli define-file` as understanding solidifies.
8. **Synthesize** — Compile findings into a coherent answer with specific file:line references.

Steps 3–7 repeat. A typical exploration is: find a symbol → read its implementation → trace its callers → read those implementations → discover related symbols → repeat until the causal chain is clear.

## When to Use the Server vs Native Tools

| Task | Use server | Why |
|------|-----------|-----|
| Find a function by name | `search` | Index lookup, not file globbing |
| Find code when name is unknown | `grep` + `symbols` | Searches all indexed files at once |
| Get a function's source | `impl` | Returns just that function, even from large files |
| Read specific lines | `peek` | Surgical extraction, not the whole file |
| Find what calls a function | `callers` | Cross-project search with exact call sites |
| Find tests for a function | `tests` | By symbol reference, not filename guessing |
| Get project overview | `structure` | Tree with file counts and language breakdown |
| Read an entire small file | Read tool | When you genuinely need the whole file |

**Default to the server.** Use Read only when you need an entire file or the server is unavailable.

## Troubleshooting

- **"Cannot connect to coderlm-server"** — Server not running. Start with `coderlm-server serve`.
- **"No active session"** — Run `cli init` first.
- **"Project was evicted"** — Server hit capacity (default 5 projects). Re-run `cli init`.
- **Search returns nothing relevant** — Try broader grep patterns or list all symbols: `cli symbols --limit 200`.

For the full API endpoint reference, see [references/api-reference.md](references/api-reference.md).
