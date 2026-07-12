use crate::session::Harness;
use std::{
    env, fs,
    path::{Path, PathBuf},
    time::SystemTime,
};
use walkdir::WalkDir;

#[derive(Debug, Clone)]
pub struct LocatedSession {
    pub harness: Harness,
    pub path: PathBuf,
    pub modified: SystemTime,
}

pub fn latest(source: Option<Harness>) -> Result<LocatedSession, String> {
    let harnesses: Vec<_> = source.map_or_else(|| Harness::ALL.to_vec(), |item| vec![item]);
    harnesses
        .into_iter()
        .filter_map(latest_for)
        .max_by_key(|item| item.modified)
        .ok_or_else(|| "no supported coding-agent session found".to_owned())
}

pub fn all(source: Option<Harness>) -> Result<Vec<LocatedSession>, String> {
    let harnesses: Vec<_> = source.map_or_else(|| Harness::ALL.to_vec(), |item| vec![item]);
    let mut sessions: Vec<_> = harnesses
        .into_iter()
        .flat_map(|harness| {
            roots(harness)
                .into_iter()
                .flat_map(move |root| candidates(harness, &root))
        })
        .collect();
    sessions.sort_by(|left, right| left.path.cmp(&right.path));
    if sessions.is_empty() {
        Err("no supported coding-agent session found".to_owned())
    } else {
        Ok(sessions)
    }
}

pub fn roots(harness: Harness) -> Vec<PathBuf> {
    let home = env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    match harness {
        Harness::Claude => split_env("CLAUDE_CONFIG_DIR")
            .unwrap_or_else(|| vec![home.join(".claude")])
            .into_iter()
            .map(|root| root.join("projects"))
            .collect(),
        Harness::Codex => split_env("CODEX_HOME")
            .unwrap_or_else(|| vec![home.join(".codex")])
            .into_iter()
            .flat_map(|root| [root.join("sessions"), root.join("archived_sessions")])
            .collect(),
        Harness::Pi => split_env("PI_CODING_AGENT_SESSION_DIR")
            .unwrap_or_else(|| vec![home.join(".pi/agent/sessions")]),
        Harness::Omp => split_env("OMP_AGENT_SESSION_DIR")
            .unwrap_or_else(|| vec![home.join(".omp/agent/sessions")]),
        Harness::OpenCode => split_env("OPENCODE_DATA_DIR")
            .unwrap_or_else(|| vec![home.join(".local/share/opencode")]),
        Harness::Gemini => {
            split_env("GEMINI_DATA_DIR").unwrap_or_else(|| vec![home.join(".gemini/tmp")])
        }
        Harness::Amp => {
            split_env("AMP_DATA_DIR").unwrap_or_else(|| vec![home.join(".local/share/amp/threads")])
        }
    }
}

fn split_env(name: &str) -> Option<Vec<PathBuf>> {
    env::var_os(name).map(|value| env::split_paths(&value).collect())
}

fn latest_for(harness: Harness) -> Option<LocatedSession> {
    roots(harness)
        .into_iter()
        .flat_map(|root| candidates(harness, &root))
        .max_by_key(|item| item.modified)
}

fn candidates(harness: Harness, root: &Path) -> Vec<LocatedSession> {
    if !root.exists() {
        return Vec::new();
    }
    WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file() && accepted(harness, entry.path()))
        .filter_map(|entry| {
            let modified = fs::metadata(entry.path()).ok()?.modified().ok()?;
            Some(LocatedSession {
                harness,
                path: entry.into_path(),
                modified,
            })
        })
        .collect()
}

fn accepted(harness: Harness, path: &Path) -> bool {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    match harness {
        Harness::Claude | Harness::Codex | Harness::Pi | Harness::Omp => {
            extension == "jsonl" && !path.to_string_lossy().contains("/subagents/")
        }
        Harness::OpenCode => {
            extension == "json" || path.file_name().is_some_and(|name| name == "opencode.db")
        }
        Harness::Gemini => {
            (extension == "json" || extension == "jsonl")
                && path.to_string_lossy().contains("/chats/")
        }
        Harness::Amp => extension == "json" || extension == "jsonl",
    }
}
