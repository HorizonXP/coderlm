# CodeRLM Profiling Baseline

Date: 2026-05-14
Baseline host: Linux workstation `singularity`, release build, `/usr/bin/time -v`, `curl`, `jq`
Repository: `/home/xpatel/Development/coderlm/.journey/worktrees/issue-23`

This baseline is measurement-first. It records reproducible CPU, wall-time, and
RSS commands plus current results for cold indexing, warm session creation,
repeated code-scope grep, caller lookup, watcher updates, and common API
operations. It does not include performance fixes.

## Environment

Build the optimized server once before measuring:

```bash
cd server
cargo build --release
```

Environment captured for this run:

| Item | Value |
| --- | --- |
| Host OS | Linux `singularity` 6.17.0-23-generic x86_64 |
| Rust | `rustc 1.94.1 (e408947bf 2026-03-25)` |
| Cargo | `cargo 1.94.1 (29ea6fb6a 2026-03-24)` |
| Server binary | `server/target/release/coderlm-server` |
| Timed root | `/home/xpatel/Development/coderlm/.journey/worktrees/issue-23` |
| `/usr/bin/time -v` | available |
| `jq` | `jq-1.8.1` |
| `perf` | `/usr/bin/perf` |
| `hyperfine` | not installed |
| `cargo flamegraph` | not installed |

Notes:

- The timed root is the Journey worktree for this issue. Runtime directories
  such as `.journey`, `.git`, and `server/target` are skipped by the server's
  ignore rules.
- Endpoint names and query parameters were checked against
  `server/src/server/routes.rs` before recording commands.

## Cold Indexing

Command:

```bash
cd server
PORT=31323
ROOT="$(pwd)/.."
/usr/bin/time -v timeout --signal=INT 2s \
  ./target/release/coderlm-server serve --port "$PORT" "$ROOT"
```

Additional live polling command pattern used for wall-time milestones:

```bash
START_MS=$(date +%s%3N)
./target/release/coderlm-server serve --port 31324 "$ROOT" &
PID=$!
until curl -fsS "http://127.0.0.1:31324/api/v1/health" >/dev/null; do
  sleep 0.02
done
curl -fsS "http://127.0.0.1:31324/api/v1/roots" | jq .
```

Captured result:

| Metric | Value |
| --- | ---: |
| Indexed files | 60 |
| Extracted symbols | 515 |
| Health endpoint available | 55 ms |
| First populated symbol table visible via `/api/v1/roots` | 86 ms |
| Full symbol table visible via `/api/v1/roots` | 575 ms |
| Server log: synchronous directory scan | 2.831 ms |
| Server log: full symbol extraction | 515 ms |
| Timed process wall time | 2.01 s |
| User CPU | 1.29 s |
| System CPU | 0.07 s |
| CPU utilization during timed window | 67% |
| Peak RSS | 26,204 KB |

Notes:

- The server pre-indexes files synchronously and starts the HTTP listener before
  background symbol extraction is fully complete.
- The 2 second timed window includes idle server time after indexing, so CPU
  percentage is lower than the burst CPU used during extraction.

## Warm Session Creation

Command:

```bash
ROOT=/home/xpatel/Development/coderlm/.journey/worktrees/issue-23
SID=$(
  curl -fsS -X POST "http://127.0.0.1:31324/api/v1/sessions" \
    -H 'Content-Type: application/json' \
    -d "{\"cwd\":\"$ROOT\"}" | jq -r .session_id
)
curl -fsS "http://127.0.0.1:31324/api/v1/roots" |
  jq --arg root "$ROOT" '.roots[] | select(.path==$root)'
```

Captured result:

| Metric | Value |
| --- | ---: |
| Session creation latency | 4.385 ms |
| File count after warm session | 60 |
| Symbol count after warm session | 515 |
| Server RSS after warm session | 22,936 KB |
| Observed extraction repeat | No full rescan; reused existing project |

The warm-session path returned the already indexed project rather than running
`walker::scan_directory` again.

## Warm API Queries

Command pattern:

```bash
curl -fsS -w '\n%{time_total}' \
  -H "X-Session-Id: $SID" \
  "http://127.0.0.1:31324/api/v1/<endpoint>"
```

Captured result:

| Operation | Endpoint | Wall time | Count |
| --- | --- | ---: | ---: |
| Symbols | `/api/v1/symbols?limit=500` | 5.443 ms | 495 |
| Search | `/api/v1/symbols/search?q=extract_symbols&limit=20` | 1.135 ms | 1 |
| Implementation | `/api/v1/symbols/implementation?symbol=extract_symbols_from_file&file=server/src/symbols/parser.rs` | 1.079 ms | n/a |
| Callers, common symbol | `/api/v1/symbols/callers?symbol=insert&file=server/src/symbols/mod.rs&limit=50` | 208.974 ms | 29 |
| Callers, rare symbol | `/api/v1/symbols/callers?symbol=WatcherHandle&file=server/src/index/watcher.rs&limit=50` | 31.768 ms | 1 |
| Grep, all scope | `/api/v1/grep?pattern=extract_symbols&max_matches=100&scope=all` | 3.224 ms | 11 |
| Grep, code scope first fill | `/api/v1/grep?pattern=extract_symbols&max_matches=100&scope=code` | 232.047 ms | 11 |
| Structure | `/api/v1/structure?depth=3` | 0.892 ms | n/a |
| Server RSS after warm queries | `ps -o rss= -p "$PID"` | 32,692 KB | n/a |

## Repeated Code-Scope Grep

Command:

```bash
for n in 1 2 3 4 5; do
  curl -fsS -o /tmp/coderlm-grep-code-$n.json \
    -w "code_grep_cached_run_$n time_s=%{time_total}\n" \
    -H "X-Session-Id: $SID" \
    "http://127.0.0.1:31324/api/v1/grep?pattern=extract_symbols&max_matches=100&scope=code"
done
```

Captured result after the first fill above:

| Run | Wall time | Matches |
| --- | ---: | ---: |
| 1 | 3.087 ms | 11 |
| 2 | 2.844 ms | 11 |
| 3 | 2.873 ms | 11 |
| 4 | 3.032 ms | 11 |
| 5 | 2.775 ms | 11 |
| Average | 2.922 ms | 11 |

Observation: repeated identical code-scope grep now reuses cached non-code byte
ranges stored on `FileTree`. The first code-scope request still pays the
tree-sitter range fill cost, but later requests are close to all-scope grep
latency. The cache is keyed by file metadata and language and is invalidated
when files are inserted, updated, or removed.

## Watcher Updates

Command outline:

```bash
TMPROOT=$(mktemp -d /tmp/coderlm-watch23.XXXXXX)
mkdir -p "$TMPROOT/src" "$TMPROOT/target/huge" "$TMPROOT/docs"
printf 'pub fn baseline() -> usize { 1 }\nfn main() { let _ = baseline(); }\n' \
  > "$TMPROOT/src/main.rs"
python3 - <<'PY' "$TMPROOT/target/huge/generated.rs" "$TMPROOT/docs/notes.txt"
import sys
open(sys.argv[1], "w").write("pub fn ignored_generated() {}\n" * 5000)
open(sys.argv[2], "w").write("unsupported notes\n" * 1000)
PY

curl -fsS -X POST "http://127.0.0.1:31324/api/v1/sessions" \
  -H 'Content-Type: application/json' \
  -d "{\"cwd\":\"$TMPROOT\"}"

printf '\npub fn single_edit() -> usize { baseline() + 1 }\n' \
  >> "$TMPROOT/src/main.rs"

for n in 1 2 3 4 5; do
  printf 'pub fn burst_%s() -> usize { %s }\n' "$n" "$n" \
    > "$TMPROOT/src/burst_$n.rs"
done
```

Captured result:

| Scenario | Measurement |
| --- | ---: |
| Initial temp-project files | 2 |
| Initial temp-project symbols | 2 |
| Single supported-file edit visible in `/api/v1/roots` | 554 ms |
| Burst save of 5 supported files visible in `/api/v1/roots` | 575 ms |
| Final temp-project files | 7 |
| Final temp-project symbols | 8 |
| Large ignored `target/` matches | 0 |
| Unsupported `.txt` grep matches | 1000 |

Observation: watcher latency is dominated by the 500 ms debounce in
`server/src/index/watcher.rs`. Events are coalesced by relative path before
re-indexing. Files under ignored directories are skipped. Unsupported files are
indexed for content grep but do not produce tree-sitter symbols.

## Top Suspects

1. Caller lookup CPU: `server/src/ops/symbol_ops.rs` scans indexed files and
   parses AST-supported files that contain the symbol text. The common `insert`
   caller query is the slowest warm query at about 209 ms, while the rare
   `WatcherHandle` caller query is about 32 ms.
2. Code-scope grep first-fill CPU: `server/src/ops/content.rs` still needs to
   compute non-code ranges on the first `scope=code` request for each supported
   file. Repeated requests are fast after `FileTree` cache population.
3. Symbol extraction CPU: `server/src/symbols/parser.rs` creates parsers and
   queries during cold extraction. Full extraction took about 515-575 ms and
   peak RSS during the 2 second timed window was 26,204 KB.
4. Watcher observability: `server/src/index/watcher.rs` coalesces events and
   reparses each changed supported file after debounce, but the public baseline
   still infers event and reparse counts indirectly from `/api/v1/roots`.

## Follow-Up Optimization Targets

- Add a call-site index or per-file caller cache keyed by file mtime/content.
  Success metric: `callers insert` averages less than 50 ms and rare-symbol
  callers average less than 20 ms on this repository.
- Reduce first-fill code-scope grep cost without regressing the cached path.
  Success metric: first `grep --scope code` for `extract_symbols` drops from
  roughly 232 ms to less than 50 ms, while five cached repeats stay under 5 ms
  on average.
- Reuse or precompile language query objects where tree-sitter allows it.
  Success metric: cold extraction CPU drops by at least 25% with peak RSS not
  exceeding the 26,204 KB baseline by more than 10%.
- Add debug or metrics counters for watcher events and reparses.
  Success metric: single-file and burst-save baselines report event count,
  unique path count, reparse count, and latency without relying only on
  indirect `/api/v1/roots` polling.

## Query Reuse Update

Issue 27 adds a per-language/per-query-kind compiled query cache for symbol
extraction, caller lookup, variable lookup, ExUnit test block lookup, and
non-code range computation. The cache shares immutable `tree_sitter::Query`
values only; parsers and query cursors remain per operation.

Focused cache tests measure construction avoidance directly:

| Scenario | Before | After |
| --- | ---: | ---: |
| Two Rust symbol query requests | 2 query constructions | 1 cached query |
| Eight concurrent Rust caller query requests | Up to 8 query constructions | 1 cached query |
| Rust + Python symbol query requests | 2 query constructions | 2 cached queries |

Expected wall-clock impact is bounded because the baseline hot paths are still
dominated by file scanning and parsing. The change removes repeated query
compilation from cold extraction and first-fill caller/variable/non-code lookup;
cached code-scope grep repeats remain dominated by the existing `FileTree`
non-code range cache rather than query construction.
