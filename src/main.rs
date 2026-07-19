use pico_args::Arguments;
use serde_json::{Value, json};
use std::{
    hash::{DefaultHasher, Hash, Hasher},
    io::{self, IsTerminal, Read},
    path::{Path, PathBuf},
    process::ExitCode,
    str::FromStr,
    time::{SystemTime, UNIX_EPOCH},
};
use usct::{
    app, cache,
    catalog::ModelsDevCatalog,
    config, discovery,
    domain::{PricedModelUsage, UsageRecord},
    report::{self, BlockOptions, CostMode, Grouping, ReportOptions, ReportTimeZone},
    session::{Harness, parse_session, parse_session_incremental},
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
    if args.contains(["-V", "--version"]) || args.contains("-v") {
        return Ok(env!("CARGO_PKG_VERSION").to_owned());
    }
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
        if matches!(command.as_str(), "daily" | "weekly" | "monthly" | "session") {
            return run_grouped(&command, args, None);
        }
        if command == "blocks" {
            return run_blocks(args, Some(Harness::Claude));
        }
        if command == "statusline" {
            return run_statusline(args, None);
        }
        if let Ok(source) = Harness::from_str(&command) {
            if args.contains(["-h", "--help"]) {
                return Ok(source_help(source));
            }
            let Some(nested) = args.subcommand().map_err(|error| error.to_string())? else {
                return Ok(source_help(source));
            };
            return match nested.as_str() {
                "daily" | "weekly" | "monthly" | "session" => {
                    run_grouped(&nested, args, Some(source))
                }
                "blocks" if source == Harness::Claude => run_blocks(args, Some(source)),
                "statusline" => run_statusline(args, Some(source)),
                _ => Err(format!("unsupported {} command '{nested}'", source.name())),
            };
        }
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
    let constrained = if source == "auto" || source == "all" {
        None
    } else {
        Some(Harness::from_str(&source)?)
    };
    let range_key = range
        .as_ref()
        .map_or_else(|| period_value.clone(), TimeRange::cache_key);
    let base_scope = session.as_ref().map_or_else(
        || source_cache_scope(&source, constrained),
        |path| format!("session:{}", path.display()),
    );
    let scope = format!("{base_scope}:range:{range_key}");
    let catalog_path = cache::models_path();
    let output_key = format!("{}:{scope}:format:{format}", env!("CARGO_PKG_VERSION"));
    if let Some(output) = cache::load_output("aggregate", &output_key, &catalog_path) {
        return Ok(output);
    }
    let report_state = cache::load_report_state(&scope, &catalog_path);
    if let Some((report, true)) = &report_state {
        let output = render_report(report, &format)?;
        cache::save_output(
            "aggregate",
            &output_key,
            &catalog_path,
            report.output_dependencies(),
            &output,
        );
        return Ok(output);
    }
    let stale_report = report_state.map(|(report, _)| report);
    let sessions: Vec<(Harness, PathBuf)> = if let Some(path) = session {
        vec![(constrained.unwrap_or_else(|| infer_harness(&path)), path)]
    } else if let Some(cached) = stale_report
        .as_ref()
        .and_then(cache::CachedReport::sessions_if_topology_unchanged)
    {
        cached
            .into_iter()
            .map(|(source, path)| Harness::from_str(&source).map(|harness| (harness, path)))
            .collect::<Result<_, _>>()?
    } else if period == Period::Session {
        let found = discovery::latest(constrained)?;
        vec![(found.harness, found.path)]
    } else {
        discovery::all(constrained)?
            .into_iter()
            .map(|found| (found.harness, found.path))
            .collect()
    };
    let session_paths: Vec<_> = sessions.iter().map(|(_, path)| path.clone()).collect();
    let directory_paths = watch_directories(constrained, &session_paths);
    let calculated = calculate_sessions(
        &sessions,
        &catalog_path,
        &range_key,
        range.as_ref(),
        stale_report.as_ref(),
    )?;
    let aggregate = calculated.aggregate;
    let report = cache::CachedReport::new(
        aggregate.sources,
        aggregate.session_count,
        aggregate.usage,
        aggregate.cost,
        range.clone(),
        &calculated.contributions,
        cache::CacheContext {
            directory_paths: &directory_paths,
            catalog_path: &catalog_path,
        },
    )?;
    cache::save_report(&report, &scope, &catalog_path);
    let output = render_report(&report, &format)?;
    cache::save_output(
        "aggregate",
        &output_key,
        &catalog_path,
        report.output_dependencies(),
        &output,
    );
    Ok(output)
}

fn run_grouped(
    command: &str,
    mut args: Arguments,
    fixed_source: Option<Harness>,
) -> Result<String, String> {
    if args.contains(["-h", "--help"]) {
        return Ok(grouped_help(command, fixed_source).to_owned());
    }
    let explicit_config: Option<PathBuf> = args
        .opt_value_from_os_str("--config", |value| Ok::<_, String>(PathBuf::from(value)))
        .map_err(|error| error.to_string())?;
    let config = config::load(explicit_config.as_deref())?;
    let source_value: Option<String> = args
        .opt_value_from_str("--source")
        .map_err(|error| error.to_string())?;
    let source = if let Some(source) = fixed_source {
        if source_value.is_some() {
            return Err("--source cannot be used with a source command".to_owned());
        }
        Some(source)
    } else {
        source_value
            .or(config.source.clone())
            .filter(|value| value != "auto" && value != "all")
            .map(|value| Harness::from_str(&value))
            .transpose()?
    };
    let since = args
        .opt_value_from_str(["-s", "--since"])
        .map_err(|error| error.to_string())?
        .or(config.since.clone());
    let until = args
        .opt_value_from_str(["-u", "--until"])
        .map_err(|error| error.to_string())?
        .or(config.until.clone());
    let timezone_value: Option<String> = args
        .opt_value_from_str(["-z", "--timezone"])
        .map_err(|error| error.to_string())?;
    let timezone = report_timezone(timezone_value.or(config.timezone.clone()))?;
    let sections_value: Option<String> = args
        .opt_value_from_str("--sections")
        .map_err(|error| error.to_string())?;
    let sections = if let Some(value) = sections_value {
        value
            .split(',')
            .map(|value| Grouping::parse(value.trim()))
            .collect::<Result<Vec<_>, _>>()?
    } else if let Some(values) = config.sections.as_ref() {
        values
            .iter()
            .map(|value| Grouping::parse(value))
            .collect::<Result<Vec<_>, _>>()?
    } else {
        vec![Grouping::parse(command)?]
    };
    if sections.is_empty() {
        return Err("--sections must name at least one section".to_owned());
    }
    let format = args
        .opt_value_from_str::<_, String>("--format")
        .map_err(|error| error.to_string())?
        .or(config.format.clone());
    if format
        .as_deref()
        .is_some_and(|format| !matches!(format, "json" | "table" | "compact"))
    {
        return Err(format!("unsupported format '{}'", format.unwrap()));
    }
    let json_requested = args.contains(["-j", "--json"]);
    let compact_requested = args.contains("--compact");
    let configured_json = match format.as_deref() {
        Some(format) => format == "json",
        None => config.json.unwrap_or(false),
    };
    let configured_compact = match format.as_deref() {
        Some(format) => format == "compact",
        None => config.compact.unwrap_or(false),
    };
    let json = json_requested || (!compact_requested && configured_json);
    let color = color_enabled(
        &mut args,
        json,
        config.color.unwrap_or(false),
        config.no_color.unwrap_or(false),
    );
    let compact = compact_requested || (!json_requested && configured_compact);
    let no_cost = args.contains("--no-cost") || config.no_cost.unwrap_or(false);
    let by_agent = args.contains("--by-agent") || config.by_agent.unwrap_or(false);
    let instances = command != "session"
        && (args.contains(["-i", "--instances"]) || config.instances.unwrap_or(false));
    let session_id = (if command == "session" {
        args.opt_value_from_str(["-i", "--id"])
            .map_err(|error| error.to_string())?
    } else {
        args.opt_value_from_str("--id")
            .map_err(|error| error.to_string())?
    })
    .or(config.session_id.clone());
    let project = args
        .opt_value_from_str(["-p", "--project"])
        .map_err(|error| error.to_string())?
        .or(config.project.clone());
    let instance = args
        .opt_value_from_str("--instance")
        .map_err(|error| error.to_string())?
        .or(config.instance.clone());
    let project_aliases_value = args
        .opt_value_from_str("--project-aliases")
        .map_err(|error| error.to_string())?;
    let project_aliases = if project_aliases_value.is_some() {
        parse_project_aliases(project_aliases_value)?
    } else {
        config.project_aliases.clone().unwrap_or_default()
    };
    let speed: String = args
        .opt_value_from_str("--speed")
        .map_err(|error| error.to_string())?
        .or(args
            .opt_value_from_str("--speed-tier")
            .map_err(|error| error.to_string())?)
        .or(config.speed.clone())
        .unwrap_or_else(|| "auto".to_owned());
    if !matches!(speed.as_str(), "auto" | "standard" | "fast") {
        return Err("--speed must be 'auto', 'standard', or 'fast'".to_owned());
    }
    let order: String = args
        .opt_value_from_str(["-o", "--order"])
        .map_err(|error| error.to_string())?
        .or(config.order.clone())
        .unwrap_or_else(|| "asc".to_owned());
    if !matches!(order.as_str(), "asc" | "desc") {
        return Err("--order must be 'asc' or 'desc'".to_owned());
    }
    let breakdown = args.contains(["-b", "--breakdown"]) || config.breakdown.unwrap_or(false);
    let mode: String = args
        .opt_value_from_str(["-m", "--mode"])
        .map_err(|error| error.to_string())?
        .or(config.mode.clone())
        .unwrap_or_else(|| "auto".to_owned());
    let cost_mode = CostMode::parse(&mode)?;
    let debug_samples = args
        .opt_value_from_str("--debug-samples")
        .map_err(|error| error.to_string())?
        .or(config.debug_samples)
        .unwrap_or(5);
    let debug = args.contains(["-d", "--debug"]) || config.debug.unwrap_or(false);
    let single_thread = args.contains("--single-thread") || config.single_thread.unwrap_or(false);
    consume_common_compatibility(&mut args)?;
    ensure_no_args(args)?;
    let custom_price_count = config.prices.len();
    let codex_fast_enabled =
        (source.is_none() || source == Some(Harness::Codex)) && codex_fast(&speed);
    let options = ReportOptions {
        source,
        since,
        until,
        timezone,
        no_cost,
        by_agent,
        session_id,
        project,
        instance,
        custom_prices: config.prices,
        instances,
        project_aliases,
        descending: order == "desc",
        breakdown,
        codex_fast: codex_fast_enabled,
        cost_mode,
        debug,
        debug_samples,
        single_thread,
    };
    let catalog_path = cache::models_path();
    let output_cache_key = if debug {
        None
    } else {
        let output_kind = if json {
            "json".to_owned()
        } else {
            format!("table:{no_cost}:{compact}:{breakdown}:{color}")
        };
        Some(report::output_cache_key(
            &sections,
            &options,
            &output_kind,
            &catalog_path,
        )?)
    };
    if let Some(key) = output_cache_key.as_deref()
        && let Some(output) = cache::load_output("grouped", key, &catalog_path)
    {
        return Ok(output);
    }
    let data_cache_key = if debug {
        None
    } else {
        Some(report::output_cache_key(
            &sections,
            &options,
            "data",
            &catalog_path,
        )?)
    };
    let cached_data = data_cache_key
        .as_deref()
        .and_then(|key| cache::load_output_entry("grouped-data", key, &catalog_path))
        .and_then(|(data, dependencies)| {
            let mut reports: Vec<report::SectionReport> = serde_json::from_str(&data).ok()?;
            if reports.len() != sections.len() {
                return None;
            }
            for (report, section) in reports.iter_mut().zip(&sections) {
                report.name = section.name().to_owned();
            }
            Some(report::GeneratedReports {
                reports,
                dependencies,
            })
        });
    let generated = if let Some(generated) = cached_data {
        generated
    } else {
        let generated = report::generate_with_dependencies(&sections, &options, &catalog_path)?;
        if let Some(key) = data_cache_key.as_deref()
            && let Ok(data) = serde_json::to_string(&generated.reports)
        {
            cache::save_output(
                "grouped-data",
                key,
                &catalog_path,
                generated.dependencies.clone(),
                &data,
            );
        }
        generated
    };
    let reports = generated.reports;
    if debug {
        eprintln!(
            "usct: debug: mode={mode}, sections={}, custom_prices={custom_price_count}, sample_limit={debug_samples}",
            reports.len()
        );
    }
    let output = if json {
        report::json_output(&reports)?
    } else {
        report::table_output(&reports, no_cost, compact, breakdown, color)
    };
    if let Some(key) = output_cache_key.as_deref() {
        cache::save_output(
            "grouped",
            key,
            &catalog_path,
            generated.dependencies,
            &output,
        );
    }
    Ok(output)
}

fn run_blocks(mut args: Arguments, source: Option<Harness>) -> Result<String, String> {
    if args.contains(["-h", "--help"]) {
        return Ok(blocks_help().to_owned());
    }
    let explicit_config: Option<PathBuf> = args
        .opt_value_from_os_str("--config", |value| Ok::<_, String>(PathBuf::from(value)))
        .map_err(|error| error.to_string())?;
    let config = config::load(explicit_config.as_deref())?;
    let since = args
        .opt_value_from_str(["-s", "--since"])
        .map_err(|error| error.to_string())?
        .or(config.since.clone());
    let until = args
        .opt_value_from_str(["-u", "--until"])
        .map_err(|error| error.to_string())?
        .or(config.until.clone());
    let timezone_value: Option<String> = args
        .opt_value_from_str(["-z", "--timezone"])
        .map_err(|error| error.to_string())?;
    let timezone = report_timezone(timezone_value.or(config.timezone.clone()))?;
    let session_length_hours = args
        .opt_value_from_str(["-n", "--session-length"])
        .map_err(|error| error.to_string())?
        .or(config.session_length_hours)
        .unwrap_or(5);
    let order: String = args
        .opt_value_from_str(["-o", "--order"])
        .map_err(|error| error.to_string())?
        .or(config.order.clone())
        .unwrap_or_else(|| "asc".to_owned());
    if order != "asc" && order != "desc" {
        return Err("--order must be 'asc' or 'desc'".to_owned());
    }
    let token_limit_value: Option<String> = args
        .opt_value_from_str(["-t", "--token-limit"])
        .map_err(|error| error.to_string())?
        .or(config.token_limit.clone());
    let token_limit = token_limit_value
        .map(|value| {
            if value == "max" {
                Ok(200_000)
            } else {
                value
                    .parse::<u64>()
                    .map_err(|_| format!("invalid token limit '{value}'"))
            }
        })
        .transpose()?;
    if token_limit == Some(0) {
        return Err("--token-limit must be greater than zero".to_owned());
    }
    let single_thread = args.contains("--single-thread") || config.single_thread.unwrap_or(false);
    let json_requested = args.contains(["-j", "--json"]);
    let compact_requested = args.contains("--compact");
    let configured_json = match config.format.as_deref() {
        Some(format) => format == "json",
        None => config.json.unwrap_or(false),
    };
    let configured_compact = match config.format.as_deref() {
        Some(format) => format == "compact",
        None => config.compact.unwrap_or(false),
    };
    let json = json_requested || (!compact_requested && configured_json);
    let color = color_enabled(
        &mut args,
        json,
        config.color.unwrap_or(false),
        config.no_color.unwrap_or(false),
    );
    let active_only = args.contains(["-a", "--active"]) || config.active.unwrap_or(false);
    let recent = args.contains(["-r", "--recent"]) || config.recent.unwrap_or(false);
    let breakdown = args.contains(["-b", "--breakdown"]) || config.breakdown.unwrap_or(false);
    let compact = compact_requested || (!json_requested && configured_compact);
    let mode: String = args
        .opt_value_from_str(["-m", "--mode"])
        .map_err(|error| error.to_string())?
        .or(config.mode.clone())
        .unwrap_or_else(|| "auto".to_owned());
    let cost_mode = CostMode::parse(&mode)?;
    let debug_samples = args
        .opt_value_from_str("--debug-samples")
        .map_err(|error| error.to_string())?
        .or(config.debug_samples)
        .unwrap_or(5);
    let debug = args.contains(["-d", "--debug"]) || config.debug.unwrap_or(false);
    let custom_price_count = config.prices.len();
    let options = ReportOptions {
        source,
        since,
        until,
        timezone,
        no_cost: args.contains("--no-cost") || config.no_cost.unwrap_or(false),
        by_agent: false,
        session_id: None,
        project: None,
        instance: None,
        custom_prices: config.prices,
        instances: false,
        project_aliases: Default::default(),
        single_thread,
        descending: false,
        breakdown: false,
        codex_fast: false,
        cost_mode,
        debug,
        debug_samples,
    };
    let block_options = BlockOptions {
        session_length_hours,
        active_only,
        recent,
        descending: order == "desc",
        breakdown,
        token_limit,
        json,
        compact,
        color,
    };
    consume_common_compatibility(&mut args)?;
    ensure_no_args(args)?;
    let output = report::blocks_output(&options, &block_options, &cache::models_path())?;
    if debug {
        eprintln!(
            "usct: debug: mode={mode}, custom_prices={custom_price_count}, sample_limit={debug_samples}"
        );
    }
    Ok(output)
}

fn run_statusline(mut args: Arguments, fixed_source: Option<Harness>) -> Result<String, String> {
    if args.contains(["-h", "--help"]) {
        return Ok(statusline_help().to_owned());
    }
    let explicit_config: Option<PathBuf> = args
        .opt_value_from_os_str("--config", |value| Ok::<_, String>(PathBuf::from(value)))
        .map_err(|error| error.to_string())?;
    let config = config::load(explicit_config.as_deref())?;
    let visual_burn_rate: String = args
        .opt_value_from_str(["-B", "--visual-burn-rate"])
        .map_err(|error| error.to_string())?
        .or(config.visual_burn_rate.clone())
        .unwrap_or_else(|| "off".to_owned());
    if !matches!(
        visual_burn_rate.as_str(),
        "off" | "emoji" | "text" | "emoji-text"
    ) {
        return Err("--visual-burn-rate must be off, emoji, text, or emoji-text".to_owned());
    }
    let cost_source: String = args
        .opt_value_from_str("--cost-source")
        .map_err(|error| error.to_string())?
        .or(config.cost_source.clone())
        .unwrap_or_else(|| "auto".to_owned());
    if !matches!(
        cost_source.as_str(),
        "auto" | "calculated" | "reported" | "both"
    ) {
        return Err("--cost-source must be auto, calculated, reported, or both".to_owned());
    }
    let refresh_interval: u64 = args
        .opt_value_from_str("--refresh-interval")
        .map_err(|error| error.to_string())?
        .or(config.refresh_interval)
        .unwrap_or(1);
    let context_low: u8 = args
        .opt_value_from_str("--context-low-threshold")
        .map_err(|error| error.to_string())?
        .or(config.context_low_threshold)
        .unwrap_or(50);
    let context_medium: u8 = args
        .opt_value_from_str("--context-medium-threshold")
        .map_err(|error| error.to_string())?
        .or(config.context_medium_threshold)
        .unwrap_or(80);
    if context_low > 100 || context_medium > 100 || context_low >= context_medium {
        return Err("context thresholds must satisfy 0 <= low < medium <= 100".to_owned());
    }
    let _: Option<String> = args
        .opt_value_from_str(["-z", "--timezone"])
        .map_err(|error| error.to_string())?;
    let cache_disabled = args.contains("--no-cache");
    let cache_requested = args.contains("--cache");
    let cache_enabled = !cache_disabled && (cache_requested || config.cache.unwrap_or(true));
    let debug = args.contains(["-d", "--debug"]) || config.debug.unwrap_or(false);
    for flag in ["-O", "--offline", "--no-offline"] {
        args.contains(flag);
    }
    ensure_no_args(args)?;
    let mut input = String::new();
    io::stdin()
        .read_to_string(&mut input)
        .map_err(|error| format!("cannot read hook input: {error}"))?;
    let hook: Value = if input.trim().is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&input).map_err(|error| format!("invalid hook JSON: {error}"))?
    };
    let default_source = fixed_source.unwrap_or(Harness::Claude);
    let path = hook
        .get("transcript_path")
        .or_else(|| hook.get("transcriptPath"))
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .or_else(|| {
            discovery::latest(Some(default_source))
                .ok()
                .map(|item| item.path)
        })
        .ok_or_else(|| {
            format!(
                "hook input has no transcript path and no {} session was found",
                default_source.name()
            )
        })?;
    let source = fixed_source.unwrap_or_else(|| infer_harness(&path));
    let mut cache_hasher = DefaultHasher::new();
    input.hash(&mut cache_hasher);
    visual_burn_rate.hash(&mut cache_hasher);
    cost_source.hash(&mut cache_hasher);
    context_low.hash(&mut cache_hasher);
    context_medium.hash(&mut cache_hasher);
    source.name().hash(&mut cache_hasher);
    let statusline_key = cache_hasher.finish();
    if cache_enabled
        && let Some(output) = load_statusline_cache(&path, statusline_key, refresh_interval)
    {
        if debug {
            eprintln!("usct: debug: statusline cache=hit path={}", path.display());
        }
        return Ok(output);
    }
    let record = parse_session(source, &path)?;
    let usage = record.usage();
    let reported_cost = hook
        .pointer("/cost/total_cost_usd")
        .or_else(|| hook.pointer("/cost/totalCostUsd"))
        .or_else(|| hook.get("total_cost_usd"))
        .and_then(Value::as_f64);
    let needs_calculated =
        matches!(cost_source.as_str(), "calculated" | "both") || reported_cost.is_none();
    let mut catalog = None;
    let mut cost = 0.0;
    if needs_calculated {
        for item in record.models {
            let id = app::pricing_id(source, &item.model);
            let price = if let Some(price) = config
                .prices
                .get(&id)
                .or_else(|| config.prices.get(&item.model))
                .copied()
            {
                price
            } else {
                if catalog.is_none() {
                    catalog = Some(
                        ModelsDevCatalog::from_path(&cache::models_path())
                            .map_err(|error| format!("{error}; run 'usct update'"))?,
                    );
                }
                catalog
                    .as_ref()
                    .and_then(|catalog| catalog.find(&id))
                    .unwrap_or(usct::domain::Price::ZERO)
            };
            cost += price.cost(item.usage);
        }
    }
    let cost_text = match cost_source.as_str() {
        "calculated" => report::format_cost(cost),
        "reported" => report::format_cost(reported_cost.unwrap_or(cost)),
        "both" => reported_cost.map_or_else(
            || report::format_cost(cost),
            |reported| {
                format!(
                    "{} / {} reported",
                    report::format_cost(cost),
                    report::format_cost(reported)
                )
            },
        ),
        _ => report::format_cost(reported_cost.unwrap_or(cost)),
    };
    let mut output = format!(
        "{} · {}",
        cost_text,
        report::format_tokens(usage.total_tokens())
    );
    if let Some(percent) = hook_context_percent(&hook) {
        let percent = percent.clamp(0.0, 100.0);
        output.push_str(&format!(" · {percent:.0}% context"));
        if visual_burn_rate != "off" {
            let (emoji, text) = if percent < f64::from(context_low) {
                ("\u{1f7e2}", "low")
            } else if percent < f64::from(context_medium) {
                ("\u{1f7e1}", "medium")
            } else {
                ("\u{1f534}", "high")
            };
            match visual_burn_rate.as_str() {
                "emoji" => output.push_str(&format!(" · {emoji}")),
                "text" => output.push_str(&format!(" · burn {text}")),
                "emoji-text" => output.push_str(&format!(" · {emoji} burn {text}")),
                _ => {}
            }
        }
    }
    if debug {
        eprintln!(
            "usct: debug: statusline cache=miss source={} cost_source={cost_source} tokens={} calculated_cost={cost:.12} reported_cost={} path={}",
            source.name(),
            usage.total_tokens(),
            reported_cost.map_or_else(|| "none".to_owned(), |value| format!("{value:.12}")),
            path.display()
        );
    }
    if cache_enabled {
        save_statusline_cache(&path, statusline_key, &output);
    }
    Ok(output)
}
fn hook_context_percent(hook: &Value) -> Option<f64> {
    hook.pointer("/context_window/used_percentage")
        .or_else(|| hook.pointer("/contextWindow/usedPercentage"))
        .and_then(Value::as_f64)
        .or_else(|| {
            let context = hook
                .get("context_window")
                .or_else(|| hook.get("contextWindow"))?;
            let size = context
                .get("context_window_size")
                .or_else(|| context.get("contextWindowSize"))
                .and_then(Value::as_f64)?;
            let used = context
                .get("total_input_tokens")
                .or_else(|| context.get("totalInputTokens"))
                .and_then(Value::as_f64)
                .or_else(|| {
                    let usage = context
                        .get("current_usage")
                        .or_else(|| context.get("currentUsage"))?;
                    Some(
                        [
                            "input_tokens",
                            "cache_creation_input_tokens",
                            "cache_read_input_tokens",
                        ]
                        .into_iter()
                        .filter_map(|key| usage.get(key).and_then(Value::as_f64))
                        .sum(),
                    )
                })?;
            (size > 0.0).then_some(used * 100.0 / size)
        })
}

fn statusline_cache_path() -> PathBuf {
    cache::models_path()
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("statusline.json")
}

fn transcript_stamp(path: &Path) -> Option<(u64, u64)> {
    let metadata = std::fs::metadata(path).ok()?;
    let modified = metadata
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_nanos()
        .try_into()
        .ok()?;
    Some((metadata.len(), modified))
}

fn now_ms() -> Option<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_millis()
        .try_into()
        .ok()
}

fn load_statusline_cache(path: &Path, key: u64, refresh_interval: u64) -> Option<String> {
    let value: Value =
        serde_json::from_slice(&std::fs::read(statusline_cache_path()).ok()?).ok()?;
    let (length, modified_ns) = transcript_stamp(path)?;
    let created_ms = value.get("createdMs")?.as_u64()?;
    let fresh_until = created_ms.saturating_add(refresh_interval.saturating_mul(1000));
    (value.get("path")?.as_str()? == path.to_string_lossy()
        && value.get("key")?.as_u64()? == key
        && value.get("length")?.as_u64()? == length
        && value.get("modifiedNs")?.as_u64()? == modified_ns
        && now_ms()? <= fresh_until)
        .then(|| value.get("output")?.as_str().map(str::to_owned))
        .flatten()
}

fn save_statusline_cache(path: &Path, key: u64, output: &str) {
    let Some((length, modified_ns)) = transcript_stamp(path) else {
        return;
    };
    let Some(created_ms) = now_ms() else {
        return;
    };
    let cache_path = statusline_cache_path();
    let Some(parent) = cache_path.parent() else {
        return;
    };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    let value = json!({
        "path": path,
        "key": key,
        "length": length,
        "modifiedNs": modified_ns,
        "createdMs": created_ms,
        "output": output,
    });
    let Ok(bytes) = serde_json::to_vec(&value) else {
        return;
    };
    let temporary = cache_path.with_extension(format!("{}.tmp", std::process::id()));
    if std::fs::write(&temporary, bytes).is_ok() {
        let _ = std::fs::rename(temporary, cache_path);
    }
}

fn color_enabled(
    args: &mut Arguments,
    json: bool,
    config_requested: bool,
    config_disabled: bool,
) -> bool {
    if json || args.contains("--no-color") {
        return false;
    }
    if args.contains("--color") {
        return true;
    }
    if std::env::var_os("NO_COLOR").is_some() {
        return false;
    }
    if std::env::var_os("FORCE_COLOR").is_some() {
        return true;
    }
    if config_disabled {
        return false;
    }
    config_requested || io::stdout().is_terminal()
}

fn consume_common_compatibility(args: &mut Arguments) -> Result<(), String> {
    for flag in [
        "--all",
        "--offline",
        "-O",
        "--no-offline",
        "--color",
        "--no-color",
    ] {
        args.contains(flag);
    }
    Ok(())
}

fn parse_project_aliases(
    value: Option<String>,
) -> Result<std::collections::HashMap<String, String>, String> {
    let mut aliases = std::collections::HashMap::new();
    let Some(value) = value else {
        return Ok(aliases);
    };
    for pair in value.split(',').filter(|value| !value.trim().is_empty()) {
        let (name, alias) = pair
            .split_once('=')
            .ok_or_else(|| format!("invalid project alias '{pair}', expected name=alias"))?;
        if name.trim().is_empty() || alias.trim().is_empty() {
            return Err(format!(
                "invalid project alias '{pair}', expected name=alias"
            ));
        }
        aliases.insert(name.trim().to_owned(), alias.trim().to_owned());
    }
    Ok(aliases)
}

fn codex_fast(speed: &str) -> bool {
    match speed {
        "fast" => true,
        "standard" => false,
        _ => {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default();
            let root = std::env::var_os("CODEX_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".codex"));
            std::fs::read_to_string(root.join("config.toml"))
                .ok()
                .is_some_and(|config| {
                    config.lines().any(|line| {
                        let line = line.split('#').next().unwrap_or_default().trim();
                        line.starts_with("service_tier")
                            && line
                                .split_once('=')
                                .is_some_and(|(_, value)| value.trim().trim_matches('"') == "fast")
                    })
                })
        }
    }
}

fn report_timezone(value: Option<String>) -> Result<ReportTimeZone, String> {
    match value {
        Some(value) => {
            let value = value.trim().trim_start_matches(':');
            if value.eq_ignore_ascii_case("UTC") {
                Ok(ReportTimeZone::Utc)
            } else {
                jiff::tz::TimeZone::get(value)
                    .map(ReportTimeZone::Named)
                    .map_err(|error| format!("invalid timezone: {error}"))
            }
        }
        None => Ok(ReportTimeZone::System),
    }
}

fn grouped_help(command: &str, source: Option<Harness>) -> String {
    let prefix = source.map_or_else(
        || "usct".to_owned(),
        |source| format!("usct {}", source.name()),
    );
    let source_option = if source.is_none() {
        "      --source SOURCE\n"
    } else {
        ""
    };
    let selection = if command == "session" {
        "  -i, --id SESSION\n"
    } else {
        "  -i, --instances\n      --id SESSION\n"
    };
    let source_options = match source {
        None => {
            "  -p, --project NAME\n      --instance NAME\n      --project-aliases MAP\n      --speed auto|standard|fast\n"
        }
        Some(Harness::Claude) => {
            "  -p, --project NAME\n      --instance NAME\n      --project-aliases MAP\n"
        }
        Some(Harness::Codex) => "      --speed auto|standard|fast\n",
        _ => "",
    };
    format!(
        "USAGE:\n  {prefix} {command} [OPTIONS]\n\nOPTIONS:\n\
  -j, --json                     JSON output\n\
      --format table|json|compact\n\
  -s, --since DATE               Inclusive start date\n\
  -u, --until DATE               Inclusive end date\n\
  -z, --timezone IANA            Date-grouping timezone\n\
      --sections LIST            daily,weekly,monthly,session\n\
      --by-agent                 Include per-agent JSON breakdowns\n\
      --compact                  Narrow table layout\n\
      --no-cost                  Omit pricing and cost fields\n\
      --color / --no-color       Control ANSI color\n\
  -o, --order asc|desc\n\
  -b, --breakdown                Show per-model table rows\n\
  -m, --mode auto|calculate|display\n\
      --single-thread            Disable parallel transcript loading\n\
  -d, --debug                    Enable pricing diagnostics\n\
      --debug-samples N          Limit diagnostic examples\n\
      --config PATH              JSON configuration file\n\
{source_option}{selection}{source_options}\
  -O, --offline                  Use local pricing data\n\
  -h, --help\n\
  -v, -V, --version"
    )
}

fn blocks_help() -> &'static str {
    "USAGE:\n  usct blocks [OPTIONS]\n\nOPTIONS:\n\
  -s, --since DATE               Inclusive start date\n\
  -u, --until DATE               Inclusive end date\n\
  -j, --json                     JSON output\n\
  -o, --order asc|desc\n\
  -b, --breakdown                Show per-model rows\n\
  -m, --mode auto|calculate|display\n\
      --single-thread            Disable parallel transcript loading\n\
  -d, --debug                    Enable pricing diagnostics\n\
      --debug-samples N          Limit diagnostic examples\n\
  -a, --active                   Show only the active block\n\
  -r, --recent                   Show recent blocks\n\
  -t, --token-limit N|max        Show projected limit utilization\n\
  -n, --session-length HOURS     Block length (default: 5)\n\
  -z, --timezone IANA\n\
      --compact\n\
      --no-cost\n\
      --color / --no-color\n\
      --config PATH\n\
  -O, --offline\n\
  -h, --help\n\
  -v, -V, --version"
}

fn statusline_help() -> &'static str {
    "USAGE:\n  usct statusline [OPTIONS]\n  usct <SOURCE> statusline [OPTIONS]\n\n\
Reads hook JSON from stdin and emits a compact cost, token, and context summary.\n\n\
OPTIONS:\n\
  -B, --visual-burn-rate MODE    off|emoji|text|emoji-text\n\
      --cost-source SOURCE       auto|calculated|reported|both\n\
      --refresh-interval SECONDS Cache freshness (default: 1)\n\
      --context-low-threshold N  Low/medium boundary (default: 50)\n\
      --context-medium-threshold N Medium/high boundary (default: 80)\n\
      --cache / --no-cache\n\
  -z, --timezone IANA\n\
      --config PATH\n\
  -O, --offline\n\
  -h, --help\n\
  -V, --version"
}

fn source_help(source: Harness) -> String {
    let extra = if source == Harness::Claude {
        "\n  blocks\n  statusline"
    } else {
        "\n  statusline"
    };
    format!(
        "USAGE:\n  usct {} <COMMAND> [OPTIONS]\n\nCOMMANDS:\n  daily\n  weekly\n  monthly\n  session{extra}\n\nRun 'usct {} <COMMAND> --help' for command options.",
        source.name(),
        source.name()
    )
}

struct CalculatedSessions {
    aggregate: app::AggregateReport,
    contributions: Vec<(PathBuf, cache::CachedSession)>,
}

fn calculate_sessions(
    sessions: &[(Harness, PathBuf)],
    catalog_path: &Path,
    range_key: &str,
    range: Option<&TimeRange>,
    stale_report: Option<&cache::CachedReport>,
) -> Result<CalculatedSessions, String> {
    let mut catalog = None;
    let mut sources = Vec::new();
    let mut usage = usct::domain::TokenUsage::default();
    let mut cost = 0.0;
    let mut contributions = Vec::with_capacity(sessions.len());
    for (harness, path) in sessions {
        let prior = stale_report.and_then(|report| report.contribution(path));
        let session = if let Some(cached) =
            stale_report.and_then(|report| report.reusable_contribution(path))
        {
            cached
        } else {
            let progress = prior
                .as_ref()
                .and_then(|prior| prior.progress.clone())
                .or_else(|| cache::load_progress(path, range_key, catalog_path));
            let (record, progress) =
                match parse_session_incremental(*harness, path, range, progress) {
                    Ok(result) => result,
                    Err(error) if error == "session contains no token usage" => continue,
                    Err(error) => return Err(format!("{}: {error}", path.display())),
                };
            let cached_prices = prior.as_ref().filter(|prior| {
                record
                    .models
                    .iter()
                    .all(|item| prior.models.iter().any(|cached| cached.model == item.model))
            });
            let (models, usage, calculated_cost) = if let Some(prior) = cached_prices {
                price_from_cache(record, prior)
            } else {
                if catalog.is_none() {
                    catalog = Some(
                        ModelsDevCatalog::from_path(catalog_path)
                            .map_err(|error| format!("{error}; run 'usct update'"))?,
                    );
                }
                let report = app::price_record(
                    *harness,
                    path,
                    catalog.as_ref().expect("catalog initialized"),
                    record,
                )
                .map_err(|error| format!("{}: {error}", path.display()))?;
                (report.models, report.usage, report.cost)
            };
            cache::CachedSession::new(
                cache::SessionData {
                    source: harness.name().to_owned(),
                    models,
                    usage,
                    cost_usd: calculated_cost,
                    progress,
                },
                path,
                catalog_path,
            )?
        };
        if !sources.contains(&session.source) {
            sources.push(session.source.clone());
        }
        usage.add_assign(session.usage);
        cost += session.cost_usd;
        contributions.push((path.clone(), session));
    }
    if contributions.is_empty() {
        return Err("sessions contain no token usage".to_owned());
    }
    sources.sort();
    Ok(CalculatedSessions {
        aggregate: app::AggregateReport {
            sources,
            session_count: contributions.len(),
            usage,
            cost,
        },
        contributions,
    })
}

fn price_from_cache(
    record: UsageRecord,
    prior: &cache::CachedSession,
) -> (Vec<PricedModelUsage>, usct::domain::TokenUsage, f64) {
    let usage = record.usage();
    let mut cost = 0.0;
    let mut models = Vec::with_capacity(record.models.len());
    for item in record.models {
        let price = prior
            .models
            .iter()
            .find(|cached| cached.model == item.model)
            .expect("cached prices checked")
            .price;
        let cost_usd = price.cost(item.usage);
        cost += cost_usd;
        models.push(PricedModelUsage {
            model: item.model,
            usage: item.usage,
            price,
            cost_usd,
        });
    }
    (models, usage, cost)
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

fn source_cache_scope(label: &str, source: Option<Harness>) -> String {
    let mut hasher = DefaultHasher::new();
    std::env::var_os("HOME").hash(&mut hasher);
    let mut hash_override = |harness| {
        let name = root_environment_variable(harness);
        if let Some(value) = std::env::var_os(name) {
            name.hash(&mut hasher);
            value.hash(&mut hasher);
        }
    };
    if let Some(source) = source {
        hash_override(source);
    } else {
        Harness::ALL.into_iter().for_each(hash_override);
    }
    format!("source:{label}:root-config:{:016x}", hasher.finish())
}

fn root_environment_variable(harness: Harness) -> &'static str {
    match harness {
        Harness::Claude => "CLAUDE_CONFIG_DIR",
        Harness::Codex => "CODEX_HOME",
        Harness::Pi => "PI_CODING_AGENT_SESSION_DIR",
        Harness::Omp => "OMP_AGENT_SESSION_DIR",
        Harness::OpenCode => "OPENCODE_DATA_DIR",
        Harness::Gemini => "GEMINI_DATA_DIR",
        Harness::Amp => "AMP_DATA_DIR",
        Harness::Droid => "DROID_SESSIONS_DIR",
        Harness::Codebuff => "CODEBUFF_DATA_DIR",
        Harness::Hermes => "HERMES_HOME",
        Harness::Goose => "GOOSE_PATH_ROOT",
        Harness::OpenClaw => "OPENCLAW_DIR",
        Harness::Kilo => "KILO_DATA_DIR",
        Harness::Kimi => "KIMI_DATA_DIR",
        Harness::Qwen => "QWEN_DATA_DIR",
        Harness::Copilot => "COPILOT_OTEL_FILE_EXPORTER_PATH",
    }
}

fn watch_directories(source: Option<Harness>, sessions: &[PathBuf]) -> Vec<PathBuf> {
    let harnesses: Vec<_> = source.map_or_else(|| Harness::ALL.to_vec(), |item| vec![item]);
    let mut directories: Vec<_> = harnesses
        .into_iter()
        .flat_map(discovery::roots)
        .filter_map(discovery::nearest_existing)
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
    "usct — ultra-speedy coding-agent usage and cost tracker\n\n\
USAGE:\n\
  usct                                      Print the aggregate dollar total\n\
  usct [AGGREGATE OPTIONS]                  Fast compact or JSON aggregate\n\
  usct <daily|weekly|monthly|session> [OPTIONS]\n\
  usct blocks [OPTIONS]\n\
  usct statusline [OPTIONS]\n\
  usct <SOURCE> <COMMAND> [OPTIONS]\n\
  usct update\n\
  usct sources\n\n\
REPORT COMMANDS:\n\
  daily      Usage grouped by date\n\
  weekly     Usage grouped by Monday-starting week\n\
  monthly    Usage grouped by month\n\
  session    Usage grouped by transcript\n\
  blocks     Usage grouped into billing blocks\n\
  statusline Compact hook status line read from stdin\n\n\
SOURCES:\n\
  auto, claude, codex, pi, omp, opencode, gemini, amp, droid,\n\
  codebuff, hermes, goose, openclaw, kilo, kimi, qwen, copilot\n\n\
AGGREGATE OPTIONS:\n\
  --source SOURCE\n\
  --period all|session|hour|day|week|month|year\n\
  --session PATH\n\
  --from DATE_OR_TIMESTAMP        Inclusive\n\
  --to DATE_OR_TIMESTAMP          Exclusive\n\
  --format compact|json\n\n\
DATES:\n\
  YYYY-MM-DD, YYYY-MM-DDTHH:MM:SS (local), or RFC 3339\n\n\
Run 'usct <COMMAND> --help' or 'usct <SOURCE> --help' for report options."
}
