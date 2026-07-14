use crate::{
    domain::{ModelUsage, TokenUsage, UsageRecord},
    time_range::{TimeRange, parse_timestamp},
};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{
    collections::HashSet,
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
}

impl Harness {
    pub const ALL: [Self; 7] = [
        Self::Claude,
        Self::Codex,
        Self::Pi,
        Self::Omp,
        Self::OpenCode,
        Self::Gemini,
        Self::Amp,
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

pub fn parse_session(harness: Harness, path: &Path) -> Result<UsageRecord, String> {
    parse_session_in_range(harness, path, None)
}

pub fn parse_session_in_range(
    harness: Harness,
    path: &Path,
    range: Option<&TimeRange>,
) -> Result<UsageRecord, String> {
    if path.extension().is_some_and(|ext| ext == "db") {
        return parse_sqlite(path, range);
    }
    match harness {
        Harness::Claude => parse_claude(path, range),
        Harness::Codex => parse_codex(path, range),
        Harness::Gemini => parse_generic(path, true, range),
        _ => parse_generic(path, false, range),
    }
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
    if path
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
                .or_else(|| object.get("tokenUsage").and_then(Value::as_object));
            if let Some(raw) = usage_object {
                let should_count = object
                    .get("id")
                    .and_then(Value::as_str)
                    .is_none_or(|identity| seen.insert(identity.to_owned()));
                if allowed && should_count {
                    attribution.add_usage(usage_from(raw, cached_is_subset));
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
    let raw_input = number(
        object,
        &[
            "input",
            "input_tokens",
            "inputTokens",
            "prompt_tokens",
            "promptTokenCount",
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
        ],
    );
    let raw_output = number(
        object,
        &[
            "output",
            "output_tokens",
            "outputTokens",
            "completion_tokens",
            "candidatesTokenCount",
        ],
    );
    let reasoning = number(
        object,
        &[
            "reasoning_output_tokens",
            "reasoning_tokens",
            "thoughtsTokenCount",
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
            ],
        ),
        reasoning,
    }
}

fn number(object: &Map<String, Value>, keys: &[&str]) -> u64 {
    keys.iter()
        .find_map(|key| object.get(*key).and_then(Value::as_u64))
        .unwrap_or(0)
}

fn find_string<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a str> {
    let object = value.as_object()?;
    keys.iter()
        .find_map(|key| object.get(*key).and_then(Value::as_str))
}

fn timestamp_ms(value: &Value) -> Option<i64> {
    let object = value.as_object()?;
    ["timestamp", "created_at", "createdAt", "time"]
        .into_iter()
        .find_map(|key| {
            let value = object.get(key)?;
            value
                .as_str()
                .and_then(parse_timestamp)
                .or_else(|| value.as_i64().map(timestamp_number))
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

fn parse_sqlite(path: &Path, range: Option<&TimeRange>) -> Result<UsageRecord, String> {
    let connection = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| format!("cannot open {}: {error}", path.display()))?;
    let mut statement = connection
        .prepare("SELECT name FROM sqlite_master WHERE type='table'")
        .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;
    let tables: Vec<String> = statement
        .query_map([], |row| row.get(0))
        .map_err(|e| e.to_string())?
        .filter_map(Result::ok)
        .collect();
    let mut values = Vec::new();
    for table in tables {
        let escaped = table.replace('"', "\"\"");
        let pragma = format!("PRAGMA table_info(\"{escaped}\")");
        let mut columns = connection.prepare(&pragma).map_err(|e| e.to_string())?;
        let text_columns: Vec<String> = columns
            .query_map([], |row| row.get::<_, String>(1))
            .map_err(|e| e.to_string())?
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
    let mut attribution = UsageAccumulator::default();
    let mut seen = HashSet::new();
    for value in &values {
        collect(value, &mut attribution, &mut seen, false, range, true);
    }
    finish(attribution.models, attribution.unattributed)
}
