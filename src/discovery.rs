use crate::session::Harness;
use std::{
    env, fs,
    path::{Path, PathBuf},
    time::SystemTime,
};
use walkdir::WalkDir;
pub const NO_SESSIONS_ERROR: &str = "no supported coding-agent session found";

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
        .ok_or_else(|| NO_SESSIONS_ERROR.to_owned())
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
        Err(NO_SESSIONS_ERROR.to_owned())
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
        Harness::Droid => {
            split_env("DROID_SESSIONS_DIR").unwrap_or_else(|| vec![home.join(".factory/sessions")])
        }
        Harness::Codebuff => split_env("CODEBUFF_DATA_DIR").unwrap_or_else(|| {
            ["manicode", "manicode-dev", "manicode-staging"]
                .into_iter()
                .map(|name| home.join(".config").join(name))
                .collect()
        }),
        Harness::Hermes => split_env("HERMES_HOME").unwrap_or_else(|| vec![home.join(".hermes")]),
        Harness::Goose => split_env("GOOSE_PATH_ROOT").unwrap_or_else(|| {
            vec![
                home.join(".local/share/goose"),
                home.join("Library/Application Support/goose"),
                home.join(".local/share/Block/goose"),
            ]
        }),
        Harness::OpenClaw => split_env("OPENCLAW_DIR").unwrap_or_else(|| {
            [".openclaw", ".clawdbot", ".moltbot", ".moldbot"]
                .into_iter()
                .map(|name| home.join(name))
                .collect()
        }),
        Harness::Kilo => {
            split_env("KILO_DATA_DIR").unwrap_or_else(|| vec![home.join(".local/share/kilo")])
        }
        Harness::Kimi => split_env("KIMI_DATA_DIR")
            .unwrap_or_else(|| vec![home.join(".kimi"), home.join(".kimi-code")]),
        Harness::Qwen => split_env("QWEN_DATA_DIR").unwrap_or_else(|| vec![home.join(".qwen")]),
        Harness::Copilot => {
            let mut roots = vec![home.join(".copilot/otel")];
            roots.extend(split_env("COPILOT_OTEL_FILE_EXPORTER_PATH").unwrap_or_default());
            roots
        }
    }
}

pub fn nearest_existing(mut path: PathBuf) -> Option<PathBuf> {
    loop {
        if path.exists() {
            return Some(path);
        }
        path = path.parent()?.to_path_buf();
    }
}

fn split_env(name: &str) -> Option<Vec<PathBuf>> {
    env::var_os(name).map(|value| {
        let text = value.to_string_lossy();
        if text.contains(',') {
            text.split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
                .collect()
        } else {
            env::split_paths(&value).collect()
        }
    })
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
            extension == "jsonl" && !has_component(path, "subagents")
        }
        Harness::OpenCode => {
            path.file_name().is_some_and(|name| name == "opencode.db")
                || (extension == "json" && has_component_pair(path, "storage", "message"))
        }
        Harness::Gemini => {
            (extension == "json" || extension == "jsonl") && has_component(path, "chats")
        }
        Harness::Amp => extension == "json" || extension == "jsonl",
        Harness::Droid => path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(".settings.json")),
        Harness::Codebuff => path
            .file_name()
            .is_some_and(|name| name == "chat-messages.json"),
        Harness::Hermes => path.file_name().is_some_and(|name| name == "state.db"),
        Harness::Goose => path.file_name().is_some_and(|name| name == "sessions.db"),
        Harness::OpenClaw => extension == "jsonl",
        Harness::Kilo => path.file_name().is_some_and(|name| name == "kilo.db"),
        Harness::Kimi => path.file_name().is_some_and(|name| name == "wire.jsonl"),
        Harness::Qwen => {
            extension == "jsonl" && has_component(path, "projects") && has_component(path, "chats")
        }
        Harness::Copilot => extension == "jsonl",
    }
}

fn has_component(path: &Path, wanted: &str) -> bool {
    path.components()
        .any(|component| component.as_os_str() == wanted)
}

fn has_component_pair(path: &Path, first: &str, second: &str) -> bool {
    let mut previous_was_first = false;
    path.components().any(|component| {
        let name = component.as_os_str();
        let matched = previous_was_first && name == second;
        previous_was_first = name == first;
        matched
    })
}
