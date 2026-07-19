#!/bin/sh
set -eu

BIN=${USCT_BENCH_BIN:-target/release/usct}
WARMUP=${USCT_BENCH_WARMUP:-20}
RUNS=${USCT_BENCH_RUNS:-200}
MAX_REGRESSION=${USCT_MAX_REGRESSION:-1.05}

if [ ! -x "$BIN" ]; then
    cargo build --release
fi

fixture=$(mktemp -d "${TMPDIR:-/tmp}/usct-bench.XXXXXX")
trap 'rm -rf "$fixture"' EXIT INT TERM
mkdir -p "$fixture/claude/projects/demo" "$fixture/cache"
now=$(date -u +%Y-%m-%dT%H:%M:%SZ)
cat >"$fixture/claude/projects/demo/session.jsonl" <<JSONL
{"type":"assistant","timestamp":"$now","message":{"id":"m1","model":"claude-sonnet-4","usage":{"input_tokens":100,"output_tokens":20,"cache_read_input_tokens":30,"cache_creation_input_tokens":10}}}
{"type":"assistant","timestamp":"$now","message":{"id":"m2","model":"claude-sonnet-4","usage":{"input_tokens":200,"output_tokens":40,"cache_read_input_tokens":60,"cache_creation_input_tokens":20}}}
JSONL
cat >"$fixture/cache/models.json" <<'JSON'
{"anthropic":{"models":{"claude-sonnet-4":{"id":"claude-sonnet-4","cost":{"input":3.0,"output":15.0,"cache_read":0.3,"cache_write":3.75}}}}}
JSON
cat >"$fixture/config.json" <<'JSON'
{"noCost":true,"timezone":"UTC","prices":{"claude-sonnet-4":{"input":3.0,"output":15.0,"cache_read":0.3,"cache_write":3.75}}}
JSON
cat >"$fixture/hook.json" <<JSON
{"transcript_path":"$fixture/claude/projects/demo/session.jsonl","context_window":{"used_percentage":42}}
JSON
export HOME="$fixture"
export CLAUDE_CONFIG_DIR="$fixture/claude"
export USCT_MODELS_PATH="$fixture/cache/models.json"

# Preserve the defining fast path: a bare invocation returns only the total.
"$BIN" >/dev/null
hyperfine --shell=none --warmup "$WARMUP" --runs "$RUNS" \
    --command-name 'bare total' "$BIN" \
    --command-name 'warm day total' "$BIN --period day" \
    --command-name 'help' "$BIN --help"

if [ -n "${USCT_BASELINE_BIN:-}" ]; then
    forward_results="$fixture/forward.json"
    reverse_results="$fixture/reverse.json"
    "$USCT_BASELINE_BIN" >/dev/null
    "$BIN" >/dev/null
    hyperfine --shell=none --warmup "$WARMUP" --runs "$RUNS" \
        --export-json "$forward_results" \
        --command-name baseline "$USCT_BASELINE_BIN" \
        --command-name candidate "$BIN"
    hyperfine --shell=none --warmup "$WARMUP" --runs "$RUNS" \
        --export-json "$reverse_results" \
        --command-name candidate "$BIN" \
        --command-name baseline "$USCT_BASELINE_BIN"
    python3 - "$forward_results" "$reverse_results" "$MAX_REGRESSION" <<'PY'
import json
import statistics
import sys

def samples(path, name):
    with open(path, encoding="utf-8") as handle:
        results = json.load(handle)["results"]
    return next(result["times"] for result in results if result["command"] == name)

baseline = statistics.median(
    samples(sys.argv[1], "baseline") + samples(sys.argv[2], "baseline")
)
candidate = statistics.median(
    samples(sys.argv[1], "candidate") + samples(sys.argv[2], "candidate")
)
limit = baseline * float(sys.argv[3])
if candidate > limit:
    raise SystemExit(
        f"paired bare-command median regressed: {candidate * 1000:.3f} ms > "
        f"{limit * 1000:.3f} ms goal (baseline {baseline * 1000:.3f} ms)"
    )
print(
    f"paired bare-command goal passed: {candidate * 1000:.3f} ms <= "
    f"{limit * 1000:.3f} ms (baseline {baseline * 1000:.3f} ms)"
)
PY
fi

# Older comparison binaries may not include grouped subcommands.
if ! "$BIN" claude --help >/dev/null 2>&1; then
    printf 'skipping report benchmarks: %s lacks grouped commands\n' "$BIN" >&2
    exit 0
fi


# Prime the event index so these measure the steady-state report paths.
"$BIN" claude daily --no-cost --timezone UTC >/dev/null
hyperfine --shell=none --warmup "$WARMUP" --runs "$RUNS" \
    --command-name 'daily table' "$BIN claude daily --no-cost --timezone UTC" \
    --command-name 'daily JSON' "$BIN claude daily --json --no-cost --timezone UTC" \
    --command-name 'four JSON sections' "$BIN claude daily --json --no-cost --timezone UTC --sections daily,weekly,monthly,session" \
    --command-name 'billing blocks' "$BIN blocks --json --config $fixture/config.json --timezone UTC"

# Hyperfine supplies stdin directly; the measured statusline command still uses no shell.
hyperfine --shell=none --warmup "$WARMUP" --runs "$RUNS" \
    --input "$fixture/hook.json" \
    --command-name 'cached statusline' "$BIN statusline --config $fixture/config.json"
