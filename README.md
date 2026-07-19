# USCT

**Ultra-Speedy Cost Tracker** (`usct`) is a read-only Rust CLI that calculates the running USD cost of local AI coding-agent sessions. It discovers session files written by popular coding harnesses, normalizes their token-usage formats, applies pricing from a disk-cached [models.dev](https://models.dev/) catalog, and prints a statusline-friendly total.

USCT is designed for frequent invocation from Starship, tmux, shell prompts, editor statuslines, and monitoring scripts:

```console
$ usct
$314.23
```

No network request occurs while calculating a report. Network access is isolated to the explicit `usct update` command.

## Features

- Keeps a no-subcommand fast path that prints only the aggregate dollar total.
- Groups usage by day, Monday-starting week, month, session, or configurable billing block.
- Emits tables, compact tables, one-section JSON, or multi-section JSON from one transcript load.
- Filters by source, date, timezone, session, Claude project, and Claude instance.
- Includes per-model and optional per-agent breakdowns, ascending or descending order, and ANSI color controls.
- Projects active-block usage and token-limit utilization.
- Produces a cached hook statusline with calculated or host-reported cost and context utilization.
- Separately accounts for input, output, cache-read, cache-write, and reasoning tokens without double charging subset counters.
- Uses provider-aware model lookup, local JSON configuration, and optional custom prices.
- Supports automatic, calculated-only, and per-message reported-cost modes.
- Maintains aggregate, event-index, parser-progress, and statusline caches with topology and file fingerprint invalidation.
- Reads JSON, JSONL, and SQLite stores without mutating them.
- Performs no report-time network requests; only `usct update` downloads the pricing catalog.

## Supported harnesses

| Source | Default local storage | Environment override |
|---|---|---|
| Claude Code | `~/.claude/projects/**/*.jsonl` | `CLAUDE_CONFIG_DIR` |
| OpenAI Codex CLI | `~/.codex/sessions/**/*.jsonl` and `~/.codex/archived_sessions/**/*.jsonl` | `CODEX_HOME` |
| Pi | `~/.pi/agent/sessions/**/*.jsonl` | `PI_CODING_AGENT_SESSION_DIR` |
| Oh My Pi | `~/.omp/agent/sessions/**/*.jsonl` | `OMP_AGENT_SESSION_DIR` |
| OpenCode | `~/.local/share/opencode/` | `OPENCODE_DATA_DIR` |
| Gemini CLI | `~/.gemini/tmp/*/chats/*.{json,jsonl}` | `GEMINI_DATA_DIR` |
| Amp | `~/.local/share/amp/threads/**/*.{json,jsonl}` | `AMP_DATA_DIR` |
| Droid | `~/.factory/sessions/**/*.settings.json` | `DROID_SESSIONS_DIR` |
| Codebuff | `~/.config/manicode*/projects/**/chat-messages.json` | `CODEBUFF_DATA_DIR` |
| Hermes | `~/.hermes/**/state.db` | `HERMES_HOME` |
| Goose | platform Goose data roots containing `sessions.db` | `GOOSE_PATH_ROOT` |
| OpenClaw | `~/.openclaw/**/*.jsonl` and legacy roots | `OPENCLAW_DIR` |
| Kilo | `~/.local/share/kilo/**/kilo.db` | `KILO_DATA_DIR` |
| Kimi | `~/.kimi/**/wire.jsonl`, `~/.kimi-code/**/wire.jsonl` | `KIMI_DATA_DIR` |
| Qwen | `~/.qwen/projects/*/chats/*.jsonl` | `QWEN_DATA_DIR` |
| Copilot | `~/.copilot/otel/*.jsonl` | `COPILOT_OTEL_FILE_EXPORTER_PATH` |

Claude Code transcripts under `subagents/` are excluded from automatic discovery to avoid counting delegated work twice when it is already represented in the parent session.

A provider appears in an aggregate only when USCT discovers at least one session containing token usage for it.

## Installation

### Build from source

Requirements:

- Rust 1.85 or newer with Cargo; the project uses Rust edition 2024.
- `curl` for `usct update`.
- macOS, Linux, or another platform supported by the Rust dependencies.

```bash
git clone git@github.com:alexandernicholson/usct.git
cd usct
cargo build --release
```

The optimized binary is written to:

```text
target/release/usct
```

Install it somewhere on `PATH`:

```bash
install -m 0755 target/release/usct ~/.local/bin/usct
```

Alternatively, install directly with Cargo:

```bash
cargo install --path .
```

## Quick start

Refresh the pricing catalog once:

```bash
usct update
```

Calculate the total across every discovered session and provider:

```bash
usct
```

Example:

```text
$314.23
```

Use the compact command directly in a statusline:

```bash
cost="$(usct)"
```

## Usage

```text
usct [--source SOURCE] [--period PERIOD] [--session PATH] [--format compact|json]
usct [--source SOURCE] --from DATE_OR_TIMESTAMP [--to DATE_OR_TIMESTAMP]

usct <daily|weekly|monthly|session> [OPTIONS]
usct <SOURCE> <daily|weekly|monthly|session> [OPTIONS]
usct blocks [OPTIONS]
usct claude blocks [OPTIONS]
usct statusline [OPTIONS]
usct <SOURCE> statusline [OPTIONS]

usct update
usct sources
```

### Aggregate all providers

With no options, USCT sums every billable session discovered across all supported providers:

```bash
usct
```

This is equivalent to `--source auto`:

```bash
usct --source auto
```

### Aggregate one source

A source without `--session` means every discovered session for that source:

```bash
usct --source claude
usct --source codex
usct --source omp
```

For example, `usct --source omp` includes every OMP transcript under `~/.omp/agent/sessions`, not merely the most recently modified session.

### Select a time period

`--period` limits usage to a named local-time window:

```bash
usct --period session
usct --period hour
usct --period day
usct --period week
usct --period month
usct --period year
```

| Period | Meaning |
|---|---|
| `all` | All discovered usage; this is the default. |
| `session` | The most recently modified session in the selected source scope. |
| `hour` | Usage since the beginning of the current local clock hour. |
| `day` | Usage since local midnight. |
| `week` | Usage since Monday at local midnight. |
| `month` | Usage since the first day of the local calendar month. |
| `year` | Usage since January 1 in the local timezone. |

Time periods combine with source selection:

```bash
usct --source claude --period day
usct --source codex --period month
usct --source omp --period session
```

### Select a custom range

Use inclusive `--from` and optional exclusive `--to` boundaries:

```bash
usct --from 2026-07-01 --to 2026-08-01
usct --source omp --from 2026-07-12T09:00:00 --to 2026-07-12T17:00:00
usct --from 2026-07-12T00:00:00Z --to 2026-07-13T00:00:00Z
```

Accepted boundary formats:

- `YYYY-MM-DD`, interpreted as local midnight;
- `YYYY-MM-DDTHH:MM:SS`, interpreted in the local timezone;
- RFC 3339 timestamps with an explicit offset or `Z`.

Omitting `--to` creates an open-ended range. `--from` cannot be combined with a named period other than the default `all`.

### Price one explicit session

`--session` changes the scope to exactly one transcript:

```bash
usct --source omp --session ~/.omp/agent/sessions/project/session.jsonl
```

The source can be inferred from a conventional path when omitted:

```bash
usct --session ~/.codex/sessions/2026/07/12/rollout-example.jsonl
```

Supplying `--source` explicitly is recommended for nonstandard paths.

### JSON output

```bash
usct --format json
```

Example:

```json
{
  "cost_usd": 309.32652670000004,
  "session_count": 25,
  "sources": ["claude", "codex", "omp"],
  "range": {
    "label": "day",
    "from": "2026-07-11T15:00:00+00:00",
    "to": null
  },
  "tokens": {
    "cache_read": 281430267,
    "cache_write": 2540859,
    "input": 1407524,
    "output": 679412,
    "reasoning": 2301
  }
}
```

Fields:

- `cost_usd`: aggregate calculated cost in US dollars.
- `session_count`: sessions containing billable token usage.
- `sources`: sources represented in the total.
- `range`: effective UTC-normalized range metadata, or `null` for all-time and session reports.
- `tokens.input`: ordinary, non-cached input tokens where the source distinguishes them.
- `tokens.output`: output tokens excluding separately reported reasoning tokens.
- `tokens.cache_read`: tokens served from a provider cache.
- `tokens.cache_write`: tokens written to a provider cache.
- `tokens.reasoning`: separately reported reasoning or thought tokens.

### Grouped reports

Grouped commands use event-level accounting while the no-subcommand aggregate retains its smaller fast path:

```bash
usct daily
usct weekly --since 2026-07-01 --until 2026-07-18
usct monthly --timezone America/Los_Angeles
usct session --json
usct codex daily --speed fast
usct claude daily --project my-project --instances
```

`--since` and `--until` accept `YYYY-MM-DD` or `YYYYMMDD`; both endpoints are inclusive. `--timezone` takes an IANA timezone and controls period boundaries and labels.

Source names can lead a grouped command, as in `usct kimi monthly`. Without a source prefix, all detected sources are included. Useful output controls:

- `--json` or `--format json` emits the stable grouped JSON contract.
- `--sections daily,weekly,monthly,session` emits several JSON arrays after loading transcripts once.
- `--no-cost` omits pricing work and cost fields.
- `--compact` selects the narrow table layout.
- `--breakdown` adds model rows to tables; JSON always includes `modelBreakdowns`.
- `--by-agent` adds `agentBreakdowns` to JSON rows.
- `--order asc|desc` controls period order.
- `--color` and `--no-color` override terminal color detection.
- `--mode auto|calculate|display` selects reported or locally calculated event costs.
- `--single-thread` disables parallel cold parsing; warm event-index reads are unchanged.
- `--debug --debug-samples N` writes bounded cost-source and price-resolution samples to standard error.
- `--id`, `--project`, and `--instance` filter transcript paths; `--project-aliases old=new,...` controls displayed Claude instance names.

Each JSON row contains `period`, `agent`, token counters, `totalTokens`, `modelsUsed`, `modelBreakdowns`, and source-specific `metadata`. Priced reports also contain `totalCost`. A top-level `totals` object covers the emitted rows.

### Billing blocks

`blocks` groups Claude usage into rolling five-hour windows by default:

```bash
usct blocks
usct blocks --active --token-limit 200000
usct blocks --recent --order desc --breakdown
usct blocks --session-length 8 --json
```

Active rows include end-of-block token and cost projections. `--token-limit max` uses the built-in maximum limit, while a numeric value reports projected utilization.

### Configuration and custom prices

Defaults are loaded from `$XDG_CONFIG_HOME/usct/config.json`, `~/.config/usct/config.json`, or the fallback `~/.usct.json`. `USCT_CONFIG` changes the default path; `--config PATH` selects an explicit file. Command-line options take precedence.

```json
{
  "source": "claude",
  "timezone": "UTC",
  "format": "json",
  "sections": ["monthly", "session"],
  "mode": "auto",
  "order": "desc",
  "noCost": false,
  "compact": false,
  "byAgent": false,
  "breakdown": true,
  "singleThread": false,
  "sessionLengthHours": 5,
  "tokenLimit": "200000",
  "costSource": "reported",
  "refreshInterval": 1,
  "cache": true,
  "prices": {
    "claude-custom": {
      "input": 3.0,
      "output": 15.0,
      "cache_read": 0.3,
      "cache_write": 3.75
    }
  }
}
```

Custom prices are USD per million tokens and allow fully local priced reports even when a model is absent from the catalog.

Shared report defaults include `source`, `timezone`, `format`, `json`, `sections`, `since`, `until`, `order`, `mode`, `compact`, `noCost`, `byAgent`, `breakdown`, `instances`, `sessionId`, `project`, `instance`, `projectAliases`, `speed`, `color`, `noColor`, `debug`, `debugSamples`, and `singleThread`. Billing-block defaults add `active`, `recent`, `tokenLimit`, and `sessionLengthHours`. Statusline defaults add `visualBurnRate`, `costSource`, `refreshInterval`, `contextLowThreshold`, `contextMediumThreshold`, and `cache`. A top-level `$schema` string is accepted for editor integration.

### Show resolved source roots

```bash
usct sources
```

This prints JSON containing every supported source and its resolved storage roots, including environment overrides.

## Pricing catalog

USCT obtains model pricing from:

```text
https://models.dev/api.json
```

Refresh it explicitly:

```bash
usct update
```

The update operation:

1. downloads the models.dev API response with `curl`;
2. rejects HTTP failures;
3. validates the JSON against USCT's required pricing representation;
4. removes metadata that is unnecessary during cost calculation;
5. writes a temporary file and synchronizes it;
6. atomically replaces the previous catalog.

The default catalog path is:

```text
${XDG_CACHE_HOME:-$HOME/.cache}/usct/models.json
```

Override it with:

```bash
USCT_MODELS_PATH=/path/to/models.json usct
```

The ordinary reporting path is offline. If the catalog is absent or invalid, USCT exits nonzero and instructs you to run `usct update`.

## Cost calculation

### Cost source modes

Grouped reports and billing blocks accept `--mode`:

- `auto` uses a nonnegative per-message reported cost when the source supplies one, otherwise it calculates from token counts and local prices.
- `calculate` ignores reported costs and always applies configured or catalog prices.
- `display` uses only per-message reported costs; events without one contribute zero cost and do not require a pricing catalog.

`--no-cost` skips both reported-cost aggregation and pricing work.

models.dev prices are denominated in USD per one million tokens. USCT calculates:

```text
(input × input_price
 + output × output_price
 + cache_read × cache_read_price
 + cache_write × cache_write_price
 + reasoning × reasoning_price)
 / 1,000,000
```

USCT keeps a separate usage bucket for every model observed in a session, applies that model's price to its bucket, and sums the resulting costs. A mid-session model change therefore affects only usage recorded after the change.

When an optional models.dev price is absent:

- cache-read tokens use the ordinary input price;
- cache-write tokens use the ordinary input price;
- reasoning tokens use the ordinary output price.

### Avoiding double charging

Token schemas differ between harnesses:

- Some report cached tokens separately from ordinary input.
- Others report cached tokens as a subset of aggregate input.
- Some report reasoning tokens as a subset of output.
- Codex records cumulative snapshots rather than independent usage events.

USCT normalizes these differences before pricing:

- Codex converts each cumulative `total_token_usage` snapshot into a delta and attributes that delta to the model active for the interval.
- Cached input is subtracted from aggregate input only for formats where it is a subset.
- Reasoning is subtracted from output when separately reported.
- Claude assistant messages are deduplicated by message ID.
- Claude `<synthetic>` assistant records are client-generated placeholders and are excluded from usage and model selection.
- Distinct OMP messages with identical usage values remain distinct and are both counted.

### Provider-aware model lookup

The same bare model ID can appear under multiple models.dev providers. USCT uses qualified provider IDs whenever the harness or model prefix establishes the provider:

- `claude-*` → Anthropic
- `gpt-*` and `o*` → OpenAI
- `gemini-*` → Google
- Claude Code defaults to Anthropic when the model name is otherwise ambiguous.
- Codex and OMP Codex sessions default to OpenAI.
- Gemini CLI defaults to Google.

A provider-qualified model ID in a transcript takes precedence over inference.
OMP's `openai-codex` provider ID is matched to the OpenAI catalog. If an OMP `-sol` model variant has no exact catalog entry, USCT uses the corresponding base model; exact entries always take precedence.
Likewise, a model revision ending in a valid `YYYY-MM-DD` date uses the corresponding undated catalog entry when no exact dated entry exists.

## Cache behavior

USCT maintains five cache forms beside `models.json`:

1. **Parser progress and per-session reports** retain normalized usage, resolved prices, and append-only JSONL state.
2. **Scope aggregates** retain all-provider, one-source, named-period, and explicit-session totals.
3. **Binary event indexes** retain timestamped per-file usage and per-message reported costs for grouped and billing-block reports.
4. **Binary report caches** retain dependency-validated grouped data and rendered output, so repeated formats return without rebuilding reports.
5. **Statusline state** retains one rendered hook result for its configured refresh interval.

Cache validation includes:

- session path;
- byte length;
- nanosecond modification time;
- relevant provider-root and parent-directory metadata;
- pricing-catalog metadata;
- cache schema version.

This lets USCT bypass recursive discovery on an unchanged warm run while still invalidating when:

- a session is appended;
- a session is replaced or removed;
- a new session appears in an existing project;
- a new project directory appears;
- models.dev prices are refreshed.

When one transcript changes, unchanged transcript contributions are reused directly from the aggregate state.

Aggregate states retain every session's normalized per-model contributions, resolved model prices, fingerprint, and parser progress. When one session changes, USCT subtracts its prior contribution and adds the updated contribution; unchanged sessions are reused directly from the aggregate state. Ordinary appends therefore avoid recursive discovery, per-session cache reads, and models.dev JSON decoding.

For append-only JSONL transcripts, USCT also persists source-specific parser progress:

- the byte offset of the last complete JSONL record;
- a hash of the trailing parsed bytes;
- filesystem identity for replacement detection;
- accumulated normalized per-model usage;
- active-model and message-ID deduplication state;
- the previous Codex cumulative snapshot;
- effective time-range boundaries.

When an active harness appends a record, USCT seeks directly to the prior byte offset and parses only complete newly appended records. A partial trailing JSONL record is deferred until a later append supplies its terminating newline. Truncation, replacement, changed file identity, or a mismatching tail hash triggers a safe full rebuild.

Plain JSON and SQLite stores retain the safe full-parser fallback because they do not provide an append-only JSONL boundary.

## Performance

Measurements were taken on macOS arm64 against the optimized release binary with Hyperfine 1.20. Warm commands use `--shell=none --warmup 20 --runs 200`; medians and p95 values are calculated from Hyperfine's exported per-run timings. Forced full rebuilds use 50 isolated temporary session and catalog paths per provider so an existing cache cannot turn a rebuild into a cache hit.

The repository includes a hermetic benchmark suite:

```bash
benches/hyperfine.sh
```

It benchmarks the bare dollar total, named-period aggregate, help, daily table and JSON reports, four-section JSON, billing blocks, and cached statusline. Every measured command uses `--shell=none`; Hyperfine supplies statusline stdin with `--input`.

Set `USCT_BASELINE_BIN` to compare the bare command against another binary. The script fails when the candidate median exceeds the baseline by more than 5% by default:

```bash
USCT_BASELINE_BIN=/path/to/baseline benches/hyperfine.sh
```

`USCT_BENCH_BIN`, `USCT_BENCH_WARMUP`, `USCT_BENCH_RUNS`, and `USCT_MAX_REGRESSION` control the candidate, sample counts, and regression ceiling.

### Standalone process floor

| Command | Runs | Median | p95 |
|---|---:|---:|---:|
| `/usr/bin/true` | 200 | 0.862 ms | 1.121 ms |
| `usct --help` | 200 | 1.473 ms | 1.757 ms |

Darwin process creation consumes most of a warm USCT invocation. The no-subcommand path does not initialize timezone grouping; grouped commands resolve the system IANA timezone only when `--timezone` is omitted.

### Warm reports

| Command | Runs | Median | p95 |
|---|---:|---:|---:|
| `usct --period day` | 200 | **1.721 ms** | 2.476 ms |
| `usct --source omp --period day` | 200 | 1.758 ms | 2.201 ms |
| `usct --source omp --session <active.jsonl>` | 200 | 1.567 ms | 2.039 ms |

These paths load a valid aggregate or session contribution and do not reparse transcript history.

### Forced full rebuilds

Each benchmark uses the largest local transcript available for that provider at measurement time. Before every invocation Hyperfine copies the transcript and catalog into a fresh temporary cache root, forcing transcript parsing, models.dev decoding, pricing, and an atomic cache write.

| Provider | Bytes | Records | Runs | Median | p95 | Minimum | Maximum |
|---|---:|---:|---:|---:|---:|---:|---:|
| OMP | 44,768,149 | 7,785 | 50 | **33.430 ms** | 37.240 ms | 31.172 ms | 40.156 ms |
| Claude Code | 8,057,560 | 3,031 | 50 | **12.739 ms** | 13.771 ms | 10.837 ms | 15.234 ms |
| Codex CLI | 10,859,972 | 5,609 | 50 | **14.044 ms** | 15.277 ms | 12.375 ms | 16.524 ms |

Borrowed provider envelopes extract only accounting fields and ask Serde to skip `content`, tool arguments, tool results, generated text, and unrelated metadata instead of constructing a recursive `serde_json::Value` tree for every known record.

Claude preserves exact message-ID deduplication. Codex preserves cumulative token snapshots, pre-range baselines, cached-input normalization, and reasoning-token separation. OMP preserves nested `message.details.response.usage` accounting. Unknown record types containing usage fields fall back to recursive `Value` traversal for schema compatibility.


### Live aggregate state

The named-period all-provider aggregate measured **1.721 ms median** and 2.476 ms p95 over 200 warm invocations. Its aggregate state retains normalized contributions, resolved prices, transcript fingerprints, directory topology, and parser progress. A normal append avoids recursive discovery, unchanged session reads, and models.dev decoding, then performs one atomic aggregate-state write.

## Statusline integration

### Hook statusline

The `statusline` command reads hook JSON from standard input. It uses `transcript_path`, calculates current session usage, and adds context utilization when the hook provides `context_window` data:

```bash
usct statusline --cost-source both --visual-burn-rate text
usct omp statusline --cost-source both --visual-burn-rate text
```

A source prefix forces the corresponding transcript parser and pricing rules; without one, USCT infers the source from `transcript_path`. If the hook supplies no path, the unprefixed command retains its Claude-compatible fallback.

OMP does not execute Claude's external statusline-command protocol directly. The bundled OMP extension passes the current session path and context percentage to `usct omp statusline`, then renders cost, tokens, context pressure, and burn rate with the active OMP theme.

```bash
mkdir -p ~/.omp/agent/extensions
install -m 644 integrations/omp/usct-statusline.ts ~/.omp/agent/extensions/usct-statusline.ts
```

Run `/reload` or restart OMP after installation. Set `USCT_BIN` if `usct` is not available on OMP's `PATH`.

`--cost-source` selects calculated, host-reported, or combined cost. `--visual-burn-rate` accepts `off`, `text`, `emoji`, or `emoji-text`; threshold options control the low, medium, and high context bands. The one-line result uses a hybrid time-and-file cache, so an append invalidates it even before `--refresh-interval` expires. Use `--no-cache` for an unconditional refresh.

### Starship

A minimal custom module:

```toml
[custom.usct]
command = "usct"
when = "test -f \"${USCT_MODELS_PATH:-$HOME/.cache/usct/models.json}\""
format = "[$output]($style) "
style = "bold yellow"
```

Starship may suppress rendering when `TERM=dumb`; that behavior comes from Starship, not USCT.

### tmux

```tmux
set -g status-right '#(usct --period day) %H:%M'
```

Choose a tmux status interval appropriate for how frequently the underlying harness writes usage:

```tmux
set -g status-interval 2
```

### Shell prompt

```bash
PROMPT='$(usct) %~ %# '
```

Frequent command substitution still pays the operating system's process-launch cost even when USCT's report is cached.

## Environment variables

| Variable | Purpose |
|---|---|
| `USCT_MODELS_PATH` | Override the models.dev pricing-cache file. |
| `XDG_CACHE_HOME` | Change the base cache directory when `USCT_MODELS_PATH` is unset. |
| `CLAUDE_CONFIG_DIR` | Override one or more Claude Code configuration roots. |
| `CODEX_HOME` | Override one or more Codex home directories. |
| `PI_CODING_AGENT_SESSION_DIR` | Override Pi's session directory. |
| `OMP_AGENT_SESSION_DIR` | Override Oh My Pi's session directory. |
| `OPENCODE_DATA_DIR` | Override OpenCode's data directory. |
| `GEMINI_DATA_DIR` | Override Gemini CLI's data directory. |
| `AMP_DATA_DIR` | Override Amp's data directory. |
| `DROID_SESSIONS_DIR` | Override Droid's sessions directory. |
| `CODEBUFF_DATA_DIR` | Override one or more Codebuff data roots. |
| `HERMES_HOME` | Override one or more Hermes roots. |
| `GOOSE_PATH_ROOT` | Override one or more Goose data roots. |
| `OPENCLAW_DIR` | Override one or more OpenClaw roots. |
| `KILO_DATA_DIR` | Override one or more Kilo data roots. |
| `KIMI_DATA_DIR` | Override one or more Kimi roots. |
| `QWEN_DATA_DIR` | Override one or more Qwen roots. |
| `COPILOT_OTEL_FILE_EXPORTER_PATH` | Add an explicit Copilot OTEL JSONL path. |
| `USCT_CONFIG` | Override the default JSON configuration path. |
| `FORCE_COLOR` / `NO_COLOR` | Force or disable ANSI table color. |

Path-list overrides use the platform's normal path-list separator.

## Exit behavior

USCT writes a successful report to standard output and exits `0`.

Errors are written to standard error with a nonzero exit code:

```text
usct: no supported coding-agent session found
```

Common errors include:

- pricing cache missing or malformed;
- no sessions found for a selected fast aggregate scope (grouped reports instead return empty rows and zero totals);
- a transcript containing no usable model identifier;
- a model absent from the pricing catalog;
- malformed JSON or an unreadable SQLite database;
- `curl` unavailable or models.dev returning an HTTP error during `update`.

## Troubleshooting

### `zsh: command not found: bat` when running `cat README.md`

This means your interactive shell aliases `cat` to `bat`, but `bat` is not installed. It is unrelated to USCT. Bypass the alias with:

```bash
command cat README.md
```

or install `bat`.

### Starship reports `TERM=dumb`

Starship intentionally refuses to render under a dumb terminal. Run USCT directly:

```bash
./target/release/usct
```

or execute the command from a normal interactive terminal with an appropriate `TERM` value.

### A source reports no sessions

Inspect the resolved roots:

```bash
usct sources
```

Then verify the corresponding environment override and session directory.

### The total changed after an update

Pricing comes from the cached models.dev catalog. `usct update` can change historical calculated totals when models.dev changes a model's listed price. The report cache is invalidated automatically when `models.json` changes.

### The total increases while testing USCT

If development is occurring inside a supported coding harness, that harness is actively appending token usage to its transcript. The calculated running total can therefore increase between consecutive commands.

## Architecture

```text
CLI and rendering
       │
       ├── compact aggregate path
       │      ├── incremental session parsers
       │      └── aggregate/session caches
       │
       └── event report path
              ├── source adapters and event indexes
              ├── timezone grouping and filters
              └── tables, JSON, blocks, statusline
                         │
                         ▼
                token and pricing domain
```

Source layout:

| Path | Responsibility |
|---|---|
| `src/main.rs` | CLI routing, option layering, aggregate rendering, and hook statusline. |
| `src/app.rs` | Provider-aware session pricing and aggregate domain operations. |
| `src/domain.rs` | Token usage, model price, and cost arithmetic. |
| `src/catalog.rs` | models.dev parsing, compaction, and lookup. |
| `src/config.rs` | JSON configuration discovery and decoding. |
| `src/discovery.rs` | Source roots, candidate filtering, and session discovery. |
| `src/session.rs` | JSON, JSONL, OTEL, and source-specific SQLite normalization. |
| `src/report.rs` | Event indexes, grouping, JSON/table output, and billing blocks. |
| `src/cache.rs` | Pricing updates, fingerprints, parser progress, and aggregate caches. |
| `tests/contracts.rs` | Behavioral, adapter, cache, and CLI contract tests. |

The domain layer does not perform filesystem, network, CLI, or rendering work. Source adapters normalize external formats into the same `TokenUsage` representation.

## Development

Run the tests:

```bash
cargo test
```

Run strict Clippy checks:

```bash
cargo clippy --all-targets -- -D warnings
```

Check formatting:

```bash
cargo fmt --check
```

Build the optimized binary:

```bash
cargo build --release
```

The test suite covers:

- independent token-class arithmetic and subset normalization;
- models.dev lookup and source-aware provider disambiguation;
- Claude message deduplication and Codex cumulative snapshots;
- OMP equal-usage messages and generic schema fallback;
- native JSON adapters and structured SQLite adapters;
- timezone/date grouping, inclusive filters, and multi-section JSON;
- grouped event-cache append invalidation;
- offline custom pricing and Codex fast-tier pricing;
- hook statusline options;
- cross-provider aggregate and explicit-session output.

## Data and privacy

USCT reads local transcripts but does not upload them. Normal reports use only the local pricing cache and local session files. `usct update` downloads public pricing data from models.dev and does not send transcript contents.

Coding-agent transcripts may contain prompts, source code, tool output, file paths, and secrets. Protect the underlying session directories and do not publish them as debugging fixtures without redaction.

## License

USCT is distributed under the MIT license, as declared in `Cargo.toml`.
