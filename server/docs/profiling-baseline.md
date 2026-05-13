# CodeRLM Profiling Baseline

Date: 2026-05-12
Baseline host: Linux workstation, release build, `/usr/bin/time -v`, `curl`, `jq`
Repository: `/home/xpatel/Development/coderlm/.journey/worktrees/issue-11`

This baseline is measurement-first. It records reproducible CPU, wall-time, and
RSS commands plus current results for cold indexing, warm queries, watcher
updates, and common API operations. It does not include performance fixes.

## Prerequisites

Build the optimized server once before measuring:

```bash
cd server
cargo build --release
```

Optional tools:

- `/usr/bin/time -v`: captures CPU percentage and peak RSS.
- `hyperfine`: optional replacement for repeated wall-time samples.
- `perf`: optional CPU hotspot sampling on Linux.
- `cargo flamegraph`: optional flamegraph wrapper when installed.

Tools available in this baseline run:

- `/usr/bin/time -v`: available.
- `perf`: available.
- `hyperfine`: not installed.
- `cargo flamegraph`: not installed.

## Cold Indexing

Command:

```bash
PORT=31312
ROOT="$(pwd)/.."
/usr/bin/time -v timeout --signal=INT 2s \
  ./target/release/coderlm-server serve --port "$PORT" "$ROOT"
```

Captured result:

| Metric | Value |
| --- | ---: |
| Indexed files | 55 |
| Extracted symbols | 433 |
| Wall time until first populated symbol table | 43 ms |
| Session creation against pre-indexed project | 15 ms |
| Timed process wall time | 2.01 s |
| User CPU | 0.75 s |
| System CPU | 0.04 s |
| CPU utilization during timed window | 39% |
| Peak RSS | 17,560 KB |

Notes:

- The server pre-indexes files synchronously and spawns symbol extraction in the
  background. The HTTP listener is available before extraction completes.
- The 2 second timed window includes idle server time after indexing so CPU
  percentage is lower than the burst CPU used during extraction.

## Warm Session Creation

Command:

```bash
ROOT=/path/to/coderlm
SID=$(
  curl -fsS -X POST "http://127.0.0.1:31313/api/v1/sessions" \
    -H 'Content-Type: application/json' \
    -d "{\"cwd\":\"$ROOT\"}" | jq -r .session_id
)
curl -fsS "http://127.0.0.1:31313/api/v1/roots" |
  jq --arg root "$ROOT" '.roots[] | select(.path==$root)'
```

Captured result:

| Metric | Value |
| --- | ---: |
| Session creation latency | 15 ms |
| File count after warm session | 55 |
| Symbol count after warm session | 429-433 |
| Observed extraction repeat | No full rescan; reused existing project |

The small symbol-count variation came from watcher activity while profiling.
The warm-session path returned the already indexed project rather than running
`walker::scan_directory` again.

## Warm API Queries

Command pattern:

```bash
curl -fsS -w '\n%{time_total}' \
  -H "X-Session-Id: $SID" \
  "http://127.0.0.1:31313/api/v1/<endpoint>"
```

Captured result:

| Operation | Endpoint | Wall time |
| --- | --- | ---: |
| Symbols | `/api/v1/symbols?limit=500` | 4.014 ms |
| Search | `/api/v1/symbols/search?q=extract_symbols&limit=20` | 1.535 ms |
| Implementation | `/api/v1/symbols/implementation?symbol=extract_symbols_from_file&file=server/src/symbols/parser.rs` | 0.896 ms |
| Callers, common symbol | `/api/v1/symbols/callers?symbol=insert&file=server/src/symbols/mod.rs&limit=50` | 159.873 ms |
| Callers, rare symbol | `/api/v1/symbols/callers?symbol=WatcherHandle&file=server/src/index/watcher.rs&limit=50` | 44.275 ms |
| Grep, all scope | `/api/v1/grep?pattern=extract_symbols&max_matches=100&scope=all` | 2.800 ms |
| Grep, code scope | `/api/v1/grep?pattern=extract_symbols&max_matches=100&scope=code` | 169.343 ms |
| Structure | `/api/v1/structure?depth=3` | 0.957 ms |
| Server RSS after warm queries | `ps -o rss= -p "$PID"` | 27,776 KB |

## Repeated Code-Scope Grep

Command:

```bash
for n in 1 2 3 4 5; do
  curl -fsS -o /tmp/coderlm-grep-code-$n.json \
    -w "code_grep_run_$n time_s=%{time_total}\n" \
    -H "X-Session-Id: $SID" \
    "http://127.0.0.1:31313/api/v1/grep?pattern=extract_symbols&max_matches=100&scope=code"
done
```

Captured result:

| Run | Wall time |
| --- | ---: |
| 1 | 177.710 ms |
| 2 | 197.475 ms |
| 3 | 184.121 ms |
| 4 | 177.656 ms |
| 5 | 171.530 ms |

Observation: repeated identical code-scope grep remained close to the original
latency. This suggests comment/string exclusion ranges are recomputed for each
request rather than cached.

## Watcher Updates

Command outline:

```bash
TMPROOT=$(mktemp -d /tmp/coderlm-watch.XXXXXX)
mkdir -p "$TMPROOT/src" "$TMPROOT/target/huge" "$TMPROOT/docs"
printf 'pub fn baseline() -> usize { 1 }\nfn main() { let _ = baseline(); }\n' \
  > "$TMPROOT/src/main.rs"
python3 - <<'PY' "$TMPROOT/target/huge/generated.rs" "$TMPROOT/docs/notes.txt"
import sys
open(sys.argv[1], "w").write("pub fn ignored_generated() {}\n" * 5000)
open(sys.argv[2], "w").write("unsupported notes\n" * 1000)
PY

curl -fsS -X POST "http://127.0.0.1:31313/api/v1/sessions" \
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
| Single supported-file edit visible in `/roots` | 523 ms |
| Burst save of 5 supported files visible in `/roots` | 589 ms |
| Final temp-project files | 7 |
| Final temp-project symbols | 8 |
| Large ignored `target/` matches | 0 |
| Unsupported `.txt` grep matches | 1000 |

Observation: watcher latency matches the 500 ms debouncer in
`server/src/index/watcher.rs`. Files under ignored directories are skipped.
Unsupported files are indexed for content grep but do not produce tree-sitter
symbols.

## Top Suspects

1. Code-scope grep CPU: `server/src/ops/content.rs` recomputes tree-sitter
   comment/string ranges for each supported file on every `scope=code` request.
   Target: repeated identical code-scope grep should drop from roughly
   170-200 ms to less than 30 ms on this repo after caching or reusing ranges.
2. Caller lookup CPU: `server/src/ops/symbol_ops.rs` scans indexed files and
   reparses AST-supported files that contain the symbol text. Target: common
   caller query should drop from roughly 160 ms to less than 50 ms on this repo
   by indexing call sites or caching parsed caller data.
3. Symbol extraction memory/CPU: `server/src/symbols/parser.rs` creates a new
   parser and query per file during cold extraction. Target: keep cold-index RSS
   under 20 MB on this repo while reducing extraction CPU by at least 25%.
4. Watcher reparse churn: `server/src/index/watcher.rs` reparses every changed
   supported file after debounce. Target: preserve the roughly 500-600 ms
   visible update latency while coalescing duplicate burst events so a file is
   reparsed at most once per debounce window.

## Follow-Up Optimization Targets

- Cache non-code byte ranges per file and invalidate them from watcher updates.
  Success metric: five repeated `grep --scope code` runs average less than
  30 ms on this repository.
- Add a call-site index or per-file caller cache keyed by file mtime/content.
  Success metric: `callers insert` averages less than 50 ms and rare-symbol
  callers average less than 20 ms on this repository.
- Reuse or precompile language query objects where tree-sitter allows it.
  Success metric: cold extraction CPU drops by at least 25% with peak RSS not
  exceeding the 17,560 KB baseline by more than 10%.
- Add debug or metrics counters for watcher events and reparses.
  Success metric: single-file and burst-save baselines report event count,
  reparse count, and latency without relying on indirect `/roots` polling.
