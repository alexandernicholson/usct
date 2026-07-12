use pico_args::Arguments;
use serde_json::json;
use std::{
    path::{Path, PathBuf},
    process::ExitCode,
    str::FromStr,
};
use usct::{
    app, cache,
    catalog::ModelsDevCatalog,
    discovery,
    session::{Harness, parse_session_incremental},
    time_range::{Period, TimeRange, custom_range},
};

fn main() -> ExitCode {
    match run() {
        Ok(output) => {
            println!("{output}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("usct: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<String, String> {
    let mut args = Arguments::from_env();
    let command = args.subcommand().map_err(|error| error.to_string())?;
    if command.as_deref() == Some("update") {
        ensure_no_args(args)?;
        let path = cache::models_path();
        cache::update(&path)?;
        return Ok(format!("updated {}", path.display()));
    }
    if command.as_deref() == Some("sources") {
        ensure_no_args(args)?;
        let rows = Harness::ALL
            .into_iter()
            .map(|harness| {
                let roots: Vec<_> = discovery::roots(harness)
                    .into_iter()
                    .map(|path| path.display().to_string())
                    .collect();
                json!({"source": harness.name(), "roots": roots})
            })
            .collect::<Vec<_>>();
        return serde_json::to_string(&rows).map_err(|error| error.to_string());
    }
    if let Some(command) = command {
        return Err(format!("unknown command '{command}'"));
    }

    let source: String = args
        .opt_value_from_str("--source")
        .map_err(|error| error.to_string())?
        .unwrap_or_else(|| "auto".to_owned());
    let session: Option<PathBuf> = args
        .opt_value_from_os_str("--session", |value| Ok::<_, String>(PathBuf::from(value)))
        .map_err(|error| error.to_string())?;
    let format: String = args
        .opt_value_from_str("--format")
        .map_err(|error| error.to_string())?
        .unwrap_or_else(|| "compact".to_owned());
    let period_value: String = args
        .opt_value_from_str("--period")
        .map_err(|error| error.to_string())?
        .unwrap_or_else(|| "all".to_owned());
    let from: Option<String> = args
        .opt_value_from_str("--from")
        .map_err(|error| error.to_string())?;
    let to: Option<String> = args
        .opt_value_from_str("--to")
        .map_err(|error| error.to_string())?;
    if args.contains(["-h", "--help"]) {
        return Ok(help().to_owned());
    }
    ensure_no_args(args)?;
    if format != "compact" && format != "json" {
        return Err(format!("unsupported format '{format}'"));
    }
    let period = Period::parse(&period_value)?;
    let range = if let Some(from) = from.as_deref() {
        if period != Period::All {
            return Err("--from cannot be combined with a non-all --period".to_owned());
        }
        Some(custom_range(from, to.as_deref())?)
    } else {
        if to.is_some() {
            return Err("--to requires --from".to_owned());
        }
        period.range()?
    };
    let constrained = if source == "auto" {
        None
    } else {
        Some(Harness::from_str(&source)?)
    };
    let range_key = range
        .as_ref()
        .map_or_else(|| period_value.clone(), TimeRange::cache_key);
    let base_scope = session.as_ref().map_or_else(
        || format!("source:{source}"),
        |path| format!("session:{}", path.display()),
    );
    let scope = format!("{base_scope}:range:{range_key}");
    let catalog_path = cache::models_path();
    if let Some(report) = cache::load_report(&scope, &catalog_path) {
        return render_report(&report, &format);
    }
    let sessions: Vec<(Harness, PathBuf)> = match session {
        Some(path) => vec![(constrained.unwrap_or_else(|| infer_harness(&path)), path)],
        None if period == Period::Session => {
            let found = discovery::latest(constrained)?;
            vec![(found.harness, found.path)]
        }
        None => discovery::all(constrained)?
            .into_iter()
            .map(|found| (found.harness, found.path))
            .collect(),
    };
    let session_paths: Vec<_> = sessions.iter().map(|(_, path)| path.clone()).collect();
    let directory_paths = watch_directories(constrained, &session_paths);
    let catalog = ModelsDevCatalog::from_path(&catalog_path)
        .map_err(|error| format!("{error}; run 'usct update'"))?;
    let calculated = calculate_sessions(
        &sessions,
        &catalog,
        &catalog_path,
        &range_key,
        range.as_ref(),
    )?;
    let report = cache::CachedReport::new(
        calculated.sources,
        calculated.session_count,
        calculated.usage,
        calculated.cost,
        range.clone(),
        cache::CacheContext {
            session_paths: &session_paths,
            directory_paths: &directory_paths,
            catalog_path: &catalog_path,
        },
    )?;
    cache::save_report(&report, &scope, &catalog_path);
    render_report(&report, &format)
}

fn calculate_sessions(
    sessions: &[(Harness, PathBuf)],
    catalog: &ModelsDevCatalog,
    catalog_path: &Path,
    range_key: &str,
    range: Option<&TimeRange>,
) -> Result<app::AggregateReport, String> {
    let mut sources = Vec::new();
    let mut usage = usct::domain::TokenUsage::default();
    let mut cost = 0.0;
    let mut session_count = 0;
    for (harness, path) in sessions {
        let session = if let Some(cached) = cache::load_session(path, range_key, catalog_path) {
            cached
        } else {
            let progress = cache::load_progress(path, range_key, catalog_path);
            let (record, progress) =
                match parse_session_incremental(*harness, path, range, progress) {
                    Ok(result) => result,
                    Err(error) if error == "session contains no token usage" => continue,
                    Err(error) => return Err(format!("{}: {error}", path.display())),
                };
            let report = app::price_record(*harness, path, catalog, record)
                .map_err(|error| format!("{}: {error}", path.display()))?;
            let cached = cache::CachedSession::new(
                harness.name().to_owned(),
                report.record.usage,
                report.cost,
                path,
                catalog_path,
            )?;
            if let Some(progress) = progress.as_ref() {
                cache::save_progress(progress, path, range_key, catalog_path);
            }
            cache::save_session(&cached, path, range_key, catalog_path);
            cached
        };
        if !sources.contains(&session.source) {
            sources.push(session.source.clone());
        }
        usage.add_assign(session.usage);
        cost += session.cost_usd;
        session_count += 1;
    }
    if session_count == 0 {
        return Err("sessions contain no token usage".to_owned());
    }
    sources.sort();
    Ok(app::AggregateReport {
        sources,
        session_count,
        usage,
        cost,
    })
}

fn render_report(report: &cache::CachedReport, format: &str) -> Result<String, String> {
    if format == "json" {
        serde_json::to_string(&json!({
            "cost_usd": report.cost_usd,
            "sources": report.sources,
            "session_count": report.session_count,
            "range": report.range.as_ref().map(|range| json!({
                "label": range.label,
                "from": range.start_rfc3339(),
                "to": range.end_rfc3339()
            })),
            "tokens": {
                "input": report.usage.input,
                "output": report.usage.output,
                "cache_read": report.usage.cache_read,
                "cache_write": report.usage.cache_write,
                "reasoning": report.usage.reasoning
            }
        }))
        .map_err(|error| error.to_string())
    } else {
        Ok(compact(report.cost_usd))
    }
}

fn infer_harness(path: &Path) -> Harness {
    let text = path.to_string_lossy().to_ascii_lowercase();
    if text.contains("codex") || text.contains("rollout-") {
        Harness::Codex
    } else if text.contains("opencode") || path.extension().is_some_and(|ext| ext == "db") {
        Harness::OpenCode
    } else if text.contains("/.omp/") {
        Harness::Omp
    } else if text.contains("/.pi/") {
        Harness::Pi
    } else if text.contains("gemini") || text.contains("/chats/") {
        Harness::Gemini
    } else if text.contains("amp") || text.contains("threads") {
        Harness::Amp
    } else {
        Harness::Claude
    }
}

fn watch_directories(source: Option<Harness>, sessions: &[PathBuf]) -> Vec<PathBuf> {
    let harnesses: Vec<_> = source.map_or_else(|| Harness::ALL.to_vec(), |item| vec![item]);
    let mut directories: Vec<_> = harnesses
        .into_iter()
        .flat_map(discovery::roots)
        .filter(|path| path.exists())
        .collect();
    directories.extend(
        sessions
            .iter()
            .filter_map(|path| path.parent().map(Path::to_path_buf)),
    );
    directories.sort();
    directories.dedup();
    directories
}

fn compact(cost: f64) -> String {
    if cost == 0.0 {
        "$0.00".to_owned()
    } else if cost < 0.01 {
        format!("${cost:.4}")
    } else if cost < 1_000.0 {
        format!("${cost:.2}")
    } else {
        format!("${cost:.0}")
    }
}

fn ensure_no_args(args: Arguments) -> Result<(), String> {
    let remaining = args.finish();
    if remaining.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "unexpected argument '{}'",
            remaining[0].to_string_lossy()
        ))
    }
}

fn help() -> &'static str {
    "usct — ultra-speedy coding-agent cost tracker\n\nUSAGE:\n  usct [--source SOURCE] [--period PERIOD] [--session PATH] [--format FORMAT]\n  usct [--source SOURCE] --from DATE_OR_TIMESTAMP [--to DATE_OR_TIMESTAMP] [--format FORMAT]\n  usct update\n  usct sources\n\nSOURCES:\n  auto, claude, codex, pi, omp, opencode, gemini, amp\n\nPERIODS:\n  all (default), session, hour, day, week, month, year\n\nFORMATS:\n  compact (default), json\n\nDATES:\n  YYYY-MM-DD, YYYY-MM-DDTHH:MM:SS (local), or RFC 3339\n  --from is inclusive; --to is exclusive"
}
