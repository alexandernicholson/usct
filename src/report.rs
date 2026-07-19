use crate::{
    app::pricing_id,
    cache::DependencyStamp,
    catalog::ModelsDevCatalog,
    discovery::{self, LocatedSession},
    domain::{Price, TokenUsage},
    session::{Harness, SessionMetadata, UsageEvent, parse_usage_session},
    table::{Alignment, Cell, Table},
};
use chrono::{DateTime, Datelike, Duration, NaiveDate, Timelike, Utc};
use jiff::{Timestamp, Zoned, civil::Date as CivilDate, tz::TimeZone};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap},
    fs,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    str::FromStr,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

// models.dev does not expose per-tier multipliers; current priority processing
// falls back to twice the standard price.
const CODEX_FAST_FALLBACK_MULTIPLIER: f64 = 2.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grouping {
    Daily,
    Weekly,
    Monthly,
    Session,
}

impl Grouping {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "daily" => Ok(Self::Daily),
            "weekly" => Ok(Self::Weekly),
            "monthly" => Ok(Self::Monthly),
            "session" => Ok(Self::Session),
            _ => Err(format!("unsupported report section '{value}'")),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Daily => "daily",
            Self::Weekly => "weekly",
            Self::Monthly => "monthly",
            Self::Session => "session",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CostMode {
    Auto,
    Calculate,
    Display,
}

impl CostMode {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "auto" => Ok(Self::Auto),
            "calculate" => Ok(Self::Calculate),
            "display" => Ok(Self::Display),
            _ => Err("--mode must be 'auto', 'calculate', or 'display'".to_owned()),
        }
    }
}

#[derive(Debug, Clone)]
pub enum ReportTimeZone {
    Utc,
    System,
    Named(TimeZone),
}

impl ReportTimeZone {
    fn cache_key(&self) -> String {
        match self {
            Self::Utc => "UTC".to_owned(),
            Self::System => std::env::var_os("TZ").map_or_else(
                || "system".to_owned(),
                |value| format!("system:{}", value.to_string_lossy()),
            ),
            Self::Named(timezone) => timezone
                .iana_name()
                .map(str::to_owned)
                .or_else(|| {
                    timezone
                        .to_fixed_offset()
                        .ok()
                        .map(|offset| format!("offset:{}", offset.seconds()))
                })
                .unwrap_or_else(|| format!("{timezone:?}")),
        }
    }

    fn is_system(&self) -> bool {
        matches!(self, Self::System)
    }
}

#[derive(Debug, Clone)]
pub struct ReportOptions {
    pub source: Option<Harness>,
    pub since: Option<String>,
    pub until: Option<String>,
    pub timezone: ReportTimeZone,
    pub no_cost: bool,
    pub by_agent: bool,
    pub session_id: Option<String>,
    pub project: Option<String>,
    pub instance: Option<String>,
    pub custom_prices: HashMap<String, Price>,
    pub instances: bool,
    pub project_aliases: HashMap<String, String>,
    pub descending: bool,
    pub breakdown: bool,
    pub codex_fast: bool,
    pub cost_mode: CostMode,
    pub debug: bool,
    pub debug_samples: usize,
    pub single_thread: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ReportCacheKey<'a> {
    package_version: &'static str,
    event_cache_version: u8,
    output: &'a str,
    sections: Vec<&'static str>,
    source: Option<&'static str>,
    roots: Vec<PathBuf>,
    since: Option<&'a str>,
    until: Option<&'a str>,
    timezone: String,
    no_cost: bool,
    by_agent: bool,
    session_id: Option<&'a str>,
    project: Option<&'a str>,
    instance: Option<&'a str>,
    custom_prices: BTreeMap<&'a str, Price>,
    instances: bool,
    project_aliases: BTreeMap<&'a str, &'a str>,
    descending: bool,
    codex_fast: bool,
    cost_mode: &'static str,
    catalog_present: bool,
}

pub fn output_cache_key(
    sections: &[Grouping],
    options: &ReportOptions,
    output: &str,
    catalog_path: &Path,
) -> Result<String, String> {
    let mut roots: Vec<_> = options
        .source
        .map_or_else(|| Harness::ALL.to_vec(), |source| vec![source])
        .into_iter()
        .flat_map(discovery::roots)
        .collect();
    roots.sort();
    roots.dedup();
    let timezone = options.timezone.cache_key();
    let custom_prices = options
        .custom_prices
        .iter()
        .map(|(model, price)| (model.as_str(), *price))
        .collect();
    let project_aliases = options
        .project_aliases
        .iter()
        .map(|(project, alias)| (project.as_str(), alias.as_str()))
        .collect();
    serde_json::to_string(&ReportCacheKey {
        package_version: env!("CARGO_PKG_VERSION"),
        event_cache_version: EVENT_CACHE_VERSION,
        output,
        sections: sections.iter().map(|section| section.name()).collect(),
        source: options.source.map(Harness::name),
        roots,
        since: options.since.as_deref(),
        until: options.until.as_deref(),
        timezone,
        no_cost: options.no_cost,
        by_agent: options.by_agent,
        session_id: options.session_id.as_deref(),
        project: options.project.as_deref(),
        instance: options.instance.as_deref(),
        custom_prices,
        instances: options.instances,
        project_aliases,
        descending: options.descending,
        codex_fast: options.codex_fast,
        cost_mode: match options.cost_mode {
            CostMode::Auto => "auto",
            CostMode::Calculate => "calculate",
            CostMode::Display => "display",
        },
        catalog_present: catalog_path.is_file(),
    })
    .map_err(|error| format!("cannot encode report cache key: {error}"))
}

#[derive(Debug, Clone)]
struct Event {
    timestamp_ms: i64,
    agent: &'static str,
    session: Arc<str>,
    session_title: Option<Arc<str>>,
    path: Arc<PathBuf>,
    instance: Arc<str>,
    model: String,
    usage: TokenUsage,
    cost: f64,
}

const EVENT_CACHE_VERSION: u8 = 8;

type FileStamp = DependencyStamp;

struct RawEventSession {
    harness: Harness,
    path: PathBuf,
    metadata: SessionMetadata,
    events: Vec<UsageEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize, bincode::Encode, bincode::Decode)]
struct CachedEventSession {
    file: FileStamp,
    harness: String,
    metadata: SessionMetadata,
    events: Vec<UsageEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize, bincode::Encode, bincode::Decode)]
struct EventIndex {
    version: u8,
    source: String,
    roots: Vec<PathBuf>,
    watchers: Vec<FileStamp>,
    sessions: Vec<CachedEventSession>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Totals {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
    pub total_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_cost: Option<f64>,
}

impl Totals {
    fn empty(no_cost: bool) -> Self {
        Self {
            total_cost: (!no_cost).then_some(0.0),
            ..Self::default()
        }
    }
    fn add(&mut self, usage: TokenUsage, cost: f64, no_cost: bool) {
        self.input_tokens = self.input_tokens.saturating_add(usage.input);
        self.output_tokens = self
            .output_tokens
            .saturating_add(usage.output.saturating_add(usage.reasoning));
        self.cache_creation_tokens = self.cache_creation_tokens.saturating_add(usage.cache_write);
        self.cache_read_tokens = self.cache_read_tokens.saturating_add(usage.cache_read);
        self.total_tokens = self.total_tokens.saturating_add(
            usage
                .input
                .saturating_add(usage.output)
                .saturating_add(usage.cache_read)
                .saturating_add(usage.cache_write)
                .saturating_add(usage.reasoning),
        );
        if !no_cost {
            *self.total_cost.get_or_insert(0.0) += cost;
        }
    }

    fn merge(&mut self, other: &Self, no_cost: bool) {
        self.input_tokens = self.input_tokens.saturating_add(other.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(other.output_tokens);
        self.cache_creation_tokens = self
            .cache_creation_tokens
            .saturating_add(other.cache_creation_tokens);
        self.cache_read_tokens = self
            .cache_read_tokens
            .saturating_add(other.cache_read_tokens);
        self.total_tokens = self.total_tokens.saturating_add(other.total_tokens);
        if !no_cost {
            *self.total_cost.get_or_insert(0.0) += other.total_cost.unwrap_or(0.0);
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelBreakdown {
    pub model_name: String,
    #[serde(flatten)]
    pub totals: Totals,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentBreakdown {
    pub agent: String,
    #[serde(flatten)]
    pub totals: Totals,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportRow {
    pub period: String,
    pub agent: String,
    #[serde(flatten)]
    pub totals: Totals,
    pub models_used: Vec<String>,
    pub model_breakdowns: Vec<ModelBreakdown>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_breakdowns: Option<Vec<AgentBreakdown>>,
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SectionReport {
    #[serde(skip)]
    pub name: String,
    pub rows: Vec<ReportRow>,
    pub totals: Totals,
}

pub struct GeneratedReports {
    pub reports: Vec<SectionReport>,
    pub dependencies: Vec<DependencyStamp>,
}

#[derive(Default)]
struct RowBuilder {
    agents: BTreeMap<&'static str, Totals>,
    models: BTreeMap<String, Totals>,
    totals: Totals,
    paths: HashMap<usize, Arc<PathBuf>>,
    session_title: Option<String>,
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum GroupPeriod {
    Daily(i32, u32, u32),
    Weekly(i32, u32, u32),
    Monthly(i32, u32),
    Session(Arc<str>),
}

impl GroupPeriod {
    fn render(&self) -> String {
        match self {
            Self::Daily(year, month, day) | Self::Weekly(year, month, day) => {
                format!("{year:04}-{month:02}-{day:02}")
            }
            Self::Monthly(year, month) => format!("{year:04}-{month:02}"),
            Self::Session(session) => session.to_string(),
        }
    }
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum GroupDiscriminator {
    Agent(&'static str),
    Instance(Arc<str>),
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct GroupKey {
    period: GroupPeriod,
    discriminator: Option<GroupDiscriminator>,
}

pub fn generate(
    sections: &[Grouping],
    options: &ReportOptions,
    catalog_path: &Path,
) -> Result<Vec<SectionReport>, String> {
    generate_with_dependencies(sections, options, catalog_path).map(|generated| generated.reports)
}

pub fn generate_with_dependencies(
    sections: &[Grouping],
    options: &ReportOptions,
    catalog_path: &Path,
) -> Result<GeneratedReports, String> {
    let (events, dependencies) = load_events(options, catalog_path)?;
    let reports = sections
        .iter()
        .map(|section| group_events(*section, &events, options))
        .collect::<Result<_, _>>()?;
    Ok(GeneratedReports {
        reports,
        dependencies,
    })
}

fn load_raw_events(
    source: Option<Harness>,
    catalog_path: &Path,
    single_thread: bool,
) -> Result<(Vec<RawEventSession>, Vec<DependencyStamp>), String> {
    let source_name = source.map_or("auto", Harness::name);
    let mut roots: Vec<PathBuf> = source
        .map_or_else(|| Harness::ALL.to_vec(), |source| vec![source])
        .into_iter()
        .flat_map(discovery::roots)
        .collect();
    roots.sort();
    roots.dedup();
    let cache_path = event_cache_path(catalog_path, source_name);
    let cached = fs::read(&cache_path)
        .ok()
        .and_then(|bytes| decode_event_index_bytes(&bytes));
    let cache_is_current = cached.as_ref().is_some_and(|index| {
        index.version == EVENT_CACHE_VERSION
            && index.source == source_name
            && index.roots == roots
            && index.watchers.iter().all(DependencyStamp::is_current)
            && index
                .sessions
                .iter()
                .all(|session| session.file.is_current())
    });
    if cache_is_current {
        return decode_event_index(cached.expect("current cache is present"));
    }

    let stale = cached.filter(|index| {
        index.version == EVENT_CACHE_VERSION && index.source == source_name && index.roots == roots
    });
    let stale_by_path: HashMap<PathBuf, CachedEventSession> = stale
        .map(|index| {
            index
                .sessions
                .into_iter()
                .map(|session| (session.file.path.clone(), session))
                .collect()
        })
        .unwrap_or_default();
    let located = match discovery::all(source) {
        Ok(located) => located,
        Err(error) if error == discovery::NO_SESSIONS_ERROR => Vec::new(),
        Err(error) => return Err(error),
    };
    let parse = |located: &LocatedSession| -> Result<CachedEventSession, String> {
        let file = file_stamp(&located.path)?;
        if let Some(cached) = stale_by_path.get(&located.path)
            && cached.file == file
            && cached.harness == located.harness.name()
        {
            return Ok(cached.clone());
        }
        let (metadata, events) = match parse_usage_session(located.harness, &located.path) {
            Ok(session) => (session.metadata, session.events),
            Err(error) if error == "session contains no token usage" => {
                (SessionMetadata::default(), Vec::new())
            }
            Err(error) => return Err(format!("{}: {error}", located.path.display())),
        };
        Ok(CachedEventSession {
            file,
            harness: located.harness.name().to_owned(),
            metadata,
            events,
        })
    };
    let sessions = if !single_thread && located.len() >= 8 {
        let worker_count = std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(1)
            .min(located.len());
        let chunk_size = located.len().div_ceil(worker_count);
        std::thread::scope(|scope| -> Result<Vec<CachedEventSession>, String> {
            let parse = &parse;
            let handles: Vec<_> = located
                .chunks(chunk_size)
                .map(|chunk| {
                    scope.spawn(move || {
                        chunk
                            .iter()
                            .map(parse)
                            .collect::<Result<Vec<CachedEventSession>, String>>()
                    })
                })
                .collect();
            let mut sessions = Vec::with_capacity(located.len());
            for handle in handles {
                sessions.extend(
                    handle
                        .join()
                        .map_err(|_| "event parser worker panicked".to_owned())??,
                );
            }
            Ok(sessions)
        })?
    } else {
        located.iter().map(parse).collect::<Result<Vec<_>, _>>()?
    };
    let mut watch_paths: Vec<PathBuf> = roots
        .iter()
        .cloned()
        .filter_map(discovery::nearest_existing)
        .collect();
    watch_paths.extend(
        located
            .iter()
            .filter_map(|session| session.path.parent().map(Path::to_path_buf)),
    );
    watch_paths.sort();
    watch_paths.dedup();
    let watchers = watch_paths
        .into_iter()
        .filter_map(|path| file_stamp(&path).ok())
        .collect();
    let index = EventIndex {
        version: EVENT_CACHE_VERSION,
        source: source_name.to_owned(),
        roots,
        watchers,
        sessions,
    };
    save_event_index(&cache_path, &index);
    decode_event_index(index)
}

fn decode_event_index(
    index: EventIndex,
) -> Result<(Vec<RawEventSession>, Vec<DependencyStamp>), String> {
    let mut dependencies = index.watchers;
    dependencies.extend(index.sessions.iter().map(|session| session.file.clone()));
    let mut sessions = Vec::with_capacity(index.sessions.len());
    for session in index.sessions {
        sessions.push(RawEventSession {
            harness: Harness::from_str(&session.harness)?,
            path: session.file.path,
            metadata: session.metadata,
            events: session.events,
        });
    }
    Ok((sessions, dependencies))
}

fn event_cache_path(catalog_path: &Path, source: &str) -> PathBuf {
    catalog_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!("events-{source}.bin"))
}

fn decode_event_index_bytes(bytes: &[u8]) -> Option<EventIndex> {
    let (index, consumed) = bincode::decode_from_slice(bytes, bincode::config::standard()).ok()?;
    (consumed == bytes.len()).then_some(index)
}

fn file_stamp(path: &Path) -> Result<FileStamp, String> {
    DependencyStamp::capture(path)
}

fn save_event_index(path: &Path, index: &EventIndex) {
    let Ok(bytes) = bincode::encode_to_vec(index, bincode::config::standard()) else {
        return;
    };
    let Some(parent) = path.parent() else {
        return;
    };
    if fs::create_dir_all(parent).is_err() {
        return;
    }
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    if fs::write(&temporary, bytes).is_ok() {
        let _ = fs::rename(temporary, path);
    }
}

fn load_events(
    options: &ReportOptions,
    catalog_path: &Path,
) -> Result<(Vec<Event>, Vec<DependencyStamp>), String> {
    let (parsed, mut dependencies) =
        load_raw_events(options.source, catalog_path, options.single_thread)?;
    let (start, end) = date_bounds(options)?;
    let mut catalog = None;
    let mut title_resolver = SessionTitleResolver::default();
    let event_capacity = parsed.iter().map(|session| session.events.len()).sum();
    let mut events = Vec::with_capacity(event_capacity);
    let mut debug_emitted = 0;
    for parsed_session in parsed {
        let RawEventSession {
            harness,
            path,
            metadata,
            events: usage_events,
        } = parsed_session;
        if options
            .project
            .as_ref()
            .is_some_and(|project| !path.to_string_lossy().contains(project))
            || options
                .instance
                .as_ref()
                .is_some_and(|instance| !path.to_string_lossy().contains(instance))
        {
            continue;
        }
        let session = metadata.id.unwrap_or_else(|| {
            path.file_stem()
                .and_then(|value| value.to_str())
                .unwrap_or("unknown")
                .to_owned()
        });
        let session_title = metadata
            .title
            .and_then(|title| clean_session_title(title, &session))
            .or_else(|| title_resolver.resolve(harness, &path, &session));
        if options.session_id.as_ref().is_some_and(|id| {
            id != &session
                && !path.to_string_lossy().contains(id)
                && session_title
                    .as_ref()
                    .is_none_or(|title| !title.contains(id))
        }) {
            continue;
        }
        let raw_instance = path
            .parent()
            .and_then(Path::file_name)
            .and_then(|value| value.to_str())
            .unwrap_or("unknown");
        let instance: Arc<str> = options
            .project_aliases
            .get(raw_instance)
            .cloned()
            .unwrap_or_else(|| raw_instance.to_owned())
            .into();
        let agent = harness.name();
        let session: Arc<str> = session.into();
        let session_title = session_title.map(Arc::<str>::from);
        let path = Arc::new(path);
        for item in usage_events {
            if start.is_some_and(|start| item.timestamp_ms < start)
                || end.is_some_and(|end| item.timestamp_ms >= end)
            {
                continue;
            }
            let should_calculate = options.cost_mode == CostMode::Calculate
                || (options.cost_mode == CostMode::Auto && item.reported_cost.is_none());
            let cost = if options.no_cost {
                0.0
            } else if !should_calculate {
                let cost = item.reported_cost.unwrap_or(0.0);
                if options.debug && debug_emitted < options.debug_samples {
                    eprintln!(
                        "usct: debug: source={} model={} cost_source=reported tokens={} cost={cost:.12} path={}",
                        harness.name(),
                        item.model,
                        item.usage.total_tokens(),
                        path.display()
                    );
                    debug_emitted += 1;
                }
                cost
            } else {
                let id = pricing_id(harness, &item.model);
                let custom_price = options
                    .custom_prices
                    .get(&id)
                    .or_else(|| options.custom_prices.get(&item.model))
                    .copied();
                let (price, price_source) = if let Some(price) = custom_price {
                    (price, "config")
                } else {
                    if catalog.is_none() {
                        catalog = Some(
                            ModelsDevCatalog::from_path(catalog_path)
                                .map_err(|error| format!("{error}; run 'usct update'"))?,
                        );
                    }
                    match catalog.as_ref().and_then(|catalog| catalog.find(&id)) {
                        Some(price) => (price, "models.dev"),
                        None => {
                            if options.debug && debug_emitted < options.debug_samples {
                                eprintln!(
                                    "usct: debug: source={} model={} pricing_id={} cost_source=missing path={}",
                                    harness.name(),
                                    item.model,
                                    id,
                                    path.display()
                                );
                                debug_emitted += 1;
                            }
                            (Price::ZERO, "missing")
                        }
                    }
                };
                let standard_cost = price.cost(item.usage);
                let cost = if harness == Harness::Codex && options.codex_fast {
                    standard_cost * CODEX_FAST_FALLBACK_MULTIPLIER
                } else {
                    standard_cost
                };
                if options.debug && debug_emitted < options.debug_samples {
                    eprintln!(
                        "usct: debug: source={} model={} pricing_id={} cost_source={} tokens={} cost={cost:.12} path={}",
                        harness.name(),
                        item.model,
                        id,
                        price_source,
                        item.usage.total_tokens(),
                        path.display()
                    );
                    debug_emitted += 1;
                }
                cost
            };
            events.push(Event {
                timestamp_ms: item.timestamp_ms,
                agent,
                session: Arc::clone(&session),
                session_title: session_title.as_ref().map(Arc::clone),
                path: Arc::clone(&path),
                instance: Arc::clone(&instance),
                model: item.model,
                usage: item.usage,
                cost,
            });
        }
    }
    events.sort_unstable_by_key(|event| event.timestamp_ms);
    dependencies.extend(title_resolver.into_dependencies());
    if !options.no_cost
        && let Ok(catalog) = DependencyStamp::capture(catalog_path)
    {
        dependencies.push(catalog);
    }
    if options.timezone.is_system()
        && let Ok(local_timezone) = DependencyStamp::capture(Path::new("/etc/localtime"))
    {
        dependencies.push(local_timezone);
    }
    dependencies.sort_unstable_by(|left, right| left.path.cmp(&right.path));
    dependencies.dedup_by(|left, right| left.path == right.path);
    Ok((events, dependencies))
}

#[derive(Default)]
struct SessionTitleResolver {
    codex_indexes: HashMap<PathBuf, HashMap<String, String>>,
    opencode_files: HashMap<PathBuf, Option<String>>,
    dependencies: BTreeMap<PathBuf, DependencyStamp>,
}

impl SessionTitleResolver {
    fn resolve(&mut self, harness: Harness, path: &Path, session_id: &str) -> Option<String> {
        let title = match harness {
            Harness::Codex => self.codex_title(path, session_id),
            Harness::OpenCode => self.opencode_title(path, session_id),
            _ => None,
        }?;
        clean_session_title(title, session_id)
    }

    fn track(&mut self, path: &Path) {
        let dependency = DependencyStamp::capture(path).ok().or_else(|| {
            discovery::nearest_existing(path.to_path_buf())
                .and_then(|existing| DependencyStamp::capture(&existing).ok())
        });
        if let Some(dependency) = dependency {
            self.dependencies
                .insert(dependency.path.clone(), dependency);
        }
    }

    fn into_dependencies(self) -> impl Iterator<Item = DependencyStamp> {
        self.dependencies.into_values()
    }

    fn codex_title(&mut self, path: &Path, session_id: &str) -> Option<String> {
        let index_path = codex_index_path(path)?;
        self.track(&index_path);
        self.codex_indexes
            .entry(index_path.clone())
            .or_insert_with(|| load_codex_titles(&index_path))
            .get(session_id)
            .cloned()
    }

    fn opencode_title(&mut self, path: &Path, session_id: &str) -> Option<String> {
        let title_path = opencode_title_path(path, session_id)?;
        self.track(&title_path);
        self.opencode_files
            .entry(title_path.clone())
            .or_insert_with(|| load_opencode_title(&title_path))
            .clone()
    }
}

#[derive(Deserialize)]
struct CodexTitleRow {
    id: String,
    thread_name: String,
}

fn load_codex_titles(path: &Path) -> HashMap<String, String> {
    let Ok(file) = fs::File::open(path) else {
        return HashMap::new();
    };
    BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter_map(|line| serde_json::from_str::<CodexTitleRow>(&line).ok())
        .map(|row| (row.id, row.thread_name))
        .collect()
}

fn codex_index_path(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .find(|ancestor| {
            ancestor
                .file_name()
                .is_some_and(|name| name == "sessions" || name == "archived_sessions")
        })
        .and_then(Path::parent)
        .map(|root| root.join("session_index.jsonl"))
}

fn opencode_title_path(path: &Path, session_id: &str) -> Option<PathBuf> {
    path.ancestors()
        .find(|ancestor| ancestor.file_name().is_some_and(|name| name == "storage"))
        .map(|storage| storage.join("session").join(format!("{session_id}.json")))
}

fn load_opencode_title(path: &Path) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(&fs::read(path).ok()?).ok()?;
    value.get("title")?.as_str().map(str::to_owned)
}

fn clean_session_title(title: String, session_id: &str) -> Option<String> {
    let title = title.trim();
    (!title.is_empty() && title != session_id).then(|| title.to_owned())
}

fn group_events(
    grouping: Grouping,
    events: &[Event],
    options: &ReportOptions,
) -> Result<SectionReport, String> {
    let mut groups: BTreeMap<GroupKey, RowBuilder> = BTreeMap::new();
    for event in events {
        let period = period_key(grouping, event, &options.timezone)?;
        let discriminator = if grouping == Grouping::Session {
            Some(GroupDiscriminator::Agent(event.agent))
        } else if options.instances {
            Some(GroupDiscriminator::Instance(Arc::clone(&event.instance)))
        } else {
            None
        };
        let row = groups
            .entry(GroupKey {
                period,
                discriminator,
            })
            .or_default();
        row.totals.add(event.usage, event.cost, options.no_cost);
        row.agents
            .entry(event.agent)
            .or_default()
            .add(event.usage, event.cost, options.no_cost);
        if !row.models.contains_key(event.model.as_str()) {
            row.models.insert(event.model.clone(), Totals::default());
        }
        row.models
            .get_mut(event.model.as_str())
            .expect("model inserted")
            .add(event.usage, event.cost, options.no_cost);
        row.paths
            .entry(Arc::as_ptr(&event.path) as usize)
            .or_insert_with(|| Arc::clone(&event.path));
        if grouping == Grouping::Session && row.session_title.is_none() {
            row.session_title = event.session_title.as_deref().map(str::to_owned);
        }
    }
    let mut totals = Totals::empty(options.no_cost);
    let mut rows = Vec::with_capacity(groups.len());
    for (key, group) in groups {
        let RowBuilder {
            agents,
            models,
            totals: row_totals,
            paths,
            session_title,
        } = group;
        let period = key.period.render();
        let instance = match key.discriminator {
            Some(GroupDiscriminator::Instance(instance)) => Some(instance),
            _ => None,
        };
        totals.merge(&row_totals, options.no_cost);
        let agent = if agents.len() == 1 {
            agents.keys().next().copied().unwrap_or_default().to_owned()
        } else {
            "all".to_owned()
        };
        let models_used = models.keys().cloned().collect();
        let model_breakdowns = models
            .into_iter()
            .map(|(model_name, totals)| ModelBreakdown { model_name, totals })
            .collect();
        let agent_breakdowns = options.by_agent.then(|| {
            agents
                .into_iter()
                .map(|(agent, totals)| AgentBreakdown {
                    agent: agent.to_owned(),
                    totals,
                })
                .collect()
        });
        let mut paths: Vec<_> = paths.into_values().collect();
        paths.sort_unstable_by(|left, right| left.as_path().cmp(right.as_path()));
        paths.dedup_by(|left, right| left.as_path() == right.as_path());
        let path_count = paths.len();
        let mut metadata = match grouping {
            Grouping::Session => {
                let paths: Vec<_> = paths
                    .into_iter()
                    .map(|path| path.display().to_string())
                    .collect();
                serde_json::json!({"sessionId": period, "paths": paths})
            }
            _ if instance.is_some() => serde_json::json!({
                "sessionCount": path_count,
                "instance": instance.as_deref()
            }),
            _ => serde_json::json!({"sessionCount": path_count}),
        };
        if let Some(session_title) = session_title {
            metadata
                .as_object_mut()
                .expect("report metadata is an object")
                .insert(
                    "sessionTitle".to_owned(),
                    serde_json::Value::String(session_title),
                );
        }
        rows.push(ReportRow {
            period,
            agent,
            totals: row_totals,
            models_used,
            model_breakdowns,
            agent_breakdowns,
            metadata,
        });
    }
    if options.descending {
        rows.reverse();
    }
    Ok(SectionReport {
        name: grouping.name().to_owned(),
        rows,
        totals,
    })
}

#[derive(Clone, Copy)]
struct LocalParts {
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
    offset_seconds: i32,
}

fn period_key(
    grouping: Grouping,
    event: &Event,
    timezone: &ReportTimeZone,
) -> Result<GroupPeriod, String> {
    if grouping == Grouping::Session {
        return Ok(GroupPeriod::Session(Arc::clone(&event.session)));
    }
    let local = local_parts(event.timestamp_ms, timezone)?;
    match grouping {
        Grouping::Daily => Ok(GroupPeriod::Daily(local.year, local.month, local.day)),
        Grouping::Monthly => Ok(GroupPeriod::Monthly(local.year, local.month)),
        Grouping::Weekly => {
            let date = NaiveDate::from_ymd_opt(local.year, local.month, local.day)
                .expect("valid local date");
            let start = date - Duration::days(i64::from(date.weekday().num_days_from_monday()));
            Ok(GroupPeriod::Weekly(
                start.year(),
                start.month(),
                start.day(),
            ))
        }
        Grouping::Session => unreachable!(),
    }
}

fn date_bounds(options: &ReportOptions) -> Result<(Option<i64>, Option<i64>), String> {
    let start = options
        .since
        .as_deref()
        .map(|value| date_boundary(value, &options.timezone))
        .transpose()?;
    let end = options
        .until
        .as_deref()
        .map(|value| {
            let date = parse_date(value)? + Duration::days(1);
            local_midnight(date, &options.timezone, value)
        })
        .transpose()?;
    if start.zip(end).is_some_and(|(start, end)| end <= start) {
        return Err("--until must not precede --since".to_owned());
    }
    Ok((start, end))
}

fn date_boundary(value: &str, timezone: &ReportTimeZone) -> Result<i64, String> {
    if let Ok(value) = DateTime::parse_from_rfc3339(value) {
        return Ok(value.timestamp_millis());
    }
    local_midnight(parse_date(value)?, timezone, value)
}

fn local_midnight(date: NaiveDate, timezone: &ReportTimeZone, value: &str) -> Result<i64, String> {
    match timezone {
        ReportTimeZone::Utc => Ok(date
            .and_hms_opt(0, 0, 0)
            .expect("valid midnight")
            .and_utc()
            .timestamp_millis()),
        ReportTimeZone::Named(timezone) => named_local_midnight(date, timezone, value),
        ReportTimeZone::System => system_local_midnight(date, value),
    }
}

fn named_local_midnight(date: NaiveDate, timezone: &TimeZone, value: &str) -> Result<i64, String> {
    let year = i16::try_from(date.year())
        .map_err(|_| format!("date '{value}' is outside the supported range"))?;
    let month = i8::try_from(date.month()).expect("valid month");
    let day = i8::try_from(date.day()).expect("valid day");
    CivilDate::new(year, month, day)
        .and_then(|date| date.at(0, 0, 0, 0).to_zoned(timezone.clone()))
        .map(|value| value.timestamp().as_millisecond())
        .map_err(|error| format!("cannot resolve local midnight for '{value}': {error}"))
}

#[cfg(unix)]
fn system_local_midnight(date: NaiveDate, value: &str) -> Result<i64, String> {
    // SAFETY: A zeroed `tm` is valid, and `mktime` only mutates the supplied value.
    let mut local: libc::tm = unsafe { std::mem::zeroed() };
    local.tm_year = date.year() - 1900;
    local.tm_mon = i32::try_from(date.month0()).expect("valid month");
    local.tm_mday = i32::try_from(date.day()).expect("valid day");
    local.tm_isdst = -1;
    // SAFETY: `local` points to a valid, initialized `tm`.
    let seconds = unsafe { libc::mktime(&mut local) };
    if seconds == -1 {
        return Err(format!("cannot resolve local midnight for '{value}'"));
    }
    (seconds as i64)
        .checked_mul(1_000)
        .ok_or_else(|| format!("date '{value}' is outside the supported range"))
}

#[cfg(not(unix))]
fn system_local_midnight(date: NaiveDate, value: &str) -> Result<i64, String> {
    named_local_midnight(date, &TimeZone::system(), value)
}

fn local_parts(timestamp_ms: i64, timezone: &ReportTimeZone) -> Result<LocalParts, String> {
    match timezone {
        ReportTimeZone::Utc => {
            let local = DateTime::<Utc>::from_timestamp_millis(timestamp_ms)
                .ok_or_else(|| format!("invalid event timestamp {timestamp_ms}"))?;
            Ok(LocalParts {
                year: local.year(),
                month: local.month(),
                day: local.day(),
                hour: local.hour(),
                minute: local.minute(),
                second: local.second(),
                offset_seconds: 0,
            })
        }
        ReportTimeZone::Named(timezone) => {
            let local = named_zoned_timestamp(timestamp_ms, timezone)?;
            Ok(LocalParts {
                year: i32::from(local.year()),
                month: u32::try_from(local.month()).expect("valid Jiff month"),
                day: u32::try_from(local.day()).expect("valid Jiff day"),
                hour: u32::try_from(local.hour()).expect("valid Jiff hour"),
                minute: u32::try_from(local.minute()).expect("valid Jiff minute"),
                second: u32::try_from(local.second()).expect("valid Jiff second"),
                offset_seconds: local.offset().seconds(),
            })
        }
        ReportTimeZone::System => system_local_parts(timestamp_ms),
    }
}

#[cfg(unix)]
fn system_local_parts(timestamp_ms: i64) -> Result<LocalParts, String> {
    let seconds_i64 = timestamp_ms.div_euclid(1_000);
    let seconds = seconds_i64 as libc::time_t;
    if seconds as i64 != seconds_i64 {
        return Err(format!("invalid event timestamp {timestamp_ms}"));
    }
    let mut local = std::mem::MaybeUninit::<libc::tm>::uninit();
    // SAFETY: Both pointers are valid, and `localtime_r` initializes `local` on success.
    let result = unsafe { libc::localtime_r(&seconds, local.as_mut_ptr()) };
    if result.is_null() {
        return Err(format!("invalid event timestamp {timestamp_ms}"));
    }
    // SAFETY: A non-null result from `localtime_r` points to the initialized output.
    let local = unsafe { local.assume_init() };
    let mut utc_interpretation = local;
    // SAFETY: `utc_interpretation` is a valid `tm` produced by `localtime_r`.
    let interpreted_seconds = unsafe { libc::timegm(&mut utc_interpretation) };
    let offset_seconds = interpreted_seconds
        .checked_sub(seconds)
        .and_then(|offset| i32::try_from(offset).ok())
        .ok_or_else(|| format!("invalid local offset for timestamp {timestamp_ms}"))?;
    Ok(LocalParts {
        year: local.tm_year + 1900,
        month: u32::try_from(local.tm_mon + 1).expect("valid system month"),
        day: u32::try_from(local.tm_mday).expect("valid system day"),
        hour: u32::try_from(local.tm_hour).expect("valid system hour"),
        minute: u32::try_from(local.tm_min).expect("valid system minute"),
        second: u32::try_from(local.tm_sec).expect("valid system second"),
        offset_seconds,
    })
}

#[cfg(not(unix))]
fn system_local_parts(timestamp_ms: i64) -> Result<LocalParts, String> {
    let timezone = TimeZone::system();
    let local = named_zoned_timestamp(timestamp_ms, &timezone)?;
    Ok(LocalParts {
        year: i32::from(local.year()),
        month: u32::try_from(local.month()).expect("valid Jiff month"),
        day: u32::try_from(local.day()).expect("valid Jiff day"),
        hour: u32::try_from(local.hour()).expect("valid Jiff hour"),
        minute: u32::try_from(local.minute()).expect("valid Jiff minute"),
        second: u32::try_from(local.second()).expect("valid Jiff second"),
        offset_seconds: local.offset().seconds(),
    })
}

fn named_zoned_timestamp(timestamp_ms: i64, timezone: &TimeZone) -> Result<Zoned, String> {
    Timestamp::from_millisecond(timestamp_ms)
        .map(|timestamp| timestamp.to_zoned(timezone.clone()))
        .map_err(|error| format!("invalid event timestamp {timestamp_ms}: {error}"))
}

fn format_block_timestamp(timestamp_ms: i64, timezone: &ReportTimeZone, compact: bool) -> String {
    if let ReportTimeZone::Named(timezone) = timezone {
        let value = named_zoned_timestamp(timestamp_ms, timezone).expect("valid block timestamp");
        let format = if compact {
            "%m-%d %H:%M"
        } else {
            "%Y-%m-%dT%H:%M:%S%.f%:z"
        };
        return jiff::fmt::strtime::format(format, &value)
            .expect("valid static time format")
            .to_string();
    }
    let value = local_parts(timestamp_ms, timezone).expect("valid block timestamp");
    if compact {
        return format!(
            "{:02}-{:02} {:02}:{:02}",
            value.month, value.day, value.hour, value.minute
        );
    }
    let offset = value.offset_seconds;
    let sign = if offset < 0 { '-' } else { '+' };
    let offset = offset.unsigned_abs();
    let offset_hours = offset / 3_600;
    let offset_minutes = offset % 3_600 / 60;
    let offset_seconds = offset % 60;
    let zone = if offset_seconds == 0 {
        format!("{sign}{offset_hours:02}:{offset_minutes:02}")
    } else {
        format!("{sign}{offset_hours:02}:{offset_minutes:02}:{offset_seconds:02}")
    };
    let milliseconds = timestamp_ms.rem_euclid(1_000);
    if milliseconds == 0 {
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}{zone}",
            value.year, value.month, value.day, value.hour, value.minute, value.second
        )
    } else {
        let fraction = format!("{milliseconds:03}");
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{}{zone}",
            value.year,
            value.month,
            value.day,
            value.hour,
            value.minute,
            value.second,
            fraction.trim_end_matches('0')
        )
    }
}

fn parse_date(value: &str) -> Result<NaiveDate, String> {
    NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .or_else(|_| NaiveDate::parse_from_str(value, "%Y%m%d"))
        .map_err(|_| format!("invalid date '{value}'"))
}

pub fn json_output(reports: &[SectionReport]) -> Result<String, String> {
    let mut object = serde_json::Map::new();
    for report in reports {
        object.insert(
            report.name.clone(),
            serde_json::to_value(&report.rows).map_err(|error| error.to_string())?,
        );
    }
    let mut totals = Totals::default();
    if let Some(report) = reports.first() {
        totals = report.totals.clone();
    }
    object.insert(
        "totals".to_owned(),
        serde_json::to_value(totals).map_err(|error| error.to_string())?,
    );
    serde_json::to_string_pretty(&object).map_err(|error| error.to_string())
}

pub fn table_output(
    reports: &[SectionReport],
    no_cost: bool,
    compact: bool,
    breakdown: bool,
    color: bool,
) -> String {
    let mut output = String::new();
    for (index, report) in reports.iter().enumerate() {
        let table = if compact {
            compact_report_table(report, no_cost, breakdown, color)
        } else {
            full_report_table(report, no_cost, breakdown, color)
        };
        if index > 0 {
            output.push_str("\n\n");
        }
        if reports.len() > 1 {
            output.push_str(&ansi("1", &report.name.to_ascii_uppercase(), color));
            if !table.is_empty() {
                output.push('\n');
            }
        }
        output.push_str(&table);
    }
    output
}

fn full_report_table(
    report: &SectionReport,
    no_cost: bool,
    breakdown: bool,
    color: bool,
) -> String {
    let breakdown_rows = if breakdown {
        report
            .rows
            .iter()
            .map(|row| row.model_breakdowns.len())
            .sum()
    } else {
        0
    };
    let row_capacity = 1 + report.rows.len() + breakdown_rows;
    let mut table = Table::with_capacity(
        &[
            Alignment::Left,
            Alignment::Left,
            Alignment::Left,
            Alignment::Right,
            Alignment::Right,
            Alignment::Right,
            Alignment::Right,
            Alignment::Right,
            Alignment::Right,
        ],
        row_capacity,
        color,
    );
    table.push([
        Cell::styled(
            if report.name == Grouping::Session.name() {
                "Session"
            } else {
                "Period"
            },
            "1",
        ),
        Cell::styled("Agent", "1"),
        if breakdown {
            Cell::styled("Model", "1")
        } else {
            Cell::empty()
        },
        Cell::styled("Input", "1"),
        Cell::styled("Output", "1"),
        Cell::styled("Cache Read", "1"),
        Cell::styled("Cache Write", "1"),
        Cell::styled("Total", "1"),
        if no_cost {
            Cell::empty()
        } else {
            Cell::styled("Cost", "1")
        },
    ]);
    for row in &report.rows {
        table.push([
            Cell::new(report_period_label(report, row)),
            Cell::new(row.agent.clone()),
            Cell::empty(),
            Cell::new(format_tokens(row.totals.input_tokens)),
            Cell::new(format_tokens(row.totals.output_tokens)),
            Cell::new(format_tokens(row.totals.cache_read_tokens)),
            Cell::new(format_tokens(row.totals.cache_creation_tokens)),
            Cell::new(format_tokens(row.totals.total_tokens)),
            if no_cost {
                Cell::empty()
            } else {
                Cell::styled(format_cost(row.totals.total_cost.unwrap_or(0.0)), "32")
            },
        ]);
        if breakdown {
            for model in &row.model_breakdowns {
                table.push([
                    Cell::empty(),
                    Cell::empty(),
                    Cell::new(format!("↳ {}", model.model_name)),
                    Cell::new(format_tokens(model.totals.input_tokens)),
                    Cell::new(format_tokens(model.totals.output_tokens)),
                    Cell::new(format_tokens(model.totals.cache_read_tokens)),
                    Cell::new(format_tokens(model.totals.cache_creation_tokens)),
                    Cell::new(format_tokens(model.totals.total_tokens)),
                    if no_cost {
                        Cell::empty()
                    } else {
                        Cell::styled(format_cost(model.totals.total_cost.unwrap_or(0.0)), "32")
                    },
                ]);
            }
        }
    }
    table.render()
}

fn compact_report_table(
    report: &SectionReport,
    no_cost: bool,
    breakdown: bool,
    color: bool,
) -> String {
    let breakdown_rows = if breakdown {
        report
            .rows
            .iter()
            .map(|row| row.model_breakdowns.len())
            .sum()
    } else {
        0
    };
    let row_capacity = report.rows.len() + breakdown_rows;
    let mut table = Table::with_capacity(
        &[
            Alignment::Left,
            Alignment::Left,
            Alignment::Right,
            Alignment::Right,
        ],
        row_capacity,
        color,
    );
    for row in &report.rows {
        table.push([
            Cell::styled(report_period_label(report, row), "36"),
            Cell::empty(),
            Cell::new(format_tokens(row.totals.total_tokens)),
            if no_cost {
                Cell::empty()
            } else {
                Cell::styled(format_cost(row.totals.total_cost.unwrap_or(0.0)), "32")
            },
        ]);
        if breakdown {
            for model in &row.model_breakdowns {
                table.push([
                    Cell::empty(),
                    Cell::new(format!("↳ {}", model.model_name)),
                    Cell::new(format_tokens(model.totals.total_tokens)),
                    if no_cost {
                        Cell::empty()
                    } else {
                        Cell::styled(format_cost(model.totals.total_cost.unwrap_or(0.0)), "32")
                    },
                ]);
            }
        }
    }
    table.render()
}

fn report_period_label<'a>(report: &SectionReport, row: &'a ReportRow) -> &'a str {
    if report.name == Grouping::Session.name() {
        row.metadata
            .get("sessionTitle")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(&row.period)
    } else {
        &row.period
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct BlockRow {
    start_time: String,
    end_time: String,
    active: bool,
    #[serde(flatten)]
    totals: Totals,
    models: Vec<ModelBreakdown>,
    #[serde(skip_serializing_if = "Option::is_none")]
    projected_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    projected_cost: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    token_limit: Option<u64>,
}

pub struct BlockOptions {
    pub session_length_hours: u32,
    pub active_only: bool,
    pub recent: bool,
    pub descending: bool,
    pub breakdown: bool,
    pub token_limit: Option<u64>,
    pub json: bool,
    pub compact: bool,
    pub color: bool,
}

pub fn blocks_output(
    options: &ReportOptions,
    block_options: &BlockOptions,
    catalog_path: &Path,
) -> Result<String, String> {
    if block_options.session_length_hours == 0 {
        return Err("--session-length must be greater than zero".to_owned());
    }
    let (events, _) = load_events(options, catalog_path)?;
    let duration_ms = i64::from(block_options.session_length_hours) * 3_600_000;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("system clock predates Unix epoch: {error}"))?
        .as_millis()
        .try_into()
        .map_err(|_| "system clock exceeds report range".to_owned())?;
    let mut raw: Vec<(i64, i64, RowBuilder)> = Vec::new();
    for event in events {
        let new_block = raw
            .last()
            .is_none_or(|(start, _, _)| event.timestamp_ms >= *start + duration_ms);
        if new_block {
            raw.push((
                event.timestamp_ms,
                event.timestamp_ms + duration_ms,
                RowBuilder::default(),
            ));
        }
        let (_, _, row) = raw.last_mut().expect("block inserted");
        row.totals.add(event.usage, event.cost, options.no_cost);
        row.models
            .entry(event.model)
            .or_default()
            .add(event.usage, event.cost, options.no_cost);
    }
    let recent_cutoff = now - 3 * 24 * 3_600_000;
    let mut rows: Vec<_> = raw
        .into_iter()
        .filter(|(start, end, _)| {
            (!block_options.active_only || (*start <= now && now < *end))
                && (!block_options.recent || *end >= recent_cutoff)
        })
        .map(|(start, end, row)| {
            let active = start <= now && now < end;
            let elapsed = (now - start).clamp(1, duration_ms) as f64;
            let scale = duration_ms as f64 / elapsed;
            let projected_tokens =
                active.then(|| (row.totals.total_tokens as f64 * scale).round() as u64);
            let projected_cost = active
                .then(|| row.totals.total_cost.map(|value| value * scale))
                .flatten();
            let compact = block_options.compact && !block_options.json;
            let start_time = format_block_timestamp(start, &options.timezone, compact);
            let end_time = format_block_timestamp(end, &options.timezone, compact);
            BlockRow {
                start_time,
                end_time,
                active,
                totals: row.totals,
                models: row
                    .models
                    .into_iter()
                    .map(|(model_name, totals)| ModelBreakdown { model_name, totals })
                    .collect(),
                projected_tokens,
                projected_cost,
                token_limit: block_options.token_limit,
            }
        })
        .collect();
    if block_options.descending {
        rows.reverse();
    }
    if block_options.json {
        let mut totals = Totals::empty(options.no_cost);
        for row in &rows {
            totals.merge(&row.totals, options.no_cost);
        }
        return serde_json::to_string_pretty(&serde_json::json!({
            "blocks": rows,
            "totals": totals
        }))
        .map_err(|error| error.to_string());
    }
    Ok(blocks_table(
        &rows,
        options.no_cost,
        block_options.breakdown,
        block_options.color,
    ))
}

fn blocks_table(rows: &[BlockRow], no_cost: bool, breakdown: bool, color: bool) -> String {
    let breakdown_rows = if breakdown {
        rows.iter().map(|row| row.models.len()).sum()
    } else {
        0
    };
    let row_capacity = rows.len() + breakdown_rows;
    let mut table = Table::with_capacity(
        &[
            Alignment::Left,
            Alignment::Left,
            Alignment::Left,
            Alignment::Right,
            Alignment::Right,
            Alignment::Left,
            Alignment::Left,
            Alignment::Left,
        ],
        row_capacity,
        color,
    );
    for row in rows {
        let active = if row.active {
            Cell::styled("active", "32;1")
        } else {
            Cell::empty()
        };
        let projected = row
            .projected_tokens
            .map(|value| Cell::new(format!("projected {}", format_tokens(value))))
            .unwrap_or_else(Cell::empty);
        let limit = row
            .token_limit
            .map(|limit| {
                let used = row.projected_tokens.unwrap_or(row.totals.total_tokens);
                Cell::new(format!(
                    "{:.1}% of limit",
                    used as f64 * 100.0 / limit as f64
                ))
            })
            .unwrap_or_else(Cell::empty);
        table.push([
            Cell::new(row.start_time.clone()),
            Cell::new(format!("– {}", row.end_time)),
            Cell::empty(),
            Cell::new(format_tokens(row.totals.total_tokens)),
            if no_cost {
                Cell::empty()
            } else {
                Cell::styled(format_cost(row.totals.total_cost.unwrap_or(0.0)), "32")
            },
            active,
            projected,
            limit,
        ]);
        if breakdown {
            for model in &row.models {
                table.push([
                    Cell::empty(),
                    Cell::empty(),
                    Cell::new(format!("↳ {}", model.model_name)),
                    Cell::new(format_tokens(model.totals.total_tokens)),
                    if no_cost {
                        Cell::empty()
                    } else {
                        Cell::styled(format_cost(model.totals.total_cost.unwrap_or(0.0)), "32")
                    },
                    Cell::empty(),
                    Cell::empty(),
                    Cell::empty(),
                ]);
            }
        }
    }
    table.render()
}

fn ansi(code: &str, value: &str, enabled: bool) -> String {
    if enabled {
        format!("\u{1b}[{code}m{value}\u{1b}[0m")
    } else {
        value.to_owned()
    }
}

pub fn format_tokens(value: u64) -> String {
    if value >= 1_000_000_000 {
        format!("{:.2}B", value as f64 / 1_000_000_000.0)
    } else if value >= 1_000_000 {
        format!("{:.2}M", value as f64 / 1_000_000.0)
    } else if value >= 1_000 {
        format!("{:.1}K", value as f64 / 1_000.0)
    } else {
        value.to_string()
    }
}

pub fn format_cost(value: f64) -> String {
    if value == 0.0 {
        "$0.00".to_owned()
    } else if value < 0.01 {
        format!("${value:.4}")
    } else {
        format!("${value:.2}")
    }
}
