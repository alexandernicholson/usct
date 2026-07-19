use crate::{
    domain::{PricedModelUsage, TokenUsage},
    session::ParserProgress,
    time_range::TimeRange,
};
use serde::{Deserialize, Serialize};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::time::UNIX_EPOCH;

use std::{
    env, fs,
    io::Write,
    path::{Path, PathBuf},
};

pub const MODELS_URL: &str = "https://models.dev/api.json";
const REPORT_CACHE_VERSION: u8 = 10;
const OUTPUT_CACHE_VERSION: u8 = 2;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, bincode::Encode, bincode::Decode)]
pub struct DependencyStamp {
    pub path: PathBuf,
    length: u64,
    modified_ns: u128,
}

impl DependencyStamp {
    pub fn capture(path: &Path) -> Result<Self, String> {
        let metadata = fs::metadata(path)
            .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;
        let modified_ns = metadata_modified_ns(&metadata);
        Ok(Self {
            path: path.to_path_buf(),
            length: metadata.len(),
            modified_ns,
        })
    }

    pub(crate) fn is_current(&self) -> bool {
        fs::metadata(&self.path).is_ok_and(|metadata| {
            self.length == metadata.len() && self.modified_ns == metadata_modified_ns(&metadata)
        })
    }
}

fn metadata_modified_ns(metadata: &fs::Metadata) -> u128 {
    metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_nanos())
}

#[derive(Debug, bincode::Encode, bincode::Decode)]
struct CachedOutput {
    version: u8,
    key: String,
    dependencies: Vec<DependencyStamp>,
    output: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct FileFingerprint {
    path: String,
    len: u64,
    mtime_ns: u128,
}

impl FileFingerprint {
    fn matches(&self, path: &Path) -> bool {
        Path::new(&self.path) == path && self.is_current()
    }

    fn is_current(&self) -> bool {
        fs::metadata(&self.path).is_ok_and(|metadata| {
            let modified_ns = metadata
                .modified()
                .ok()
                .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
                .map(|duration| duration.as_nanos());
            self.len == metadata.len() && modified_ns == Some(self.mtime_ns)
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedSession {
    pub source: String,
    pub models: Vec<PricedModelUsage>,
    pub usage: TokenUsage,
    pub cost_usd: f64,
    pub progress: Option<ParserProgress>,
    version: u8,
    file: FileFingerprint,
    catalog: FileFingerprint,
}

pub fn load_progress(path: &Path, range_key: &str, catalog_path: &Path) -> Option<ParserProgress> {
    let cache_path = progress_state_path(catalog_path, path, range_key)?;
    serde_json::from_slice(&fs::read(cache_path).ok()?).ok()
}

pub fn save_progress(progress: &ParserProgress, path: &Path, range_key: &str, catalog_path: &Path) {
    let Some(cache_path) = progress_state_path(catalog_path, path, range_key) else {
        return;
    };
    write_atomic(&cache_path, progress);
}

pub fn load_session(path: &Path, range_key: &str, catalog_path: &Path) -> Option<CachedSession> {
    let cache_path = session_state_path(catalog_path, path, range_key)?;
    let bytes = fs::read(cache_path).ok()?;
    let session: CachedSession = serde_json::from_slice(&bytes).ok()?;
    (session.version == 5 && session.file.matches(path) && session.catalog.matches(catalog_path))
        .then_some(session)
}

pub fn save_session(session: &CachedSession, path: &Path, range_key: &str, catalog_path: &Path) {
    let Some(cache_path) = session_state_path(catalog_path, path, range_key) else {
        return;
    };
    write_atomic(&cache_path, session);
}

pub struct SessionData {
    pub source: String,
    pub models: Vec<PricedModelUsage>,
    pub usage: TokenUsage,
    pub cost_usd: f64,
    pub progress: Option<ParserProgress>,
}

impl CachedSession {
    pub fn new(data: SessionData, path: &Path, catalog_path: &Path) -> Result<Self, String> {
        Ok(Self {
            source: data.source,
            models: data.models,
            usage: data.usage,
            cost_usd: data.cost_usd,
            progress: data.progress,
            version: 5,
            file: fingerprint(path)?,
            catalog: fingerprint(catalog_path)?,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedContribution {
    path: String,
    file: FileFingerprint,
    source: String,
    models: Vec<PricedModelUsage>,
    usage: TokenUsage,
    cost_usd: f64,
    progress: Option<ParserProgress>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CachedReport {
    pub sources: Vec<String>,
    pub session_count: usize,
    pub usage: TokenUsage,
    pub cost_usd: f64,
    pub range: Option<TimeRange>,
    version: u8,
    files: Vec<FileFingerprint>,
    directories: Vec<FileFingerprint>,
    catalog: FileFingerprint,
    contributions: Vec<CachedContribution>,
}

pub struct CacheContext<'a> {
    pub directory_paths: &'a [PathBuf],
    pub catalog_path: &'a Path,
}

impl CachedReport {
    pub fn new(
        sources: Vec<String>,
        session_count: usize,
        usage: TokenUsage,
        cost_usd: f64,
        range: Option<TimeRange>,
        contributions: &[(PathBuf, CachedSession)],
        context: CacheContext<'_>,
    ) -> Result<Self, String> {
        Ok(Self {
            session_count,
            sources,
            usage,
            cost_usd,
            range,
            version: REPORT_CACHE_VERSION,
            files: contributions
                .iter()
                .map(|(_, session)| session.file.clone())
                .collect(),
            directories: fingerprints(context.directory_paths)?,
            catalog: fingerprint(context.catalog_path)?,
            contributions: contributions
                .iter()
                .map(|(path, session)| CachedContribution {
                    path: path.display().to_string(),
                    file: session.file.clone(),
                    source: session.source.clone(),
                    models: session.models.clone(),
                    usage: session.usage,
                    cost_usd: session.cost_usd,
                    progress: session.progress.clone(),
                })
                .collect(),
        })
    }

    pub fn contribution(&self, path: &Path) -> Option<CachedSession> {
        let contribution = self
            .contributions
            .iter()
            .find(|contribution| contribution.path == path.display().to_string())?;
        Some(CachedSession {
            source: contribution.source.clone(),
            models: contribution.models.clone(),
            usage: contribution.usage,
            cost_usd: contribution.cost_usd,
            progress: contribution.progress.clone(),
            version: 5,
            file: contribution.file.clone(),
            catalog: self.catalog.clone(),
        })
    }

    pub fn reusable_contribution(&self, path: &Path) -> Option<CachedSession> {
        let session = self.contribution(path)?;
        session.file.matches(path).then_some(session)
    }

    pub fn sessions_if_topology_unchanged(&self) -> Option<Vec<(String, PathBuf)>> {
        self.directories
            .iter()
            .all(FileFingerprint::is_current)
            .then(|| {
                self.contributions
                    .iter()
                    .map(|contribution| {
                        (
                            contribution.source.clone(),
                            PathBuf::from(&contribution.path),
                        )
                    })
                    .collect()
            })
    }

    pub fn output_dependencies(&self) -> Vec<DependencyStamp> {
        self.directories
            .iter()
            .chain(&self.files)
            .chain(std::iter::once(&self.catalog))
            .map(|fingerprint| DependencyStamp {
                path: PathBuf::from(&fingerprint.path),
                length: fingerprint.len,
                modified_ns: fingerprint.mtime_ns,
            })
            .collect()
    }
}

pub fn load_output_entry(
    namespace: &str,
    key: &str,
    catalog_path: &Path,
) -> Option<(String, Vec<DependencyStamp>)> {
    let path = output_state_path(catalog_path, namespace, key)?;
    let bytes = fs::read(path).ok()?;
    let (cached, consumed): (CachedOutput, _) =
        bincode::decode_from_slice(&bytes, bincode::config::standard()).ok()?;
    (consumed == bytes.len()
        && cached.version == OUTPUT_CACHE_VERSION
        && cached.key == key
        && cached.dependencies.iter().all(DependencyStamp::is_current))
    .then_some((cached.output, cached.dependencies))
}

pub fn load_output(namespace: &str, key: &str, catalog_path: &Path) -> Option<String> {
    load_output_entry(namespace, key, catalog_path).map(|(output, _)| output)
}

pub fn save_output(
    namespace: &str,
    key: &str,
    catalog_path: &Path,
    dependencies: Vec<DependencyStamp>,
    output: &str,
) {
    let Some(path) = output_state_path(catalog_path, namespace, key) else {
        return;
    };
    let cached = CachedOutput {
        version: OUTPUT_CACHE_VERSION,
        key: key.to_owned(),
        dependencies,
        output: output.to_owned(),
    };
    if let Ok(bytes) = bincode::encode_to_vec(cached, bincode::config::standard()) {
        write_atomic_bytes(&path, &bytes);
    }
}

pub fn load_report_state(scope: &str, catalog_path: &Path) -> Option<(CachedReport, bool)> {
    let path = state_path(catalog_path, scope)?;
    let bytes = fs::read(path).ok()?;
    let report: CachedReport = serde_json::from_slice(&bytes).ok()?;
    if report.version != REPORT_CACHE_VERSION || !report.catalog.matches(catalog_path) {
        return None;
    }
    let current = report.files.iter().all(FileFingerprint::is_current)
        && report.directories.iter().all(FileFingerprint::is_current);
    Some((report, current))
}

pub fn load_report(scope: &str, catalog_path: &Path) -> Option<CachedReport> {
    load_report_state(scope, catalog_path).and_then(|(report, current)| current.then_some(report))
}

pub fn load_stale_report(scope: &str, catalog_path: &Path) -> Option<CachedReport> {
    load_report_state(scope, catalog_path).map(|(report, _)| report)
}

pub fn save_report(report: &CachedReport, scope: &str, catalog_path: &Path) {
    let Some(path) = state_path(catalog_path, scope) else {
        return;
    };
    let Ok(bytes) = serde_json::to_vec(report) else {
        return;
    };
    let Some(parent) = path.parent() else { return };
    if fs::create_dir_all(parent).is_err() {
        return;
    }
    let temporary = parent.join(format!(".state.json.{}.tmp", std::process::id()));
    if fs::write(&temporary, bytes).is_ok() {
        let _ = fs::rename(&temporary, path);
    } else {
        let _ = fs::remove_file(temporary);
    }
}

fn fingerprints(paths: &[PathBuf]) -> Result<Vec<FileFingerprint>, String> {
    paths.iter().map(|path| fingerprint(path)).collect()
}

fn state_path(catalog_path: &Path, scope: &str) -> Option<PathBuf> {
    let mut hasher = DefaultHasher::new();
    scope.hash(&mut hasher);
    Some(
        catalog_path
            .parent()?
            .join(format!("state-{:016x}.json", hasher.finish())),
    )
}

fn output_state_path(catalog_path: &Path, namespace: &str, key: &str) -> Option<PathBuf> {
    let mut hasher = DefaultHasher::new();
    namespace.hash(&mut hasher);
    key.hash(&mut hasher);
    Some(
        catalog_path
            .parent()?
            .join(format!("output-{:016x}.bin", hasher.finish())),
    )
}

fn session_state_path(
    catalog_path: &Path,
    session_path: &Path,
    range_key: &str,
) -> Option<PathBuf> {
    let mut hasher = DefaultHasher::new();
    session_path.hash(&mut hasher);
    range_key.hash(&mut hasher);
    Some(
        catalog_path
            .parent()?
            .join(format!("session-{:016x}.json", hasher.finish())),
    )
}
fn progress_state_path(
    catalog_path: &Path,
    session_path: &Path,
    range_key: &str,
) -> Option<PathBuf> {
    let mut hasher = DefaultHasher::new();
    session_path.hash(&mut hasher);
    range_key.hash(&mut hasher);
    Some(
        catalog_path
            .parent()?
            .join(format!("progress-{:016x}.json", hasher.finish())),
    )
}

fn write_atomic(path: &Path, value: &impl Serialize) {
    let Ok(bytes) = serde_json::to_vec(value) else {
        return;
    };
    write_atomic_bytes(path, &bytes);
}

fn write_atomic_bytes(path: &Path, bytes: &[u8]) {
    let Some(parent) = path.parent() else { return };
    if fs::create_dir_all(parent).is_err() {
        return;
    }
    let temporary = parent.join(format!(".cache.{}.tmp", std::process::id()));
    if fs::write(&temporary, bytes).is_ok() {
        let _ = fs::rename(&temporary, path);
    } else {
        let _ = fs::remove_file(temporary);
    }
}

fn fingerprint(path: &Path) -> Result<FileFingerprint, String> {
    let metadata = fs::metadata(path)
        .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;
    let modified = metadata
        .modified()
        .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("invalid modification time for {}: {error}", path.display()))?
        .as_nanos();
    Ok(FileFingerprint {
        path: path.display().to_string(),
        len: metadata.len(),
        mtime_ns: modified,
    })
}

pub fn models_path() -> PathBuf {
    if let Some(path) = env::var_os("USCT_MODELS_PATH") {
        return path.into();
    }
    if let Some(cache) = env::var_os("XDG_CACHE_HOME") {
        return PathBuf::from(cache).join("usct/models.json");
    }
    env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".cache/usct/models.json")
}

pub fn update(path: &Path) -> Result<(), String> {
    let response = std::process::Command::new("curl")
        .args([
            "--fail",
            "--location",
            "--silent",
            "--show-error",
            MODELS_URL,
        ])
        .output()
        .map_err(|error| format!("cannot execute curl: {error}"))?;
    if !response.status.success() {
        return Err(format!(
            "models.dev download failed: {}",
            String::from_utf8_lossy(&response.stderr).trim()
        ));
    }
    let bytes = response.stdout;
    let bytes = crate::catalog::ModelsDevCatalog::from_slice(&bytes)?.to_compact_vec()?;
    let parent = path
        .parent()
        .ok_or_else(|| format!("invalid cache path: {}", path.display()))?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    let temporary = parent.join(format!(".models.json.{}.tmp", std::process::id()));
    let result = (|| {
        let mut file = fs::File::create(&temporary)
            .map_err(|error| format!("cannot create {}: {error}", temporary.display()))?;
        file.write_all(&bytes)
            .map_err(|error| format!("cannot write {}: {error}", temporary.display()))?;
        file.sync_all()
            .map_err(|error| format!("cannot sync {}: {error}", temporary.display()))?;
        fs::rename(&temporary, path)
            .map_err(|error| format!("cannot replace {}: {error}", path.display()))
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}
