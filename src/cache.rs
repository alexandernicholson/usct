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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct FileFingerprint {
    path: String,
    len: u64,
    mtime_ns: u128,
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
    (session.version == 5
        && session.file == fingerprint(path).ok()?
        && session.catalog == fingerprint(catalog_path).ok()?)
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
            version: 9,
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
        (session.file == fingerprint(path).ok()?).then_some(session)
    }

    pub fn sessions_if_topology_unchanged(&self) -> Option<Vec<(String, PathBuf)>> {
        (self.directories == refresh(&self.directories).ok()?).then(|| {
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
}

pub fn load_report(scope: &str, catalog_path: &Path) -> Option<CachedReport> {
    let path = state_path(catalog_path, scope)?;
    let bytes = fs::read(path).ok()?;
    let report: CachedReport = serde_json::from_slice(&bytes).ok()?;
    (report.version == 9
        && report.files == refresh(&report.files).ok()?
        && report.directories == refresh(&report.directories).ok()?
        && report.catalog == fingerprint(catalog_path).ok()?)
    .then_some(report)
}

pub fn load_stale_report(scope: &str, catalog_path: &Path) -> Option<CachedReport> {
    let path = state_path(catalog_path, scope)?;
    let report: CachedReport = serde_json::from_slice(&fs::read(path).ok()?).ok()?;
    (report.version == 9 && report.catalog == fingerprint(catalog_path).ok()?).then_some(report)
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

fn refresh(stored: &[FileFingerprint]) -> Result<Vec<FileFingerprint>, String> {
    stored
        .iter()
        .map(|item| fingerprint(Path::new(&item.path)))
        .collect()
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
    let Some(parent) = path.parent() else { return };
    if fs::create_dir_all(parent).is_err() {
        return;
    }
    let temporary = parent.join(format!(".session.json.{}.tmp", std::process::id()));
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
