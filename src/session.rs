use crate::{
    domain::{ModelUsage, TokenUsage, UsageRecord},
    time_range::{TimeRange, parse_timestamp},
};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{
    collections::{HashMap, HashSet},
    fs,
    hash::{DefaultHasher, Hash, Hasher},
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::Path,
    str::FromStr,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Harness {
    Claude,
    Codex,
    Pi,
    Omp,
    OpenCode,
    Gemini,
    Amp,
    Droid,
    Codebuff,
    Hermes,
    Goose,
    OpenClaw,
    Kilo,
    Kimi,
    Qwen,
    Copilot,
}

impl Harness {
    pub const ALL: [Self; 16] = [
        Self::Claude,
        Self::Codex,
        Self::Pi,
        Self::Omp,
        Self::OpenCode,
        Self::Gemini,
        Self::Amp,
        Self::Droid,
        Self::Codebuff,
        Self::Hermes,
        Self::Goose,
        Self::OpenClaw,
        Self::Kilo,
        Self::Kimi,
        Self::Qwen,
        Self::Copilot,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Pi => "pi",
            Self::Omp => "omp",
            Self::OpenCode => "opencode",
            Self::Gemini => "gemini",
            Self::Amp => "amp",
            Self::Droid => "droid",
            Self::Codebuff => "codebuff",
            Self::Hermes => "hermes",
            Self::Goose => "goose",
            Self::OpenClaw => "openclaw",
            Self::Kilo => "kilo",
            Self::Kimi => "kimi",
            Self::Qwen => "qwen",
            Self::Copilot => "copilot",
        }
    }
}

impl FromStr for Harness {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|item| item.name() == value)
            .ok_or_else(|| format!("unsupported source '{value}'"))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, bincode::Encode, bincode::Decode)]
pub struct UsageEvent {
    pub timestamp_ms: i64,
    pub model: String,
    pub usage: TokenUsage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reported_cost: Option<f64>,
}

#[derive(
    Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, bincode::Encode, bincode::Decode,
)]
pub struct SessionMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UsageSession {
    pub metadata: SessionMetadata,
    pub events: Vec<UsageEvent>,
}

pub fn parse_session(harness: Harness, path: &Path) -> Result<UsageRecord, String> {
    parse_session_in_range(harness, path, None)
}

pub fn parse_session_in_range(
    harness: Harness,
    path: &Path,
    range: Option<&TimeRange>,
) -> Result<UsageRecord, String> {
    if path.extension().is_some_and(|ext| ext == "db") {
        return parse_sqlite_record(harness, path, range);
    }
    match harness {
        Harness::Claude => parse_claude(path, range),
        Harness::Codex => parse_codex(path, range),
        Harness::Gemini => parse_generic(path, true, range),
        Harness::Droid
        | Harness::Codebuff
        | Harness::Hermes
        | Harness::Goose
        | Harness::OpenClaw
        | Harness::Kilo
        | Harness::Kimi
        | Harness::Qwen
        | Harness::Copilot => record_from_usage_events(harness, path, range),
        Harness::Pi | Harness::Omp | Harness::OpenCode | Harness::Amp => {
            parse_generic(path, false, range)
        }
    }
}

fn record_from_usage_events(
    harness: Harness,
    path: &Path,
    range: Option<&TimeRange>,
) -> Result<UsageRecord, String> {
    let mut models = Vec::new();
    for event in parse_usage_events(harness, path)? {
        if range.is_none_or(|range| range.contains(event.timestamp_ms)) {
            ModelUsage::add_to(&mut models, &event.model, event.usage);
        }
    }
    finish(models, TokenUsage::default())
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct UsageAccumulator {
    active_model: Option<String>,
    models: Vec<ModelUsage>,
    unattributed: TokenUsage,
}

impl UsageAccumulator {
    fn set_model(&mut self, model: &str) {
        if self.active_model.as_deref() != Some(model) {
            self.active_model = Some(model.to_owned());
        }
        if !self.unattributed.is_empty() {
            ModelUsage::add_to(
                &mut self.models,
                model,
                std::mem::take(&mut self.unattributed),
            );
        }
    }

    fn add_usage(&mut self, usage: TokenUsage) {
        if let Some(model) = self.active_model.as_deref() {
            ModelUsage::add_to(&mut self.models, model, usage);
        } else {
            self.unattributed.add_assign(usage);
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParserProgress {
    version: u8,
    harness: String,
    identity: u128,
    offset: u64,
    tail_hash: u64,
    attribution: UsageAccumulator,
    seen_ids: HashSet<String>,
    codex_previous: TokenUsage,
    range_start_ms: Option<i64>,
    range_end_ms: Option<i64>,
}

#[derive(Deserialize)]
struct JsonlEnvelope<'a> {
    #[serde(rename = "type")]
    kind: Option<&'a str>,
    #[serde(borrow)]
    message: Option<JsonlMessage<'a>>,
    #[serde(alias = "model_id", alias = "modelID")]
    model: Option<&'a str>,
    #[serde(alias = "created_at", alias = "createdAt", alias = "time")]
    timestamp: Option<JsonTimestamp<'a>>,
}

#[derive(Deserialize)]
struct JsonlMessage<'a> {
    id: Option<&'a str>,
    #[serde(alias = "model_id", alias = "modelID")]
    model: Option<&'a str>,
    #[serde(borrow)]
    details: Option<JsonlDetails<'a>>,
    usage: Option<JsonUsage>,
    #[serde(alias = "created_at", alias = "createdAt", alias = "time")]
    timestamp: Option<JsonTimestamp<'a>>,
}

#[derive(Deserialize)]
struct JsonlDetails<'a> {
    #[serde(borrow)]
    response: Option<JsonlResponse<'a>>,
}

#[derive(Deserialize)]
struct JsonlResponse<'a> {
    model: Option<&'a str>,
    usage: Option<JsonUsage>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum JsonTimestamp<'a> {
    Text(&'a str),
    Signed(i64),
    Unsigned(u64),
}

impl JsonTimestamp<'_> {
    fn milliseconds(&self) -> Option<i64> {
        match self {
            Self::Text(value) => parse_timestamp(value),
            Self::Signed(value) => Some(timestamp_number(*value)),
            Self::Unsigned(value) => i64::try_from(*value).ok().map(timestamp_number),
        }
    }
}

#[derive(Default, Deserialize)]
struct JsonUsage {
    #[serde(
        default,
        alias = "input_tokens",
        alias = "inputTokens",
        alias = "prompt_tokens",
        alias = "promptTokenCount"
    )]
    input: u64,
    #[serde(
        default,
        alias = "output_tokens",
        alias = "outputTokens",
        alias = "completion_tokens",
        alias = "candidatesTokenCount"
    )]
    output: u64,
    #[serde(
        default,
        rename = "cacheRead",
        alias = "cache_read_input_tokens",
        alias = "cached_input_tokens",
        alias = "cacheReadInputTokens",
        alias = "cachedContentTokenCount"
    )]
    cache_read: u64,
    #[serde(
        default,
        rename = "cacheWrite",
        alias = "cache_creation_input_tokens",
        alias = "cache_write_input_tokens",
        alias = "cacheWriteInputTokens"
    )]
    cache_write: u64,
    #[serde(
        default,
        alias = "reasoning_output_tokens",
        alias = "thoughtsTokenCount"
    )]
    reasoning_tokens: u64,
}

impl JsonUsage {
    fn normalized(&self, cached_is_subset: bool) -> TokenUsage {
        TokenUsage {
            input: if cached_is_subset {
                self.input.saturating_sub(self.cache_read)
            } else {
                self.input
            },
            output: self.output.saturating_sub(self.reasoning_tokens),
            cache_read: self.cache_read,
            cache_write: self.cache_write,
            reasoning: self.reasoning_tokens,
        }
    }
}

#[derive(Deserialize)]
struct CodexEnvelope<'a> {
    #[serde(rename = "type")]
    kind: Option<&'a str>,
    timestamp: Option<JsonTimestamp<'a>>,
    #[serde(borrow)]
    payload: Option<CodexPayload<'a>>,
}

#[derive(Deserialize)]
struct CodexPayload<'a> {
    #[serde(rename = "type")]
    kind: Option<&'a str>,
    #[serde(alias = "model_id", alias = "modelID")]
    model: Option<&'a str>,
    info: Option<CodexInfo>,
}

#[derive(Deserialize)]
struct CodexInfo {
    total_token_usage: Option<JsonUsage>,
}

pub fn parse_session_incremental(
    harness: Harness,
    path: &Path,
    range: Option<&TimeRange>,
    previous: Option<ParserProgress>,
) -> Result<(UsageRecord, Option<ParserProgress>), String> {
    if matches!(
        harness,
        Harness::Droid
            | Harness::Codebuff
            | Harness::Hermes
            | Harness::Goose
            | Harness::OpenClaw
            | Harness::Kilo
            | Harness::Kimi
            | Harness::Qwen
            | Harness::Copilot
    ) || path
        .extension()
        .is_none_or(|extension| extension != "jsonl")
    {
        return parse_session_in_range(harness, path, range).map(|record| (record, None));
    }
    let metadata = fs::metadata(path)
        .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;
    let identity = file_identity(&metadata);
    let range_start_ms = range.map(|range| range.start_ms);
    let range_end_ms = range.and_then(|range| range.end_ms);
    let mut state = previous
        .filter(|state| {
            state.version == 2
                && state.harness == harness.name()
                && state.identity == identity
                && state.offset <= metadata.len()
                && state.range_start_ms == range_start_ms
                && state.range_end_ms == range_end_ms
                && tail_hash(path, state.offset).is_some_and(|hash| hash == state.tail_hash)
        })
        .unwrap_or_else(|| ParserProgress {
            version: 2,
            harness: harness.name().to_owned(),
            identity,
            offset: 0,
            tail_hash: 0,
            attribution: UsageAccumulator::default(),
            seen_ids: HashSet::new(),
            codex_previous: TokenUsage::default(),
            range_start_ms,
            range_end_ms,
        });
    let file =
        fs::File::open(path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let mut reader = BufReader::new(file);
    reader
        .seek(SeekFrom::Start(state.offset))
        .map_err(|error| format!("cannot seek {}: {error}", path.display()))?;
    let mut bytes = Vec::with_capacity(4096);
    loop {
        bytes.clear();
        let read = reader
            .read_until(b'\n', &mut bytes)
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        if read == 0 {
            break;
        }
        if !bytes.ends_with(b"\n") {
            break;
        }
        state.offset = state.offset.saturating_add(read as u64);
        let line = bytes.strip_suffix(b"\n").unwrap_or(&bytes);
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        if !process_typed_jsonl(harness, line, range, &mut state) {
            let value: Value = serde_json::from_slice(line).map_err(|error| {
                format!(
                    "invalid JSON near byte {} in {}: {error}",
                    state.offset,
                    path.display()
                )
            })?;
            process_incremental_value(harness, &value, range, &mut state);
        }
    }
    state.tail_hash = tail_hash(path, state.offset).unwrap_or(0);
    let record = finish(
        state.attribution.models.clone(),
        state.attribution.unattributed,
    )?;
    Ok((record, Some(state)))
}

fn process_typed_jsonl(
    harness: Harness,
    line: &[u8],
    range: Option<&TimeRange>,
    state: &mut ParserProgress,
) -> bool {
    match harness {
        Harness::Claude => process_typed_claude(line, range, state),
        Harness::Codex => process_typed_codex(line, range, state),
        Harness::Omp => process_typed_omp(line, range, state),
        _ => false,
    }
}

fn process_typed_claude(
    line: &[u8],
    range: Option<&TimeRange>,
    state: &mut ParserProgress,
) -> bool {
    let Ok(envelope) = serde_json::from_slice::<JsonlEnvelope<'_>>(line) else {
        return false;
    };
    let known_shape = matches!(
        envelope.kind,
        Some(
            "mode"
                | "permission-mode"
                | "file-history-snapshot"
                | "user"
                | "system"
                | "attachment"
                | "ai-title"
                | "assistant"
                | "last-prompt"
                | "queue-operation"
        )
    );
    let Some(message) = envelope.message else {
        return known_shape || !contains_usage_key(line);
    };
    if message.model == Some("<synthetic>") {
        return true;
    }
    if let Some(model) = message.model {
        state.attribution.set_model(model);
    }
    let Some(usage) = message.usage else {
        return known_shape || !contains_usage_key(line);
    };
    let timestamp = envelope
        .timestamp
        .as_ref()
        .or(message.timestamp.as_ref())
        .and_then(JsonTimestamp::milliseconds);
    if range.is_some_and(|range| timestamp.is_none_or(|time| !range.contains(time))) {
        return true;
    }
    let should_count = message
        .id
        .is_none_or(|identity| state.seen_ids.insert(identity.to_owned()));
    if should_count {
        state.attribution.add_usage(usage.normalized(false));
    }
    true
}

fn process_typed_codex(line: &[u8], range: Option<&TimeRange>, state: &mut ParserProgress) -> bool {
    let Ok(envelope) = serde_json::from_slice::<CodexEnvelope<'_>>(line) else {
        return false;
    };
    let known_shape = matches!(
        envelope.kind,
        Some("session_meta" | "event_msg" | "response_item" | "world_state" | "turn_context")
    );
    let Some(payload) = envelope.payload else {
        return known_shape || !contains_usage_key(line);
    };
    if let Some(model) = payload.model {
        state.attribution.set_model(model);
    }
    if payload.kind != Some("token_count") {
        return known_shape || !contains_usage_key(line);
    }
    let Some(snapshot) = payload
        .info
        .and_then(|info| info.total_token_usage)
        .map(|usage| usage.normalized(true))
    else {
        return !contains_usage_key(line);
    };
    let timestamp = envelope
        .timestamp
        .as_ref()
        .and_then(JsonTimestamp::milliseconds);
    let delta = snapshot.delta_from(state.codex_previous);
    state.codex_previous = snapshot;
    if range.is_none_or(|range| timestamp.is_some_and(|time| range.contains(time))) {
        state.attribution.add_usage(delta);
    }
    true
}

fn process_typed_omp(line: &[u8], range: Option<&TimeRange>, state: &mut ParserProgress) -> bool {
    let Ok(envelope) = serde_json::from_slice::<JsonlEnvelope<'_>>(line) else {
        return false;
    };
    if let Some(model) = envelope.model {
        state.attribution.set_model(model);
    }
    let known_shape = matches!(
        envelope.kind,
        Some(
            "title"
                | "session"
                | "model_change"
                | "thinking_level_change"
                | "message"
                | "title_change"
                | "custom"
                | "custom_message"
                | "compaction"
        )
    );
    let Some(message) = envelope.message else {
        return known_shape || !contains_usage_key(line);
    };
    if let Some(model) = message.model {
        state.attribution.set_model(model);
    }
    let timestamp = envelope
        .timestamp
        .as_ref()
        .or(message.timestamp.as_ref())
        .and_then(JsonTimestamp::milliseconds);
    let identity = message.id;
    let usage = if let Some(usage) = message.usage {
        usage
    } else if let Some(response) = message.details.and_then(|details| details.response)
        && let Some(usage) = response.usage
    {
        if let Some(model) = response.model {
            state.attribution.set_model(model);
        }
        usage
    } else {
        return known_shape || !contains_usage_key(line);
    };
    if range.is_some_and(|range| timestamp.is_none_or(|time| !range.contains(time))) {
        return true;
    }
    let should_count = identity.is_none_or(|identity| state.seen_ids.insert(identity.to_owned()));
    if should_count {
        state.attribution.add_usage(usage.normalized(false));
    }
    true
}

fn contains_usage_key(line: &[u8]) -> bool {
    [
        b"\"usage\":".as_slice(),
        b"\"usageMetadata\":",
        b"\"tokenUsage\":",
        b"\"total_token_usage\":",
    ]
    .into_iter()
    .any(|needle| line.windows(needle.len()).any(|window| window == needle))
}

fn process_incremental_value(
    harness: Harness,
    value: &Value,
    range: Option<&TimeRange>,
    state: &mut ParserProgress,
) {
    match harness {
        Harness::Claude => {
            let Some(message) = value.get("message").and_then(Value::as_object) else {
                return;
            };
            if message.get("model").and_then(Value::as_str) == Some("<synthetic>") {
                return;
            }
            if let Some(model) = message.get("model").and_then(Value::as_str) {
                state.attribution.set_model(model);
            }
            let Some(raw_usage) = message.get("usage").and_then(Value::as_object) else {
                return;
            };
            if !value_in_range(value, range) {
                return;
            }
            let should_count = message
                .get("id")
                .and_then(Value::as_str)
                .is_none_or(|identity| state.seen_ids.insert(identity.to_owned()));
            if should_count {
                state.attribution.add_usage(usage_from(raw_usage, false));
            }
        }
        Harness::Codex => {
            let payload = value.get("payload").unwrap_or(value);
            if let Some(model) = find_string(payload, &["model", "model_id", "modelID"]) {
                state.attribution.set_model(model);
            }
            if payload.get("type").and_then(Value::as_str) != Some("token_count") {
                return;
            }
            let Some(total) = payload
                .pointer("/info/total_token_usage")
                .and_then(Value::as_object)
            else {
                return;
            };
            let snapshot = usage_from(total, true);
            let delta = snapshot.delta_from(state.codex_previous);
            state.codex_previous = snapshot;
            if value_in_range(value, range) {
                state.attribution.add_usage(delta);
            }
        }
        _ => {
            let cached_is_subset = harness == Harness::Gemini;
            collect(
                value,
                &mut state.attribution,
                &mut state.seen_ids,
                cached_is_subset,
                range,
                true,
            );
        }
    }
}

fn tail_hash(path: &Path, offset: u64) -> Option<u64> {
    if offset == 0 {
        return Some(0);
    }
    let start = offset.saturating_sub(4096);
    let mut file = fs::File::open(path).ok()?;
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = vec![0; (offset - start) as usize];
    file.read_exact(&mut bytes).ok()?;
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    Some(hasher.finish())
}

#[cfg(unix)]
fn file_identity(metadata: &fs::Metadata) -> u128 {
    use std::os::unix::fs::MetadataExt;
    ((metadata.dev() as u128) << 64) | metadata.ino() as u128
}

#[cfg(not(unix))]
fn file_identity(metadata: &fs::Metadata) -> u128 {
    metadata
        .created()
        .ok()
        .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(metadata.len() as u128, |value| value.as_nanos())
}

fn json_lines(path: &Path) -> Result<Vec<Value>, String> {
    let file =
        fs::File::open(path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let mut values = Vec::new();
    for (index, line) in BufReader::new(file).lines().enumerate() {
        let line = line.map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        if line.trim().is_empty() {
            continue;
        }
        let value = serde_json::from_str(&line).map_err(|error| {
            format!("invalid JSON at {}:{}: {error}", path.display(), index + 1)
        })?;
        values.push(value);
    }
    Ok(values)
}

pub fn parse_usage_events(harness: Harness, path: &Path) -> Result<Vec<UsageEvent>, String> {
    parse_usage_session(harness, path).map(|session| session.events)
}

pub fn parse_usage_session(harness: Harness, path: &Path) -> Result<UsageSession, String> {
    let fallback_timestamp = fs::metadata(path)
        .ok()
        .and_then(|metadata| metadata.modified().ok())
        .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|value| i64::try_from(value.as_millis()).ok())
        .unwrap_or(0);
    if path.extension().is_some_and(|extension| extension == "db") {
        let events = if matches!(harness, Harness::Hermes | Harness::Goose | Harness::Kilo) {
            parse_structured_sqlite_events(harness, path)
        } else {
            parse_sqlite_generic_events(path, fallback_timestamp)
        }?;
        return Ok(UsageSession {
            metadata: SessionMetadata::default(),
            events,
        });
    }
    let values = if path
        .extension()
        .is_some_and(|extension| extension == "jsonl")
    {
        json_lines(path)?
    } else {
        let bytes =
            fs::read(path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        vec![
            serde_json::from_slice(&bytes)
                .map_err(|error| format!("invalid JSON in {}: {error}", path.display()))?,
        ]
    };
    let metadata = extract_session_metadata(harness, &values);
    let mut events = Vec::new();
    let mut seen = HashSet::new();
    match harness {
        Harness::Claude => {
            let mut active_model = None;
            for value in &values {
                let Some(message) = value.get("message").and_then(Value::as_object) else {
                    continue;
                };
                if let Some(model) = message.get("model").and_then(Value::as_str) {
                    if model == "<synthetic>" {
                        continue;
                    }
                    active_model = Some(model.to_owned());
                }
                let Some(raw) = message.get("usage").and_then(Value::as_object) else {
                    continue;
                };
                let identity = message
                    .get("id")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .unwrap_or_else(|| serde_json::to_string(raw).unwrap_or_default());
                if seen.insert(identity) {
                    events.push(UsageEvent {
                        timestamp_ms: timestamp_ms(value)
                            .or_else(|| message.get("timestamp").and_then(timestamp_value))
                            .unwrap_or(fallback_timestamp),
                        model: active_model.clone().unwrap_or_else(|| "unknown".to_owned()),
                        usage: usage_from(raw, false),
                        reported_cost: reported_cost(value),
                    });
                }
            }
        }
        Harness::Codex => {
            let mut active_model = None;
            let mut previous = TokenUsage::default();
            for value in &values {
                let payload = value.get("payload").unwrap_or(value);
                if let Some(model) = find_string(payload, &["model", "model_id", "modelID"]) {
                    active_model = Some(model.to_owned());
                }
                if payload.get("type").and_then(Value::as_str) == Some("token_count")
                    && let Some(total) = payload
                        .pointer("/info/total_token_usage")
                        .and_then(Value::as_object)
                {
                    let snapshot = usage_from(total, true);
                    let usage = snapshot.delta_from(previous);
                    previous = snapshot;
                    if !usage.is_empty() {
                        events.push(UsageEvent {
                            timestamp_ms: timestamp_ms(value).unwrap_or(fallback_timestamp),
                            model: active_model.clone().unwrap_or_else(|| "unknown".to_owned()),
                            usage,
                            reported_cost: None,
                        });
                    }
                }
            }
        }
        Harness::Droid => {
            for value in &values {
                let Some(raw) = value.get("tokenUsage").and_then(Value::as_object) else {
                    continue;
                };
                let mut usage = usage_from(raw, false);
                usage.reasoning = number(raw, &["thinkingTokens"]);
                apply_total_fallback(&mut usage, number(raw, &["totalTokens"]));
                if !usage.is_empty() {
                    events.push(UsageEvent {
                        timestamp_ms: timestamp_ms(value).unwrap_or(fallback_timestamp),
                        model: droid_model(path, value),
                        usage,
                        reported_cost: None,
                    });
                }
            }
        }
        Harness::Omp => {
            parse_omp_events(&values, fallback_timestamp, &mut seen, &mut events);
        }
        Harness::OpenClaw => {
            parse_openclaw_events(&values, fallback_timestamp, &mut seen, &mut events);
        }
        Harness::Kimi => {
            parse_kimi_events(path, &values, fallback_timestamp, &mut seen, &mut events);
        }
        Harness::Copilot => {
            parse_copilot_events(&values, fallback_timestamp, &mut seen, &mut events);
        }
        Harness::Codebuff => {
            parse_codebuff_events(&values, fallback_timestamp, &mut seen, &mut events);
        }
        Harness::Qwen => {
            for value in &values {
                if value.get("type").and_then(Value::as_str) == Some("assistant") {
                    collect_usage_events(
                        value,
                        None,
                        None,
                        fallback_timestamp,
                        false,
                        &mut seen,
                        &mut events,
                    );
                }
            }
        }
        _ => {
            let cached_is_subset = harness == Harness::Gemini;
            for value in &values {
                collect_usage_events(
                    value,
                    None,
                    None,
                    fallback_timestamp,
                    cached_is_subset,
                    &mut seen,
                    &mut events,
                );
            }
        }
    }
    if events.is_empty() {
        Err("session contains no token usage".to_owned())
    } else {
        Ok(UsageSession { metadata, events })
    }
}

fn extract_session_metadata(harness: Harness, values: &[Value]) -> SessionMetadata {
    let generic_id = values.iter().find_map(|value| {
        find_string(
            value,
            &[
                "sessionId",
                "sessionID",
                "session_id",
                "conversationId",
                "conversationID",
                "conversation_id",
            ],
        )
    });
    let provider_id = match harness {
        Harness::Codex => values.iter().find_map(|value| {
            (value.get("type").and_then(Value::as_str) == Some("session_meta"))
                .then(|| value.get("payload"))
                .flatten()
                .and_then(|payload| find_string(payload, &["id", "sessionId", "session_id"]))
        }),
        Harness::Pi | Harness::Omp => values.iter().find_map(|value| {
            (value.get("type").and_then(Value::as_str) == Some("session"))
                .then(|| find_string(value, &["id", "sessionId", "session_id"]))
                .flatten()
        }),
        _ => None,
    };
    let title = values
        .iter()
        .find_map(|value| {
            (value.get("type").and_then(Value::as_str) == Some("title"))
                .then(|| value.get("title").and_then(Value::as_str))
                .flatten()
        })
        .or_else(|| {
            values.iter().rev().find_map(|value| {
                (value.get("type").and_then(Value::as_str) == Some("title_change"))
                    .then(|| value.get("title").and_then(Value::as_str))
                    .flatten()
            })
        })
        .or_else(|| {
            values.iter().find_map(|value| {
                (value.get("type").and_then(Value::as_str) == Some("session"))
                    .then(|| value.get("title").and_then(Value::as_str))
                    .flatten()
            })
        });
    SessionMetadata {
        id: provider_id.or(generic_id).and_then(nonempty_owned),
        title: title.and_then(nonempty_owned),
    }
}

fn nonempty_owned(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn collect_usage_events(
    value: &Value,
    inherited_model: Option<&str>,
    inherited_timestamp: Option<i64>,
    fallback_timestamp: i64,
    cached_is_subset: bool,
    seen: &mut HashSet<String>,
    events: &mut Vec<UsageEvent>,
) {
    match value {
        Value::Object(object) => {
            let model = find_string(value, &["model", "model_id", "modelID"]).or(inherited_model);
            let timestamp = timestamp_ms(value).or(inherited_timestamp);
            let usage = object
                .get("usage")
                .and_then(Value::as_object)
                .or_else(|| object.get("usageMetadata").and_then(Value::as_object))
                .or_else(|| object.get("tokenUsage").and_then(Value::as_object))
                .or_else(|| object.get("tokens").and_then(Value::as_object));
            if let Some(raw) = usage {
                let should_count = object
                    .get("id")
                    .and_then(Value::as_str)
                    .is_none_or(|identity| seen.insert(identity.to_owned()));
                if should_count {
                    let usage = usage_with_total(raw, cached_is_subset);
                    if !usage.is_empty() {
                        events.push(UsageEvent {
                            timestamp_ms: timestamp.unwrap_or(fallback_timestamp),
                            model: model.unwrap_or("unknown").to_owned(),
                            usage,
                            reported_cost: reported_cost(value),
                        });
                    }
                }
                return;
            }
            for child in object.values() {
                collect_usage_events(
                    child,
                    model,
                    timestamp,
                    fallback_timestamp,
                    cached_is_subset,
                    seen,
                    events,
                );
            }
        }
        Value::Array(items) => {
            for child in items {
                collect_usage_events(
                    child,
                    inherited_model,
                    inherited_timestamp,
                    fallback_timestamp,
                    cached_is_subset,
                    seen,
                    events,
                );
            }
        }
        _ => {}
    }
}

fn timestamp_value(value: &Value) -> Option<i64> {
    value
        .as_str()
        .and_then(parse_timestamp)
        .or_else(|| value.as_i64().map(timestamp_number))
        .or_else(|| {
            value
                .as_u64()
                .and_then(|value| i64::try_from(value).ok())
                .map(timestamp_number)
        })
}

fn droid_model(path: &Path, value: &Value) -> String {
    if let Some(model) = value.get("model").and_then(Value::as_str) {
        let model = normalize_droid_model(model);
        if !model.is_empty() {
            return model;
        }
    }
    if let Some(name) = path.file_name().and_then(|name| name.to_str())
        && let Some(prefix) = name.strip_suffix(".settings.json")
        && let Ok(content) = fs::read_to_string(path.with_file_name(format!("{prefix}.jsonl")))
    {
        for line in content.lines().take(500) {
            if let Some((_, tail)) = line.split_once("Model:") {
                let raw = tail
                    .split(['"', '\\', '['])
                    .next()
                    .unwrap_or_default()
                    .trim();
                let model = normalize_droid_model(raw);
                if !model.is_empty() {
                    return model;
                }
            }
        }
    }
    match value
        .get("providerLock")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "anthropic" | "claude" => "claude-unknown".to_owned(),
        "openai" => "gpt-unknown".to_owned(),
        "google" | "gemini" | "vertex" | "vertex_ai" => "gemini-unknown".to_owned(),
        "xai" | "grok" => "grok-unknown".to_owned(),
        _ => "unknown".to_owned(),
    }
}

fn normalize_droid_model(model: &str) -> String {
    let raw = model.strip_prefix("custom:").unwrap_or(model);
    let mut unbracketed = String::new();
    let mut depth = 0_u32;
    for character in raw.chars() {
        match character {
            '[' => depth = depth.saturating_add(1),
            ']' => depth = depth.saturating_sub(1),
            _ if depth == 0 => unbracketed.push(character),
            _ => {}
        }
    }
    let mut normalized = String::new();
    let mut dash = false;
    for character in unbracketed.trim().chars() {
        let character = if character == '.' || character.is_whitespace() || character == '-' {
            '-'
        } else {
            character.to_ascii_lowercase()
        };
        if character == '-' {
            if !dash {
                normalized.push(character);
            }
            dash = true;
        } else {
            normalized.push(character);
            dash = false;
        }
    }
    normalized.trim_matches('-').to_owned()
}

fn parse_omp_events(
    values: &[Value],
    fallback_timestamp: i64,
    seen: &mut HashSet<String>,
    events: &mut Vec<UsageEvent>,
) {
    let mut active_model = None;
    for (index, value) in values.iter().enumerate() {
        if let Some(model) = find_string(value, &["model", "model_id", "modelID"]) {
            active_model = Some(model.to_owned());
        }
        let Some(message) = value.get("message").and_then(Value::as_object) else {
            continue;
        };
        let response = message
            .get("details")
            .and_then(|details| details.get("response"))
            .and_then(Value::as_object);
        let message_model = find_string_object(message, &["model", "model_id", "modelID"]);
        let response_model = response
            .and_then(|response| find_string_object(response, &["model", "model_id", "modelID"]));
        if let Some(model) = response_model.or(message_model) {
            active_model = Some(model.to_owned());
        }
        let Some((raw_usage, cost_owner)) = message
            .get("usage")
            .and_then(Value::as_object)
            .map(|usage| (usage, message))
            .or_else(|| {
                response.and_then(|response| {
                    response
                        .get("usage")
                        .and_then(Value::as_object)
                        .map(|usage| (usage, response))
                })
            })
        else {
            continue;
        };
        let identity = message
            .get("id")
            .or_else(|| value.get("id"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| format!("line:{index}"));
        if !seen.insert(identity) {
            continue;
        }
        let usage = usage_with_total(raw_usage, false);
        if usage.is_empty() {
            continue;
        }
        events.push(UsageEvent {
            timestamp_ms: timestamp_ms(value)
                .or_else(|| message.get("timestamp").and_then(timestamp_value))
                .unwrap_or(fallback_timestamp),
            model: active_model.clone().unwrap_or_else(|| "unknown".to_owned()),
            usage,
            reported_cost: reported_cost_object(cost_owner)
                .or_else(|| reported_cost_object(message))
                .or_else(|| reported_cost(value)),
        });
    }
}

fn parse_openclaw_events(
    values: &[Value],
    fallback_timestamp: i64,
    seen: &mut HashSet<String>,
    events: &mut Vec<UsageEvent>,
) {
    let mut active_model = None;
    for (index, value) in values.iter().enumerate() {
        let kind = value.get("type").and_then(Value::as_str);
        let model_change = kind == Some("model_change")
            || (kind == Some("custom")
                && value.get("customType").and_then(Value::as_str) == Some("model-snapshot"));
        if model_change {
            let source = value
                .get("data")
                .filter(|value| value.is_object())
                .unwrap_or(value);
            if let Some(model) = find_string(source, &["modelId", "modelID", "model"]) {
                active_model = Some(model.to_owned());
            }
            continue;
        }
        if kind != Some("message") {
            continue;
        }
        let Some(message) = value.get("message").and_then(Value::as_object) else {
            continue;
        };
        if message.get("role").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let Some(raw) = message.get("usage").and_then(Value::as_object) else {
            continue;
        };
        let identity = message
            .get("id")
            .or_else(|| value.get("id"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| format!("line:{index}"));
        if !seen.insert(identity) {
            continue;
        }
        let usage = usage_with_total(raw, false);
        if usage.is_empty() {
            continue;
        }
        events.push(UsageEvent {
            timestamp_ms: message
                .get("timestamp")
                .and_then(timestamp_value)
                .or_else(|| timestamp_ms(value))
                .unwrap_or(fallback_timestamp),
            model: find_string(
                &Value::Object(message.clone()),
                &["modelId", "modelID", "model"],
            )
            .map(str::to_owned)
            .or_else(|| active_model.clone())
            .unwrap_or_else(|| "unknown".to_owned()),
            usage,
            reported_cost: reported_cost_object(message),
        });
    }
}

fn parse_kimi_events(
    path: &Path,
    values: &[Value],
    fallback_timestamp: i64,
    seen: &mut HashSet<String>,
    events: &mut Vec<UsageEvent>,
) {
    let configured_model = path
        .ancestors()
        .find(|ancestor| ancestor.file_name().is_some_and(|name| name == "sessions"))
        .and_then(Path::parent)
        .and_then(|root| fs::read(root.join("config.json")).ok())
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .and_then(|value| {
            value
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "kimi-for-coding".to_owned());
    for value in values {
        let (raw, model, identity, timestamp) =
            if value.get("type").and_then(Value::as_str) == Some("usage.record") {
                if value.get("usageScope").and_then(Value::as_str) != Some("turn") {
                    continue;
                }
                let Some(raw) = value.get("usage").and_then(Value::as_object) else {
                    continue;
                };
                (
                    raw,
                    value
                        .get("model")
                        .and_then(Value::as_str)
                        .unwrap_or(&configured_model)
                        .strip_prefix("kimi-code/")
                        .unwrap_or_else(|| {
                            value
                                .get("model")
                                .and_then(Value::as_str)
                                .unwrap_or(&configured_model)
                        }),
                    None,
                    value.get("time").and_then(timestamp_value),
                )
            } else {
                let Some(message) = value.get("message").and_then(Value::as_object) else {
                    continue;
                };
                if message.get("type").and_then(Value::as_str) != Some("StatusUpdate") {
                    continue;
                }
                let Some(payload) = message.get("payload").and_then(Value::as_object) else {
                    continue;
                };
                let Some(raw) = payload.get("token_usage").and_then(Value::as_object) else {
                    continue;
                };
                (
                    raw,
                    configured_model.as_str(),
                    payload.get("message_id").and_then(Value::as_str),
                    value.get("timestamp").and_then(timestamp_seconds),
                )
            };
        if identity.is_some_and(|identity| !seen.insert(identity.to_owned())) {
            continue;
        }
        let usage = usage_with_total(raw, false);
        if !usage.is_empty() {
            events.push(UsageEvent {
                timestamp_ms: timestamp.unwrap_or(fallback_timestamp),
                model: model.to_owned(),
                usage,
                reported_cost: reported_cost(value),
            });
        }
    }
}

fn parse_copilot_events(
    values: &[Value],
    fallback_timestamp: i64,
    seen: &mut HashSet<String>,
    events: &mut Vec<UsageEvent>,
) {
    let mut contexts: HashMap<String, (Option<String>, Option<String>)> = HashMap::new();
    for value in values {
        let Some(attributes) = value.get("attributes").and_then(Value::as_object) else {
            continue;
        };
        let Some(trace) = value
            .get("traceId")
            .or_else(|| value.pointer("/spanContext/traceId"))
            .and_then(Value::as_str)
        else {
            continue;
        };
        let context = contexts.entry(trace.to_owned()).or_default();
        if context.0.is_none() {
            context.0 = attribute_string(
                attributes,
                &[
                    "gen_ai.response.model",
                    "gen_ai.request.model",
                    "gen_ai.model",
                ],
            )
            .map(str::to_owned);
        }
        if context.1.is_none() {
            context.1 = attribute_string(
                attributes,
                &["session.id", "conversation.id", "gen_ai.conversation.id"],
            )
            .map(str::to_owned);
        }
    }
    for (index, value) in values.iter().enumerate() {
        let Some(attributes) = value.get("attributes").and_then(Value::as_object) else {
            continue;
        };
        let trace = value
            .get("traceId")
            .or_else(|| value.pointer("/spanContext/traceId"))
            .and_then(Value::as_str);
        let context = trace.and_then(|trace| contexts.get(trace));
        let cache_read = attribute_u64(attributes, &["gen_ai.usage.cache_read.input_tokens"]);
        let mut usage = TokenUsage {
            input: attribute_u64(attributes, &["gen_ai.usage.input_tokens"])
                .saturating_sub(cache_read),
            output: attribute_u64(attributes, &["gen_ai.usage.output_tokens"]),
            cache_read,
            cache_write: attribute_u64(
                attributes,
                &[
                    "gen_ai.usage.cache_write.input_tokens",
                    "gen_ai.usage.cache_creation.input_tokens",
                ],
            ),
            reasoning: attribute_u64(
                attributes,
                &[
                    "gen_ai.usage.reasoning.output_tokens",
                    "gen_ai.usage.reasoning_tokens",
                ],
            ),
        };
        let total = attribute_u64(
            attributes,
            &[
                "gen_ai.usage.total_tokens",
                "gen_ai.usage.total.token_count",
            ],
        );
        apply_total_fallback(&mut usage, total);
        if usage.is_empty() {
            continue;
        }
        let identity = attribute_string(attributes, &["gen_ai.response.id"])
            .map(str::to_owned)
            .or_else(|| trace.map(|trace| format!("{trace}:{index}")))
            .unwrap_or_else(|| format!("line:{index}"));
        if !seen.insert(identity) {
            continue;
        }
        let model = attribute_string(
            attributes,
            &[
                "gen_ai.response.model",
                "gen_ai.request.model",
                "gen_ai.model",
            ],
        )
        .map(str::to_owned)
        .or_else(|| context.and_then(|context| context.0.clone()))
        .unwrap_or_else(|| "unknown".to_owned());
        events.push(UsageEvent {
            timestamp_ms: otel_timestamp(value).unwrap_or(fallback_timestamp),
            model,
            usage,
            reported_cost: reported_cost(value),
        });
    }
}

fn parse_codebuff_events(
    values: &[Value],
    fallback_timestamp: i64,
    seen: &mut HashSet<String>,
    events: &mut Vec<UsageEvent>,
) {
    for (index, value) in values
        .iter()
        .flat_map(|value| {
            value
                .as_array()
                .map_or_else(|| vec![value], |items| items.iter().collect())
        })
        .enumerate()
    {
        let Some(message) = value.as_object() else {
            continue;
        };
        if !matches!(
            find_string(value, &["variant", "role"]),
            Some("ai" | "agent" | "assistant")
        ) {
            continue;
        }
        let metadata = message.get("metadata").unwrap_or(value);
        let Some(raw) = first_usage_object(metadata) else {
            continue;
        };
        let identity = message
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| format!("message:{index}"));
        if !seen.insert(identity) {
            continue;
        }
        let usage = usage_with_total(raw, false);
        if !usage.is_empty() {
            events.push(UsageEvent {
                timestamp_ms: timestamp_ms(value).unwrap_or(fallback_timestamp),
                model: find_string(metadata, &["model"])
                    .or_else(|| raw.get("model").and_then(Value::as_str))
                    .unwrap_or("codebuff-unknown")
                    .to_owned(),
                usage,
                reported_cost: reported_cost(value),
            });
        }
    }
}

fn first_usage_object(value: &Value) -> Option<&Map<String, Value>> {
    match value {
        Value::Object(object) => {
            if let Some(usage) = object.get("usage").and_then(Value::as_object) {
                return Some(usage);
            }
            object.values().find_map(first_usage_object)
        }
        Value::Array(items) => items.iter().find_map(first_usage_object),
        _ => None,
    }
}

fn usage_with_total(object: &Map<String, Value>, cached_is_subset: bool) -> TokenUsage {
    let mut usage = usage_from(object, cached_is_subset);
    let total = number(
        object,
        &["totalTokens", "total_tokens", "totalTokenCount", "total"],
    );
    apply_total_fallback(&mut usage, total);
    usage
}

fn apply_total_fallback(usage: &mut TokenUsage, total: u64) {
    let accounted = usage.total_tokens();
    if total > accounted {
        usage.reasoning = usage.reasoning.saturating_add(total - accounted);
    }
}

fn timestamp_seconds(value: &Value) -> Option<i64> {
    value
        .as_f64()
        .filter(|value| value.is_finite())
        .map(|value| (value * 1000.0) as i64)
        .or_else(|| timestamp_value(value))
}

fn attribute_string<'a>(attributes: &'a Map<String, Value>, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| attributes.get(*key).and_then(Value::as_str))
}

fn attribute_u64(attributes: &Map<String, Value>, keys: &[&str]) -> u64 {
    keys.iter()
        .find_map(|key| attributes.get(*key))
        .and_then(|value| {
            value
                .as_u64()
                .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
        })
        .unwrap_or(0)
}

fn otel_timestamp(value: &Value) -> Option<i64> {
    for key in [
        "startTime",
        "endTime",
        "hrTime",
        "_hrTime",
        "time",
        "timestamp",
        "observedTimestamp",
        "timeUnixNano",
    ] {
        let Some(raw) = value.get(key) else {
            continue;
        };
        if let Some(items) = raw.as_array()
            && items.len() >= 2
            && let (Some(seconds), Some(nanos)) = (items[0].as_i64(), items[1].as_i64())
        {
            return Some(
                seconds
                    .saturating_mul(1000)
                    .saturating_add(nanos / 1_000_000),
            );
        }
        if let Some(text) = raw.as_str() {
            if let Some(timestamp) = parse_timestamp(text) {
                return Some(timestamp);
            }
            if let Ok(number) = text.parse::<i128>() {
                return i64::try_from(if number > 10_000_000_000_000 {
                    number / 1_000_000
                } else {
                    number
                })
                .ok()
                .map(timestamp_number);
            }
        }
        if let Some(timestamp) = timestamp_value(raw) {
            return Some(timestamp);
        }
    }
    None
}

fn parse_claude(path: &Path, range: Option<&TimeRange>) -> Result<UsageRecord, String> {
    let mut attribution = UsageAccumulator::default();
    let mut seen = HashSet::new();
    for value in json_lines(path)? {
        let Some(message) = value.get("message").and_then(Value::as_object) else {
            continue;
        };
        if message.get("model").and_then(Value::as_str) == Some("<synthetic>") {
            continue;
        }
        let Some(raw_usage) = message.get("usage").and_then(Value::as_object) else {
            continue;
        };
        if let Some(model) = message.get("model").and_then(Value::as_str) {
            attribution.set_model(model);
        }
        if !value_in_range(&value, range) {
            continue;
        }
        let identity = message
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| serde_json::to_string(raw_usage).unwrap_or_default());
        if seen.insert(identity) {
            attribution.add_usage(usage_from(raw_usage, false));
        }
    }
    finish(attribution.models, attribution.unattributed)
}

fn parse_codex(path: &Path, range: Option<&TimeRange>) -> Result<UsageRecord, String> {
    let mut attribution = UsageAccumulator::default();
    let mut previous = TokenUsage::default();
    for value in json_lines(path)? {
        let payload = value.get("payload").unwrap_or(&value);
        if let Some(model) = find_string(payload, &["model", "model_id", "modelID"]) {
            attribution.set_model(model);
        }
        if payload.get("type").and_then(Value::as_str) == Some("token_count")
            && let Some(total) = payload
                .pointer("/info/total_token_usage")
                .and_then(Value::as_object)
        {
            let snapshot = usage_from(total, true);
            let delta = snapshot.delta_from(previous);
            previous = snapshot;
            if value_in_range(&value, range) {
                attribution.add_usage(delta);
            }
        }
    }
    finish(attribution.models, attribution.unattributed)
}

fn parse_generic(
    path: &Path,
    cached_is_subset: bool,
    range: Option<&TimeRange>,
) -> Result<UsageRecord, String> {
    let mut attribution = UsageAccumulator::default();
    let mut seen = HashSet::new();
    if path
        .extension()
        .is_some_and(|extension| extension == "jsonl")
    {
        let file = fs::File::open(path)
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        for (index, line) in BufReader::new(file).lines().enumerate() {
            let line = line.map_err(|error| format!("cannot read {}: {error}", path.display()))?;
            if line.trim().is_empty() {
                continue;
            }
            let value: Value = serde_json::from_str(&line).map_err(|error| {
                format!("invalid JSON at {}:{}: {error}", path.display(), index + 1)
            })?;
            collect(
                &value,
                &mut attribution,
                &mut seen,
                cached_is_subset,
                range,
                true,
            );
        }
    } else {
        let bytes =
            fs::read(path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|error| format!("invalid JSON in {}: {error}", path.display()))?;
        collect(
            &value,
            &mut attribution,
            &mut seen,
            cached_is_subset,
            range,
            true,
        );
    }
    finish(attribution.models, attribution.unattributed)
}

fn collect(
    value: &Value,
    attribution: &mut UsageAccumulator,
    seen: &mut HashSet<String>,
    cached_is_subset: bool,
    range: Option<&TimeRange>,
    inherited_allowed: bool,
) {
    match value {
        Value::Object(object) => {
            let allowed = timestamp_ms(value).map_or(inherited_allowed, |time| {
                range.is_none_or(|range| range.contains(time))
            });
            if let Some(model) = find_string(value, &["model", "model_id", "modelID"]) {
                attribution.set_model(model);
            }
            let usage_object = object
                .get("usage")
                .and_then(Value::as_object)
                .or_else(|| object.get("usageMetadata").and_then(Value::as_object))
                .or_else(|| object.get("tokenUsage").and_then(Value::as_object))
                .or_else(|| object.get("tokens").and_then(Value::as_object));
            if let Some(raw) = usage_object {
                let should_count = object
                    .get("id")
                    .and_then(Value::as_str)
                    .is_none_or(|identity| seen.insert(identity.to_owned()));
                if allowed && should_count {
                    attribution.add_usage(usage_with_total(raw, cached_is_subset));
                }
                return;
            }
            for child in object.values() {
                collect(child, attribution, seen, cached_is_subset, range, allowed);
            }
        }
        Value::Array(items) => {
            for child in items {
                collect(
                    child,
                    attribution,
                    seen,
                    cached_is_subset,
                    range,
                    inherited_allowed,
                );
            }
        }
        _ => {}
    }
}

fn usage_from(object: &Map<String, Value>, cached_is_subset: bool) -> TokenUsage {
    let cache = object.get("cache").and_then(Value::as_object);
    let raw_input = number(
        object,
        &[
            "input",
            "input_tokens",
            "inputTokens",
            "prompt_tokens",
            "promptTokenCount",
            "input_other",
            "inputOther",
        ],
    );
    let cache_read = number(
        object,
        &[
            "cacheRead",
            "cache_read_input_tokens",
            "cached_input_tokens",
            "cacheReadInputTokens",
            "cachedContentTokenCount",
            "input_cache_read",
            "inputCacheRead",
        ],
    )
    .max(cache.map_or(0, |cache| number(cache, &["read"])));
    let raw_output = number(
        object,
        &[
            "output",
            "output_tokens",
            "outputTokens",
            "completion_tokens",
            "candidatesTokenCount",
            "completionTokens",
        ],
    );
    let reasoning = number(
        object,
        &[
            "reasoning_output_tokens",
            "reasoning_tokens",
            "thoughtsTokenCount",
            "reasoning",
        ],
    );
    TokenUsage {
        input: if cached_is_subset {
            raw_input.saturating_sub(cache_read)
        } else {
            raw_input
        },
        output: raw_output.saturating_sub(reasoning),
        cache_read,
        cache_write: number(
            object,
            &[
                "cacheWrite",
                "cache_creation_input_tokens",
                "cache_write_input_tokens",
                "cacheWriteInputTokens",
                "cacheCreationTokens",
                "cache_creation_tokens",
                "input_cache_creation",
                "inputCacheCreation",
            ],
        )
        .max(cache.map_or(0, |cache| number(cache, &["write"]))),
        reasoning,
    }
}

fn number(object: &Map<String, Value>, keys: &[&str]) -> u64 {
    keys.iter()
        .find_map(|key| {
            let value = object.get(*key)?;
            value
                .as_u64()
                .or_else(|| value.as_i64().and_then(|value| u64::try_from(value).ok()))
                .or_else(|| {
                    value
                        .as_f64()
                        .filter(|value| value.is_finite() && *value >= 0.0)
                        .map(|value| value as u64)
                })
                .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
        })
        .unwrap_or(0)
}

fn reported_cost(value: &Value) -> Option<f64> {
    value.as_object().and_then(reported_cost_object)
}

fn reported_cost_object(object: &Map<String, Value>) -> Option<f64> {
    ["costUSD", "costUsd", "cost_usd", "cost"]
        .into_iter()
        .find_map(|key| object.get(key))
        .and_then(|value| {
            value
                .as_f64()
                .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
        })
        .filter(|value| value.is_finite() && *value >= 0.0)
}

fn find_string<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a str> {
    find_string_object(value.as_object()?, keys)
}

fn find_string_object<'a>(object: &'a Map<String, Value>, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| object.get(*key).and_then(Value::as_str))
}

fn timestamp_ms(value: &Value) -> Option<i64> {
    let object = value.as_object()?;
    [
        "timestamp",
        "created_at",
        "createdAt",
        "created",
        "time",
        "providerLockTimestamp",
    ]
    .into_iter()
    .find_map(|key| {
        let value = object.get(key)?;
        value
            .as_str()
            .and_then(parse_timestamp)
            .or_else(|| value.as_i64().map(timestamp_number))
    })
    .or_else(|| {
        object
            .get("time")
            .and_then(Value::as_object)
            .and_then(|time| time.get("created"))
            .and_then(timestamp_value)
    })
}

fn timestamp_number(value: i64) -> i64 {
    if value < 10_000_000_000 {
        value * 1000
    } else {
        value
    }
}

fn value_in_range(value: &Value, range: Option<&TimeRange>) -> bool {
    range.is_none_or(|range| timestamp_ms(value).is_some_and(|time| range.contains(time)))
}

fn finish(models: Vec<ModelUsage>, unattributed: TokenUsage) -> Result<UsageRecord, String> {
    let total = TokenUsage::total(
        models
            .iter()
            .map(|item| item.usage)
            .chain(std::iter::once(unattributed)),
    );
    if total.is_empty() {
        return Err("session contains no token usage".to_owned());
    }
    if !unattributed.is_empty() {
        return Err("session contains usage but no model identifier".to_owned());
    }
    Ok(UsageRecord { models })
}

fn parse_sqlite_record(
    harness: Harness,
    path: &Path,
    range: Option<&TimeRange>,
) -> Result<UsageRecord, String> {
    if matches!(harness, Harness::Hermes | Harness::Goose | Harness::Kilo) {
        let events = parse_structured_sqlite_events(harness, path)?;
        let mut models = Vec::new();
        for event in events {
            if range.is_none_or(|range| range.contains(event.timestamp_ms)) {
                ModelUsage::add_to(&mut models, &event.model, event.usage);
            }
        }
        return finish(models, TokenUsage::default());
    }
    parse_sqlite_generic(path, range)
}

fn parse_structured_sqlite_events(
    harness: Harness,
    path: &Path,
) -> Result<Vec<UsageEvent>, String> {
    let connection = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| format!("cannot open {}: {error}", path.display()))?;
    let events: Vec<UsageEvent> = match harness {
        Harness::Hermes => {
            let mut statement = connection
                .prepare(
                    "SELECT model, started_at, input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, reasoning_tokens FROM sessions WHERE model IS NOT NULL AND TRIM(model) != ''",
                )
                .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
            let rows = statement
                .query_map([], |row| {
                    let started_at = sqlite_number(row, 1);
                    let timestamp_ms = if started_at > 1_000_000_000_000.0 {
                        started_at as i64
                    } else {
                        (started_at * 1000.0) as i64
                    };
                    Ok(UsageEvent {
                        timestamp_ms,
                        model: row.get(0)?,
                        usage: TokenUsage {
                            input: sqlite_u64(row, 2),
                            output: sqlite_u64(row, 3),
                            cache_read: sqlite_u64(row, 4),
                            cache_write: sqlite_u64(row, 5),
                            reasoning: sqlite_u64(row, 6),
                        },
                        reported_cost: None,
                    })
                })
                .map_err(|error| error.to_string())?;
            rows.filter_map(Result::ok).collect()
        }
        Harness::Goose => {
            let mut statement = connection
                .prepare(
                    "SELECT model_config_json, created_at, total_tokens, input_tokens, output_tokens, accumulated_total_tokens, accumulated_input_tokens, accumulated_output_tokens FROM sessions WHERE model_config_json IS NOT NULL",
                )
                .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
            let rows = statement
                .query_map([], |row| {
                    let config: String = row.get(0)?;
                    let model = serde_json::from_str::<Value>(&config)
                        .ok()
                        .and_then(|value| {
                            value
                                .get("model_name")
                                .and_then(Value::as_str)
                                .map(str::to_owned)
                        })
                        .unwrap_or_else(|| "unknown".to_owned());
                    let timestamp: String = row.get(1)?;
                    let input = sqlite_u64(row, 6).max(sqlite_u64(row, 3));
                    let output = sqlite_u64(row, 7).max(sqlite_u64(row, 4));
                    let total = sqlite_u64(row, 5).max(sqlite_u64(row, 2));
                    Ok(UsageEvent {
                        timestamp_ms: parse_flexible_timestamp(&timestamp).unwrap_or(0),
                        model,
                        usage: TokenUsage {
                            input,
                            output,
                            reasoning: total.saturating_sub(input.saturating_add(output)),
                            ..TokenUsage::default()
                        },
                        reported_cost: None,
                    })
                })
                .map_err(|error| error.to_string())?;
            rows.filter_map(Result::ok).collect()
        }
        Harness::Kilo => {
            let mut statement = connection
                .prepare("SELECT data FROM message")
                .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
            let rows = statement
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(|error| error.to_string())?;
            rows.filter_map(Result::ok)
                .filter_map(|data| serde_json::from_str::<Value>(&data).ok())
                .filter_map(kilo_event)
                .collect()
        }
        _ => unreachable!("structured database harness checked"),
    };
    if events.is_empty() {
        Err("session contains no token usage".to_owned())
    } else {
        Ok(events)
    }
}

fn sqlite_u64(row: &rusqlite::Row<'_>, index: usize) -> u64 {
    row.get::<_, Option<i64>>(index)
        .ok()
        .flatten()
        .and_then(|value| u64::try_from(value.max(0)).ok())
        .or_else(|| {
            row.get::<_, Option<f64>>(index)
                .ok()
                .flatten()
                .filter(|value| value.is_finite() && *value > 0.0)
                .map(|value| value as u64)
        })
        .unwrap_or(0)
}

fn sqlite_number(row: &rusqlite::Row<'_>, index: usize) -> f64 {
    row.get::<_, Option<f64>>(index)
        .ok()
        .flatten()
        .or_else(|| {
            row.get::<_, Option<i64>>(index)
                .ok()
                .flatten()
                .map(|value| value as f64)
        })
        .unwrap_or(0.0)
}

fn parse_flexible_timestamp(value: &str) -> Option<i64> {
    let value = value.trim();
    value
        .parse::<i64>()
        .ok()
        .map(timestamp_number)
        .or_else(|| parse_timestamp(value))
        .or_else(|| {
            (value.len() == 19)
                .then(|| format!("{}T{}Z", &value[..10], &value[11..]))
                .and_then(|value| parse_timestamp(&value))
        })
}

fn kilo_event(value: Value) -> Option<UsageEvent> {
    if value.get("role").and_then(Value::as_str) != Some("assistant") {
        return None;
    }
    let tokens = value.get("tokens")?.as_object()?;
    let cache = tokens.get("cache").and_then(Value::as_object);
    let mut usage = TokenUsage {
        input: number(tokens, &["input"]),
        output: number(tokens, &["output"]),
        cache_read: cache.map_or(0, |cache| number(cache, &["read"])),
        cache_write: cache.map_or(0, |cache| number(cache, &["write"])),
        reasoning: number(tokens, &["reasoning"]),
    };
    apply_total_fallback(&mut usage, number(tokens, &["total"]));
    (!usage.is_empty()).then(|| UsageEvent {
        timestamp_ms: value
            .pointer("/time/created")
            .and_then(Value::as_i64)
            .map(timestamp_number)
            .unwrap_or(0),
        model: find_string(&value, &["modelID", "model_id", "model"])
            .unwrap_or("unknown")
            .to_owned(),
        usage,
        reported_cost: reported_cost(&value),
    })
}

fn sqlite_json_values(path: &Path) -> Result<Vec<Value>, String> {
    let connection = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| format!("cannot open {}: {error}", path.display()))?;
    let mut statement = connection
        .prepare("SELECT name FROM sqlite_master WHERE type='table'")
        .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;
    let tables: Vec<String> = statement
        .query_map([], |row| row.get(0))
        .map_err(|error| error.to_string())?
        .filter_map(Result::ok)
        .collect();
    let mut values = Vec::new();
    for table in tables {
        let escaped = table.replace('"', "\"\"");
        let pragma = format!("PRAGMA table_info(\"{escaped}\")");
        let mut columns = connection
            .prepare(&pragma)
            .map_err(|error| error.to_string())?;
        let text_columns: Vec<String> = columns
            .query_map([], |row| row.get::<_, String>(1))
            .map_err(|error| error.to_string())?
            .filter_map(Result::ok)
            .collect();
        for column in text_columns {
            let column = column.replace('"', "\"\"");
            let query = format!(
                "SELECT \"{column}\" FROM \"{escaped}\" WHERE typeof(\"{column}\")='text' AND (\"{column}\" LIKE '{{%' OR \"{column}\" LIKE '[%')"
            );
            if let Ok(mut rows) = connection.prepare(&query)
                && let Ok(iter) = rows.query_map([], |row| row.get::<_, String>(0))
            {
                values.extend(
                    iter.filter_map(Result::ok)
                        .filter_map(|text| serde_json::from_str(&text).ok()),
                );
            }
        }
    }
    Ok(values)
}

fn parse_sqlite_generic(path: &Path, range: Option<&TimeRange>) -> Result<UsageRecord, String> {
    let values = sqlite_json_values(path)?;
    let mut attribution = UsageAccumulator::default();
    let mut seen = HashSet::new();
    for value in &values {
        collect(value, &mut attribution, &mut seen, false, range, true);
    }
    finish(attribution.models, attribution.unattributed)
}

fn parse_sqlite_generic_events(
    path: &Path,
    fallback_timestamp: i64,
) -> Result<Vec<UsageEvent>, String> {
    let values = sqlite_json_values(path)?;
    let mut events = Vec::new();
    let mut seen = HashSet::new();
    for value in &values {
        collect_usage_events(
            value,
            None,
            None,
            fallback_timestamp,
            false,
            &mut seen,
            &mut events,
        );
    }
    if events.is_empty() {
        Err("session contains no token usage".to_owned())
    } else {
        Ok(events)
    }
}
