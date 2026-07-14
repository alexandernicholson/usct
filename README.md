# USCT

**Ultra-Speedy Cost Tracker** (`usct`) is a read-only Rust CLI that calculates the running USD cost of local AI coding-agent sessions. It discovers session files written by popular coding harnesses, normalizes their token-usage formats, applies pricing from a disk-cached [models.dev](https://models.dev/) catalog, and prints a statusline-friendly total.

USCT is designed for frequent invocation from Starship, tmux, shell prompts, editor statuslines, and monitoring scripts:

```console
$ usct
$314.23
```

No network request occurs while calculating a report. Network access is isolated to the explicit `usct update` command.

## Features

- Aggregates all local sessions across all available supported providers by default.
- Restricts aggregation to one harness with `--source`.
- Prices one transcript with `--session`.
- Separately accounts for input, output, cache-read, cache-write, and reasoning tokens.
- Avoids double charging when cached or reasoning tokens are subsets of broader counters.
- Uses provider-aware model lookup to disambiguate identical model IDs.
- Reads OpenCode SQLite storage in read-only mode.
- Maintains aggregate and per-session caches so unchanged transcripts are not reparsed.
- Detects appended, replaced, added, and removed sessions through file and directory fingerprints.
- Emits compact statusline output or structured JSON.
- Atomically refreshes and compacts the models.dev pricing catalog.

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
usct [--source auto|claude|codex|pi|omp|opencode|gemini|amp]
     [--period all|session|hour|day|week|month|year]
     [--session PATH]
     [--format compact|json]

usct [--source SOURCE]
     --from DATE_OR_TIMESTAMP
     [--to DATE_OR_TIMESTAMP]
     [--format compact|json]

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

models.dev prices are denominated in USD per one million tokens. USCT calculates:

```text
(input × input_price
 + output × output_price
 + cache_read × cache_read_price
 + cache_write × cache_write_price
 + reasoning × reasoning_price)
 / 1,000,000
```

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

- Codex uses the final cumulative `total_token_usage` snapshot in a transcript.
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

USCT maintains two cache layers beside `models.json`:

1. **Per-session reports** store the normalized token usage and calculated cost for an unchanged transcript.
2. **Scope aggregates** store the rendered inputs for scopes such as all providers, one source, or an explicit session.

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

Aggregate states retain every session's normalized contribution, resolved model price, fingerprint, and parser progress. When one session changes, USCT subtracts its prior contribution and adds the updated contribution; unchanged sessions are reused directly from the aggregate state. Ordinary appends therefore avoid recursive discovery, per-session cache reads, and models.dev JSON decoding.

For append-only JSONL transcripts, USCT also persists source-specific parser progress:

- the byte offset of the last complete JSONL record;
- a hash of the trailing parsed bytes;
- filesystem identity for replacement detection;
- accumulated normalized usage;
- model and message-ID deduplication state;
- the Codex cumulative baseline;
- effective time-range boundaries.

When an active harness appends a record, USCT seeks directly to the prior byte offset and parses only complete newly appended records. A partial trailing JSONL record is deferred until a later append supplies its terminating newline. Truncation, replacement, changed file identity, or a mismatching tail hash triggers a safe full rebuild.

Plain JSON and SQLite stores retain the safe full-parser fallback because they do not provide an append-only JSONL boundary.

## Performance

Measurements were taken on macOS arm64 against the optimized release binary. Warm commands use 200 separate process invocations. Full-rebuild and incremental-append measurements use isolated temporary session and catalog paths so an existing cache cannot turn a rebuild into a cache hit.

### Standalone process floor

| Command | Runs | Median | p95 |
|---|---:|---:|---:|
| `/usr/bin/true` | 200 | 1.491 ms | 2.133 ms |
| `usct --help` | 200 | 2.907 ms | 3.275 ms |

Darwin process creation consumes most of a warm USCT invocation. The CLI's application work is approximately 1.4 ms above the measured `/usr/bin/true` median.

### Warm reports

| Command | Runs | Median | p95 |
|---|---:|---:|---:|
| `usct --period day` | 200 | 2.920 ms | 3.255 ms |
| `usct --source omp --period day` | 200 | 2.981 ms | 3.495 ms |
| `usct --source omp --session <active.jsonl>` | 200 | 3.095 ms | 3.582 ms |

These paths load a valid aggregate or session contribution and do not reparse transcript history.

### Uncached small session

Each of 50 runs used a new temporary catalog path and session path, forcing catalog decoding, session parsing, pricing, and an atomic cache write.

| Runs | Median | p95 | Minimum | Maximum |
|---:|---:|---:|---:|---:|
| 50 | 5.086 ms | 7.184 ms | 4.617 ms | 12.314 ms |

### Forced full rebuilds

Each benchmark uses a frozen copy of the largest local transcript available for that provider. Every invocation receives a new temporary transcript and catalog path, forcing transcript parsing, models.dev decoding, pricing, and an atomic cache write.

| Provider | Bytes | Records | Runs | Median | p95 | Minimum | Maximum |
|---|---:|---:|---:|---:|---:|---:|---:|
| OMP | 7,786,869 | 1,605 | 100 | **9.995 ms** | 10.778 ms | 9.371 ms | 10.978 ms |
| Claude Code | 7,077,632 | 2,667 | 100 | **9.619 ms** | 10.044 ms | 9.364 ms | 10.847 ms |
| Codex CLI | 599,348 | 168 | 100 | **5.333 ms** | 5.627 ms | 5.024 ms | 6.108 ms |

Borrowed provider envelopes extract only accounting fields and ask Serde to skip `content`, tool arguments, tool results, generated text, and unrelated metadata instead of constructing a recursive `serde_json::Value` tree for every known record.

Claude preserves exact message-ID deduplication. Codex preserves cumulative token snapshots, pre-range baselines, cached-input normalization, and reasoning-token separation. OMP preserves nested `message.details.response.usage` accounting. Unknown record types containing usage fields fall back to recursive `Value` traversal for schema compatibility.

### Incremental OMP append

The controlled append benchmark first seeds the 7,786,869-byte OMP transcript, then appends one complete usage record per isolated run.

| Runs | Median | p95 | Minimum | Maximum |
|---:|---:|---:|---:|---:|
| 100 | **3.699 ms** | 4.163 ms | 3.218 ms | 10.732 ms |

The one-time full rebuild is required after installation, cache-schema changes, replacement, or truncation. Normal append-only updates seek to the stored byte offset and decode only newly completed records. Plain JSON and SQLite sources retain their safe full-parser fallback.

### Live aggregate state

The daily all-provider command measured 2.920 ms median and 3.255 ms p95 over 200 warm invocations. Its aggregate state retains normalized contributions, resolved prices, transcript fingerprints, directory topology, and parser progress. A normal append avoids recursive discovery, unchanged session reads, and models.dev decoding, then performs one atomic aggregate-state write.

## Statusline integration

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

Path-list overrides use the platform's normal path-list separator.

## Exit behavior

USCT writes a successful report to standard output and exits `0`.

Errors are written to standard error with a nonzero exit code:

```text
usct: no supported coding-agent session found
```

Common errors include:

- pricing cache missing or malformed;
- no sessions found for a selected source;
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
       ▼
Application aggregation
       │
       ├── session discovery
       ├── source-specific parsing
       ├── models.dev pricing lookup
       └── report/session caches
                │
                ▼
        token and pricing domain
```

Source layout:

| Path | Responsibility |
|---|---|
| `src/main.rs` | CLI parsing, scope selection, cached aggregation, and rendering. |
| `src/app.rs` | Provider-aware session pricing and aggregate domain operations. |
| `src/domain.rs` | Token usage, model price, and cost arithmetic. |
| `src/catalog.rs` | models.dev parsing, compaction, and lookup. |
| `src/discovery.rs` | Source roots, candidate filtering, and session discovery. |
| `src/session.rs` | Claude, Codex, Pi, OMP, OpenCode, Gemini, and Amp parsing. |
| `src/cache.rs` | Pricing updates, fingerprints, per-session caches, and aggregate caches. |
| `tests/contracts.rs` | Behavioral and CLI contract tests. |

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

- independent token-class arithmetic;
- cached and reasoning subset normalization;
- models.dev lookup;
- source-aware provider disambiguation;
- Claude message deduplication;
- Codex cumulative snapshots;
- OMP equal-usage message handling;
- generic Pi, OpenCode, Gemini, and Amp usage objects;
- cross-provider aggregation;
- explicit-session CLI output.

## Data and privacy

USCT reads local transcripts but does not upload them. Normal reports use only the local pricing cache and local session files. `usct update` downloads public pricing data from models.dev and does not send transcript contents.

Coding-agent transcripts may contain prompts, source code, tool output, file paths, and secrets. Protect the underlying session directories and do not publish them as debugging fixtures without redaction.

## License

USCT is distributed under the MIT license, as declared in `Cargo.toml`.
