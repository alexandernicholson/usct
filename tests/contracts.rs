use std::{
    fs,
    io::Write,
    process::{Command, Stdio},
};
use tempfile::tempdir;
use unicode_width::UnicodeWidthStr;
use usct::{
    app, cache,
    catalog::ModelsDevCatalog,
    domain::{ModelUsage, Price, PricedModelUsage, TokenUsage, UsageRecord},
    session::{
        Harness, parse_session, parse_session_in_range, parse_session_incremental,
        parse_usage_events,
    },
    time_range::{Period, custom_range},
};

fn priced_model(model: &str, usage: TokenUsage, price: Price) -> PricedModelUsage {
    PricedModelUsage {
        model: model.to_owned(),
        usage,
        price,
        cost_usd: price.cost(usage),
    }
}

#[test]
fn prices_distinct_token_classes_without_double_counting_cache() {
    let usage = TokenUsage {
        input: 1_000_000,
        output: 100_000,
        cache_read: 250_000,
        cache_write: 50_000,
        reasoning: 25_000,
    };
    let price = Price {
        input: 2.0,
        output: 10.0,
        cache_read: Some(0.2),
        cache_write: Some(2.5),
        reasoning: Some(8.0),
    };
    assert!((price.cost(usage) - 3.375).abs() < 1e-12);
}

#[test]
fn reports_preserve_usage_when_model_pricing_is_unavailable() {
    let catalog = ModelsDevCatalog::from_slice(b"{}").unwrap();
    let usage = TokenUsage {
        input: 42,
        output: 7,
        ..TokenUsage::default()
    };
    let report = app::price_record(
        Harness::Codex,
        std::path::Path::new("unknown.jsonl"),
        &catalog,
        UsageRecord {
            models: vec![ModelUsage {
                model: "unlisted-model".to_owned(),
                usage,
            }],
        },
    )
    .unwrap();
    assert_eq!(report.usage, usage);
    assert_eq!(report.cost, 0.0);
    assert_eq!(report.models[0].price, Price::ZERO);

    let dir = tempdir().unwrap();
    let root = dir.path().join("claude");
    let session = root.join("projects/demo/session.jsonl");
    fs::create_dir_all(session.parent().unwrap()).unwrap();
    fs::write(
        &session,
        "{\"type\":\"assistant\",\"timestamp\":\"2026-07-18T10:00:00Z\",\"message\":{\"id\":\"unknown\",\"model\":\"unlisted-model\",\"usage\":{\"input_tokens\":42,\"output_tokens\":7}}}\n",
    )
    .unwrap();
    let models = dir.path().join("models.json");
    fs::write(&models, "{}").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_usct"))
        .args([
            "claude",
            "daily",
            "--json",
            "--timezone",
            "UTC",
            "--since",
            "2026-07-18",
            "--until",
            "2026-07-18",
        ])
        .env("CLAUDE_CONFIG_DIR", &root)
        .env("USCT_MODELS_PATH", &models)
        .env("USCT_CONFIG", dir.path().join("missing-config.json"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["daily"][0]["totalTokens"], 49);
    assert_eq!(report["daily"][0]["totalCost"], 0.0);
}

#[test]
fn models_dev_lookup_accepts_provider_qualified_and_bare_ids() {
    let json = r#"{"openai":{"id":"openai","models":{"gpt-5":{"id":"gpt-5","cost":{"input":1.25,"output":10,"cache_read":0.125}}}}}"#;
    let catalog = ModelsDevCatalog::from_slice(json.as_bytes()).unwrap();
    assert_eq!(catalog.find("openai/gpt-5").unwrap().input, 1.25);
    assert_eq!(catalog.find("gpt-5").unwrap().output, 10.0);
}

#[test]
fn models_dev_lookup_resolves_omp_model_aliases() {
    let json =
        r#"{"openai":{"models":{"gpt-5.6":{"id":"gpt-5.6","cost":{"input":5,"output":30}}}}}"#;
    let catalog = ModelsDevCatalog::from_slice(json.as_bytes()).unwrap();
    assert_eq!(catalog.find("openai/gpt-5.6-sol").unwrap().input, 5.0);
    assert_eq!(
        catalog.find("openai-codex/gpt-5.6-sol").unwrap().output,
        30.0
    );
    assert!(catalog.find("openai/gpt-5.6-codex").is_none());

    let exact = ModelsDevCatalog::from_slice(
        br#"{"openai":{"models":{"gpt-5.6":{"id":"gpt-5.6","cost":{"input":5,"output":30}},"gpt-5.6-sol":{"id":"gpt-5.6-sol","cost":{"input":7,"output":42}}}}}"#,
    )
    .unwrap();
    assert_eq!(exact.find("openai/gpt-5.6-sol").unwrap().input, 7.0);
}

#[test]
fn models_dev_lookup_resolves_dated_model_revisions() {
    let json = r#"{"openai":{"models":{"gpt-5.4-mini":{"id":"gpt-5.4-mini","cost":{"input":0.75,"output":4.5}}}}}"#;
    let catalog = ModelsDevCatalog::from_slice(json.as_bytes()).unwrap();
    assert_eq!(
        catalog
            .find("openai/gpt-5.4-mini-2026-03-17")
            .unwrap()
            .output,
        4.5
    );
    assert!(catalog.find("openai/gpt-5.4-mini-2026-02-30").is_none());

    let exact = ModelsDevCatalog::from_slice(
        br#"{"openai":{"models":{"gpt-5.4-mini":{"id":"gpt-5.4-mini","cost":{"input":0.75,"output":4.5}},"gpt-5.4-mini-2026-03-17":{"id":"gpt-5.4-mini-2026-03-17","cost":{"input":1,"output":6}}}}}"#,
    )
    .unwrap();
    assert_eq!(
        exact.find("openai/gpt-5.4-mini-2026-03-17").unwrap().output,
        6.0
    );
}

#[test]
fn claude_parser_sums_unique_assistant_usage() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    fs::write(&path, concat!(
        "{\"type\":\"assistant\",\"message\":{\"id\":\"a\",\"model\":\"claude-sonnet-4\",\"usage\":{\"input_tokens\":100,\"output_tokens\":20,\"cache_read_input_tokens\":50,\"cache_creation_input_tokens\":10}}}\n",
        "{\"type\":\"assistant\",\"message\":{\"id\":\"a\",\"model\":\"claude-sonnet-4\",\"usage\":{\"input_tokens\":100,\"output_tokens\":20,\"cache_read_input_tokens\":50,\"cache_creation_input_tokens\":10}}}\n",
        "{\"type\":\"assistant\",\"message\":{\"id\":\"b\",\"model\":\"claude-sonnet-4\",\"usage\":{\"input_tokens\":5,\"output_tokens\":3}}}\n"
    )).unwrap();
    let record = parse_session(Harness::Claude, &path).unwrap();
    assert_eq!(record.models[0].model, "claude-sonnet-4");
    assert_eq!(record.usage().input, 105);
    assert_eq!(record.usage().output, 23);
    assert_eq!(record.usage().cache_read, 50);
}

#[test]
fn claude_parser_ignores_synthetic_assistant_messages() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    fs::write(
        &path,
        concat!(
            "{\"type\":\"assistant\",\"message\":{\"id\":\"a\",\"model\":\"claude-test\",\"usage\":{\"input_tokens\":100,\"output_tokens\":10}}}\n",
            "{\"type\":\"assistant\",\"message\":{\"id\":\"b\",\"model\":\"<synthetic>\",\"usage\":{\"input_tokens\":999,\"output_tokens\":999}}}\n"
        ),
    )
    .unwrap();
    let record = parse_session(Harness::Claude, &path).unwrap();
    assert_eq!(record.models[0].model, "claude-test");
    assert_eq!(record.usage().input, 100);
    assert_eq!(record.usage().output, 10);

    fs::write(
        &path,
        "{\"type\":\"assistant\",\"message\":{\"id\":\"b\",\"model\":\"<synthetic>\",\"usage\":{\"input_tokens\":999,\"output_tokens\":999}}}\n",
    )
    .unwrap();
    assert_eq!(
        parse_session(Harness::Claude, &path).unwrap_err(),
        "session contains no token usage"
    );
}

#[test]
fn codex_parser_uses_final_cumulative_snapshot() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("rollout.jsonl");
    fs::write(&path, concat!(
        "{\"type\":\"turn_context\",\"payload\":{\"model\":\"gpt-5.6-codex\"}}\n",
        "{\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\"total_token_usage\":{\"input_tokens\":100,\"cached_input_tokens\":40,\"output_tokens\":20,\"reasoning_output_tokens\":5}}}}\n",
        "{\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\"total_token_usage\":{\"input_tokens\":170,\"cached_input_tokens\":60,\"output_tokens\":35,\"reasoning_output_tokens\":8}}}}\n"
    )).unwrap();
    let record = parse_session(Harness::Codex, &path).unwrap();
    assert_eq!(record.usage().input, 110);
    assert_eq!(record.usage().cache_read, 60);
    assert_eq!(record.usage().output, 27);
    assert_eq!(record.usage().reasoning, 8);
}

#[test]
fn codex_parser_attributes_deltas_across_model_changes_and_resets() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("rollout.jsonl");
    fs::write(
        &path,
        concat!(
            "{\"type\":\"turn_context\",\"payload\":{\"model\":\"gpt-a\"}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\"total_token_usage\":{\"input_tokens\":100,\"cached_input_tokens\":0,\"output_tokens\":0}}}}\n",
            "{\"type\":\"turn_context\",\"payload\":{\"model\":\"gpt-b\"}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\"total_token_usage\":{\"input_tokens\":180,\"cached_input_tokens\":0,\"output_tokens\":0}}}}\n",
            "{\"type\":\"turn_context\",\"payload\":{\"model\":\"gpt-a\"}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\"total_token_usage\":{\"input_tokens\":20,\"cached_input_tokens\":0,\"output_tokens\":0}}}}\n"
        ),
    )
    .unwrap();

    let record = parse_session(Harness::Codex, &path).unwrap();
    assert_eq!(record.models.len(), 2);
    assert_eq!(record.models[0].model, "gpt-a");
    assert_eq!(record.models[0].usage.input, 120);
    assert_eq!(record.models[1].model, "gpt-b");
    assert_eq!(record.models[1].usage.input, 80);

    let (incremental, _) = parse_session_incremental(Harness::Codex, &path, None, None).unwrap();
    assert_eq!(incremental, record);
}

#[test]
fn cli_prices_each_model_segment_at_its_own_rate() {
    let dir = tempdir().unwrap();
    let session = dir.path().join("session.jsonl");
    let models = dir.path().join("models.json");
    fs::write(
        &session,
        concat!(
            "{\"type\":\"assistant\",\"message\":{\"id\":\"a\",\"model\":\"claude-a\",\"usage\":{\"input_tokens\":1000000,\"output_tokens\":0}}}\n",
            "{\"type\":\"assistant\",\"message\":{\"id\":\"b\",\"model\":\"claude-b\",\"usage\":{\"input_tokens\":0,\"output_tokens\":1000000}}}\n"
        ),
    )
    .unwrap();
    fs::write(
        &models,
        r#"{"anthropic":{"models":{"claude-a":{"id":"claude-a","cost":{"input":1,"output":2}},"claude-b":{"id":"claude-b","cost":{"input":3,"output":10}}}}}"#,
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_usct"))
        .args(["--source", "claude", "--session", session.to_str().unwrap()])
        .env("USCT_MODELS_PATH", &models)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "$11.00\n");
}

#[test]
fn cli_explicit_session_is_statusline_clean() {
    let dir = tempdir().unwrap();
    let session = dir.path().join("session.jsonl");
    let models = dir.path().join("models.json");
    fs::write(&session, "{\"type\":\"assistant\",\"message\":{\"id\":\"a\",\"model\":\"claude-test\",\"usage\":{\"input_tokens\":1000000,\"output_tokens\":100000}}}\n").unwrap();
    fs::write(&models, r#"{"anthropic":{"models":{"claude-test":{"id":"claude-test","cost":{"input":2,"output":10}}}},"openai":{"models":{"claude-test":{"id":"claude-test","cost":{"input":100,"output":100}}}}}"#).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_usct"))
        .args(["--source", "claude", "--session", session.to_str().unwrap()])
        .env("USCT_MODELS_PATH", &models)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "$3.00\n");
    assert!(output.stderr.is_empty());
}

#[test]
fn generic_harness_parsers_accept_persisted_usage_objects() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("session.json");
    fs::write(
        &path,
        r#"{"messages":[{"id":"m1","model":"shared-test","usage":{"inputTokens":80,"cacheReadInputTokens":30,"outputTokens":20,"reasoning_tokens":5,"cacheWriteInputTokens":4}}]}"#,
    )
    .unwrap();
    for harness in [
        Harness::Pi,
        Harness::Omp,
        Harness::OpenCode,
        Harness::Gemini,
        Harness::Amp,
    ] {
        let record = parse_session(harness, &path).unwrap();
        assert_eq!(record.models[0].model, "shared-test");
        assert_eq!(
            record.usage().input,
            if harness == Harness::Gemini { 50 } else { 80 }
        );
        assert_eq!(record.usage().cache_read, 30);
        assert_eq!(record.usage().output, 15);
        assert_eq!(record.usage().reasoning, 5);
        assert_eq!(record.usage().cache_write, 4);
    }
}

#[test]
fn omp_model_change_events_select_the_usage_bucket() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    fs::write(
        &path,
        concat!(
            "{\"type\":\"model_change\",\"model\":\"openai/gpt-a\"}\n",
            "{\"type\":\"message\",\"id\":\"a\",\"message\":{\"role\":\"assistant\",\"usage\":{\"input\":100,\"output\":10}}}\n",
            "{\"type\":\"model_change\",\"model\":\"openai/gpt-b\"}\n",
            "{\"type\":\"message\",\"id\":\"b\",\"message\":{\"role\":\"assistant\",\"details\":{\"response\":{\"usage\":{\"input\":200,\"output\":20}}}}}\n"
        ),
    )
    .unwrap();

    let full = parse_session(Harness::Omp, &path).unwrap();
    let (incremental, _) = parse_session_incremental(Harness::Omp, &path, None, None).unwrap();
    assert_eq!(incremental, full);
    assert_eq!(full.models.len(), 2);
    assert_eq!(full.models[0].model, "openai/gpt-a");
    assert_eq!(full.models[0].usage.input, 100);
    assert_eq!(full.models[1].model, "openai/gpt-b");
    assert_eq!(full.models[1].usage.input, 200);

    let events = parse_usage_events(Harness::Omp, &path).unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].model, "openai/gpt-a");
    assert_eq!(events[0].usage.input, 100);
    assert_eq!(events[1].model, "openai/gpt-b");
    assert_eq!(events[1].usage.input, 200);
}

#[test]
fn pricing_id_uses_the_underlying_provider_for_routed_models() {
    assert_eq!(
        app::pricing_id(Harness::Omp, "cvm-pantheon/claude-opus-4-8"),
        "anthropic/claude-opus-4-8"
    );
}

#[test]
fn omp_parser_counts_equal_usage_from_distinct_messages() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    let line = r#"{"type":"message","id":"entry","message":{"role":"assistant","model":"gpt-test","usage":{"input":80,"output":20,"cacheRead":30}}}"#;
    fs::write(
        &path,
        format!("{line}\n{}\n", line.replace("\"entry\"", "\"other\"")),
    )
    .unwrap();
    let record = parse_session(Harness::Omp, &path).unwrap();
    assert_eq!(record.usage().input, 160);
    assert_eq!(record.usage().output, 40);
    assert_eq!(record.usage().cache_read, 60);
}

#[test]
fn aggregate_report_sums_sessions_across_providers() {
    let dir = tempdir().unwrap();
    let claude = dir.path().join("claude.jsonl");
    let codex = dir.path().join("codex.jsonl");
    fs::write(&claude, "{\"type\":\"assistant\",\"message\":{\"id\":\"a\",\"model\":\"claude-test\",\"usage\":{\"input_tokens\":1000000,\"output_tokens\":100000}}}\n").unwrap();
    fs::write(&codex, concat!(
        "{\"type\":\"turn_context\",\"payload\":{\"model\":\"gpt-test\"}}\n",
        "{\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\"total_token_usage\":{\"input_tokens\":2000000,\"cached_input_tokens\":500000,\"output_tokens\":200000}}}}\n"
    )).unwrap();
    let catalog = ModelsDevCatalog::from_slice(
        br#"{"anthropic":{"models":{"claude-test":{"id":"claude-test","cost":{"input":2,"output":10}}}},"openai":{"models":{"gpt-test":{"id":"gpt-test","cost":{"input":4,"output":20,"cache_read":1}}}}}"#,
    ).unwrap();
    let report = app::calculate_many(
        &[(Harness::Claude, claude), (Harness::Codex, codex)],
        &catalog,
    )
    .unwrap();
    assert_eq!(report.sources, ["claude", "codex"]);
    assert_eq!(report.session_count, 2);
    assert_eq!(report.usage.input, 2_500_000);
    assert_eq!(report.usage.cache_read, 500_000);
    assert_eq!(report.usage.output, 300_000);
    assert!((report.cost - 13.5).abs() < 1e-12);
}

#[test]
fn custom_range_uses_inclusive_start_and_exclusive_end() {
    let range = custom_range("2026-07-12T00:00:00Z", Some("2026-07-13T00:00:00Z")).unwrap();
    assert!(range.contains(range.start_ms));
    assert!(!range.contains(range.end_ms.unwrap()));
}

#[test]
fn built_in_periods_create_open_local_ranges() {
    for (period, label) in [
        (Period::Hour, "hour"),
        (Period::Day, "day"),
        (Period::Week, "week"),
        (Period::Month, "month"),
        (Period::Year, "year"),
    ] {
        let range = period.range().unwrap().unwrap();
        assert_eq!(range.label, label);
        assert!(range.end_ms.is_none());
    }
    assert!(Period::All.range().unwrap().is_none());
    assert!(Period::Session.range().unwrap().is_none());
}

#[test]
fn omp_parser_filters_usage_events_by_timestamp() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    fs::write(&path, concat!(
        "{\"type\":\"message\",\"timestamp\":\"2026-07-11T23:59:59Z\",\"message\":{\"role\":\"assistant\",\"model\":\"gpt-test\",\"usage\":{\"input\":80,\"output\":20}}}\n",
        "{\"type\":\"message\",\"timestamp\":\"2026-07-12T00:00:00Z\",\"message\":{\"role\":\"assistant\",\"model\":\"gpt-test\",\"usage\":{\"input\":100,\"output\":30}}}\n"
    )).unwrap();
    let range = custom_range("2026-07-12T00:00:00Z", Some("2026-07-13T00:00:00Z")).unwrap();
    let record = parse_session_in_range(Harness::Omp, &path, Some(&range)).unwrap();
    assert_eq!(record.usage().input, 100);
    assert_eq!(record.usage().output, 30);
}

#[test]
fn codex_range_subtracts_the_cumulative_baseline() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("rollout.jsonl");
    fs::write(&path, concat!(
        "{\"timestamp\":\"2026-07-11T23:59:59Z\",\"type\":\"turn_context\",\"payload\":{\"model\":\"gpt-test\"}}\n",
        "{\"timestamp\":\"2026-07-11T23:59:59Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\"total_token_usage\":{\"input_tokens\":100,\"cached_input_tokens\":20,\"output_tokens\":10}}}}\n",
        "{\"timestamp\":\"2026-07-12T00:01:00Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\"total_token_usage\":{\"input_tokens\":180,\"cached_input_tokens\":50,\"output_tokens\":25}}}}\n"
    )).unwrap();
    let range = custom_range("2026-07-12T00:00:00Z", Some("2026-07-13T00:00:00Z")).unwrap();
    let record = parse_session_in_range(Harness::Codex, &path, Some(&range)).unwrap();
    assert_eq!(record.usage().input, 50);
    assert_eq!(record.usage().cache_read, 30);
    assert_eq!(record.usage().output, 15);
}

#[test]
fn incremental_omp_parser_reads_only_appended_records() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    let first = "{\"type\":\"message\",\"timestamp\":\"2026-07-12T00:00:00Z\",\"message\":{\"role\":\"assistant\",\"model\":\"gpt-test\",\"usage\":{\"input\":100,\"output\":10}}}\n";
    let second = "{\"type\":\"message\",\"timestamp\":\"2026-07-12T00:01:00Z\",\"message\":{\"role\":\"assistant\",\"model\":\"gpt-test\",\"usage\":{\"input\":50,\"output\":5}}}\n";
    fs::write(&path, first).unwrap();
    let (record, state) = parse_session_incremental(Harness::Omp, &path, None, None).unwrap();
    assert_eq!(record.usage().input, 100);
    fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(second.as_bytes())
        .unwrap();
    let (record, _) = parse_session_incremental(Harness::Omp, &path, None, state).unwrap();
    assert_eq!(record.usage().input, 150);
    assert_eq!(record.usage().output, 15);
}

#[test]
fn incremental_claude_parser_deduplicates_borrowed_messages() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    fs::write(
        &path,
        concat!(
            "{\"type\":\"assistant\",\"message\":{\"id\":\"a\",\"model\":\"claude-test\",\"content\":\"ignored\",\"usage\":{\"input_tokens\":100,\"output_tokens\":10}}}\n",
            "{\"type\":\"assistant\",\"message\":{\"id\":\"a\",\"model\":\"claude-test\",\"content\":\"ignored again\",\"usage\":{\"input_tokens\":100,\"output_tokens\":10}}}\n",
            "{\"type\":\"assistant\",\"message\":{\"id\":\"b\",\"model\":\"claude-test\",\"usage\":{\"input_tokens\":50,\"output_tokens\":5}}}\n"
        ),
    )
    .unwrap();
    let (record, _) = parse_session_incremental(Harness::Claude, &path, None, None).unwrap();
    assert_eq!(record.usage().input, 150);
    assert_eq!(record.usage().output, 15);
}

#[test]
fn unknown_codex_records_retain_cumulative_schema_fallback() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("rollout.jsonl");
    fs::write(
        &path,
        concat!(
            "{\"timestamp\":\"2026-07-12T00:00:00Z\",\"type\":\"future_context\",\"payload\":{\"model\":\"gpt-test\"}}\n",
            "{\"timestamp\":\"2026-07-12T00:01:00Z\",\"type\":\"future_event\",\"payload\":{\"type\":\"token_count\",\"info\":{\"total_token_usage\":{\"input_tokens\":100,\"cached_input_tokens\":20,\"output_tokens\":10}}}}\n"
        ),
    )
    .unwrap();
    let (record, _) = parse_session_incremental(Harness::Codex, &path, None, None).unwrap();
    assert_eq!(record.usage().input, 80);
    assert_eq!(record.usage().cache_read, 20);
    assert_eq!(record.usage().output, 10);
}

#[test]
fn incremental_parser_rebuilds_after_in_place_replacement() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    let original = "{\"type\":\"message\",\"message\":{\"role\":\"assistant\",\"model\":\"gpt-test\",\"usage\":{\"input\":100,\"output\":10}}}\n";
    let replacement = "{\"type\":\"message\",\"message\":{\"role\":\"assistant\",\"model\":\"gpt-test\",\"usage\":{\"input\":900,\"output\":90}}}\n";
    fs::write(&path, original).unwrap();
    let (_, state) = parse_session_incremental(Harness::Omp, &path, None, None).unwrap();
    fs::write(&path, replacement).unwrap();
    let (record, _) = parse_session_incremental(Harness::Omp, &path, None, state).unwrap();
    assert_eq!(record.usage().input, 900);
    assert_eq!(record.usage().output, 90);
}

#[test]
fn incremental_codex_parser_updates_the_latest_cumulative_delta() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("rollout.jsonl");
    fs::write(&path, concat!(
        "{\"timestamp\":\"2026-07-11T23:59:59Z\",\"type\":\"turn_context\",\"payload\":{\"model\":\"gpt-test\"}}\n",
        "{\"timestamp\":\"2026-07-11T23:59:59Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\"total_token_usage\":{\"input_tokens\":100,\"cached_input_tokens\":20,\"output_tokens\":10}}}}\n",
        "{\"timestamp\":\"2026-07-12T00:01:00Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\"total_token_usage\":{\"input_tokens\":180,\"cached_input_tokens\":50,\"output_tokens\":25}}}}\n"
    )).unwrap();
    let range = custom_range("2026-07-12T00:00:00Z", None).unwrap();
    let (record, state) =
        parse_session_incremental(Harness::Codex, &path, Some(&range), None).unwrap();
    assert_eq!(record.usage().input, 50);
    fs::OpenOptions::new().append(true).open(&path).unwrap().write_all(
        b"{\"timestamp\":\"2026-07-12T00:02:00Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\"total_token_usage\":{\"input_tokens\":250,\"cached_input_tokens\":70,\"output_tokens\":30}}}}\n"
    ).unwrap();
    let (record, _) =
        parse_session_incremental(Harness::Codex, &path, Some(&range), state).unwrap();
    assert_eq!(record.usage().input, 100);
    assert_eq!(record.usage().cache_read, 50);
    assert_eq!(record.usage().output, 20);
}

#[test]
fn incremental_parser_defers_partial_trailing_jsonl_records() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    let complete = "{\"type\":\"message\",\"message\":{\"role\":\"assistant\",\"model\":\"gpt-test\",\"usage\":{\"input\":100,\"output\":10}}}\n";
    let partial = "{\"type\":\"message\",\"message\":{\"role\":\"assistant\",\"model\":\"gpt-test\",\"usage\":{\"input\":50";
    fs::write(&path, format!("{complete}{partial}")).unwrap();
    let (record, state) = parse_session_incremental(Harness::Omp, &path, None, None).unwrap();
    assert_eq!(record.usage().input, 100);
    fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b",\"output\":5}}}\n")
        .unwrap();
    let (record, _) = parse_session_incremental(Harness::Omp, &path, None, state).unwrap();
    assert_eq!(record.usage().input, 150);
    assert_eq!(record.usage().output, 15);
}

#[test]
fn typed_omp_parser_ignores_large_message_content() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    let content = format!(
        "{} {{\"usage\":{{\"input\":999999}}}}",
        "transcript payload ".repeat(50_000)
    );
    let line = serde_json::json!({
        "type": "message",
        "message": {
            "role": "assistant",
            "model": "gpt-test",
            "content": content,
            "usage": {"input": 100, "output": 10, "cacheRead": 20}
        }
    });
    fs::write(&path, format!("{line}\n")).unwrap();
    let (record, _) = parse_session_incremental(Harness::Omp, &path, None, None).unwrap();
    assert_eq!(record.usage().input, 100);
    assert_eq!(record.usage().output, 10);
    assert_eq!(record.usage().cache_read, 20);
}

#[test]
fn typed_omp_parser_preserves_nested_response_usage() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    fs::write(
        &path,
        concat!(
            "{\"type\":\"message\",\"message\":{\"role\":\"tool\",\"details\":{\"response\":",
            "{\"model\":\"gpt-test\",\"usage\":{\"inputTokens\":150,\"outputTokens\":9}}}}}\n"
        ),
    )
    .unwrap();
    let (record, _) = parse_session_incremental(Harness::Omp, &path, None, None).unwrap();
    assert_eq!(record.usage().input, 150);
    assert_eq!(record.usage().output, 9);
}

#[test]
fn unknown_omp_records_retain_recursive_schema_fallback() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    fs::write(
        &path,
        "{\"type\":\"future_record\",\"payload\":{\"model\":\"gpt-test\",\"usage\":{\"input\":70,\"output\":7}}}\n",
    )
    .unwrap();
    let (record, _) = parse_session_incremental(Harness::Omp, &path, None, None).unwrap();
    assert_eq!(record.usage().input, 70);
    assert_eq!(record.usage().output, 7);
}

#[test]
fn aggregate_cache_reuses_only_unchanged_contributions() {
    let dir = tempdir().unwrap();
    let catalog = dir.path().join("models.json");
    let first = dir.path().join("first.jsonl");
    let second = dir.path().join("second.jsonl");
    fs::write(&catalog, "{}").unwrap();
    fs::write(&first, "first\n").unwrap();
    fs::write(&second, "second\n").unwrap();
    let price = Price {
        input: 2.0,
        output: 10.0,
        cache_read: Some(0.2),
        cache_write: None,
        reasoning: None,
    };
    let usage = TokenUsage {
        input: 100,
        output: 10,
        ..TokenUsage::default()
    };
    let first_session = cache::CachedSession::new(
        cache::SessionData {
            source: "omp".to_owned(),
            models: vec![priced_model("gpt-test", usage, price)],
            usage,
            cost_usd: price.cost(usage),
            progress: None,
        },
        &first,
        &catalog,
    )
    .unwrap();
    let second_session = cache::CachedSession::new(
        cache::SessionData {
            source: "omp".to_owned(),
            models: vec![priced_model("gpt-test", usage, price)],
            usage,
            cost_usd: price.cost(usage),
            progress: None,
        },
        &second,
        &catalog,
    )
    .unwrap();
    let contributions = vec![
        (first.clone(), first_session),
        (second.clone(), second_session),
    ];
    let report = cache::CachedReport::new(
        vec!["omp".to_owned()],
        2,
        usage,
        price.cost(usage) * 2.0,
        None,
        &contributions,
        cache::CacheContext {
            directory_paths: &[dir.path().to_path_buf()],
            catalog_path: &catalog,
        },
    )
    .unwrap();
    cache::save_report(&report, "aggregate-test", &catalog);
    fs::write(&second, "changed\n").unwrap();
    assert!(cache::load_report("aggregate-test", &catalog).is_none());
    let stale = cache::load_stale_report("aggregate-test", &catalog).unwrap();
    assert!(stale.reusable_contribution(&first).is_some());
    assert!(stale.reusable_contribution(&second).is_none());
    let previous = stale.contribution(&second).unwrap();
    assert_eq!(previous.models[0].model, "gpt-test");
    assert_eq!(previous.models[0].price, price);
}

#[test]
fn catalog_change_invalidates_resolved_contribution_prices() {
    let dir = tempdir().unwrap();
    let catalog = dir.path().join("models.json");
    let session = dir.path().join("session.jsonl");
    fs::write(&catalog, "old").unwrap();
    fs::write(&session, "session\n").unwrap();
    let price = Price {
        input: 1.0,
        output: 2.0,
        cache_read: None,
        cache_write: None,
        reasoning: None,
    };
    let cached = cache::CachedSession::new(
        cache::SessionData {
            source: "omp".to_owned(),
            models: vec![priced_model("gpt-test", TokenUsage::default(), price)],
            usage: TokenUsage::default(),
            cost_usd: 0.0,
            progress: None,
        },
        &session,
        &catalog,
    )
    .unwrap();
    let report = cache::CachedReport::new(
        vec!["omp".to_owned()],
        1,
        TokenUsage::default(),
        0.0,
        None,
        &[(session, cached)],
        cache::CacheContext {
            directory_paths: &[dir.path().to_path_buf()],
            catalog_path: &catalog,
        },
    )
    .unwrap();
    cache::save_report(&report, "catalog-test", &catalog);
    fs::write(&catalog, "new catalog contents").unwrap();
    assert!(cache::load_stale_report("catalog-test", &catalog).is_none());
}

#[test]
fn added_json_sources_extract_their_native_usage_shapes() {
    let dir = tempdir().unwrap();
    let fixtures = [
        (
            Harness::Droid,
            "droid.json",
            r#"{"providerLockTimestamp":"2026-07-18T08:00:00Z","model":"custom:Claude Sonnet 4 [latest]","tokenUsage":{"inputTokens":100,"outputTokens":20,"cacheCreationTokens":10,"cacheReadTokens":30,"thinkingTokens":5,"totalTokens":165}}"#,
            165,
            "claude-sonnet-4",
        ),
        (
            Harness::OpenCode,
            "opencode.json",
            r#"{"id":"msg-1","sessionID":"session-1","modelID":"gpt-5","providerID":"openai","time":{"created":1784361660000},"tokens":{"input":50,"output":9,"cache":{"read":5,"write":3},"total":67}}"#,
            67,
            "gpt-5",
        ),
        (
            Harness::OpenClaw,
            "openclaw.jsonl",
            "{\"type\":\"model_change\",\"data\":{\"modelId\":\"gpt-5\"}}\n{\"type\":\"message\",\"id\":\"m1\",\"timestamp\":\"2026-07-18T08:01:00Z\",\"message\":{\"role\":\"assistant\",\"usage\":{\"input\":50,\"output\":9,\"cacheRead\":5,\"cacheWrite\":3,\"totalTokens\":67}}}\n",
            67,
            "gpt-5",
        ),
        (
            Harness::Kimi,
            "wire.jsonl",
            "{\"type\":\"usage.record\",\"usageScope\":\"turn\",\"time\":1784363400000,\"model\":\"kimi-code/kimi-k2\",\"usage\":{\"inputOther\":60,\"output\":11,\"inputCacheCreation\":4,\"inputCacheRead\":6}}\n",
            81,
            "kimi-k2",
        ),
        (
            Harness::Qwen,
            "qwen.jsonl",
            "{\"type\":\"assistant\",\"timestamp\":\"2026-07-18T08:30:00Z\",\"model\":\"qwen3-coder\",\"usageMetadata\":{\"promptTokenCount\":70,\"candidatesTokenCount\":12,\"thoughtsTokenCount\":3,\"cachedContentTokenCount\":7,\"totalTokenCount\":92}}\n",
            92,
            "qwen3-coder",
        ),
        (
            Harness::Copilot,
            "copilot.jsonl",
            "{\"traceId\":\"t1\",\"startTime\":[1784364000,0],\"attributes\":{\"gen_ai.response.id\":\"r1\",\"gen_ai.response.model\":\"gpt-5\",\"gen_ai.usage.input_tokens\":90,\"gen_ai.usage.output_tokens\":15,\"gen_ai.usage.cache_read.input_tokens\":10,\"gen_ai.usage.cache_write.input_tokens\":4,\"gen_ai.usage.reasoning.output_tokens\":2,\"gen_ai.usage.total_tokens\":111}}\n",
            111,
            "gpt-5",
        ),
    ];
    for (harness, name, contents, total, model) in fixtures {
        let path = dir.path().join(name);
        fs::write(&path, contents).unwrap();
        let events = parse_usage_events(harness, &path).unwrap();
        assert_eq!(
            events
                .iter()
                .map(|event| event.usage.total_tokens())
                .sum::<u64>(),
            total
        );
        assert_eq!(events[0].model, model);
    }
}

#[test]
fn structured_sqlite_sources_extract_session_totals() {
    let dir = tempdir().unwrap();

    let hermes = dir.path().join("state.db");
    let connection = rusqlite::Connection::open(&hermes).unwrap();
    connection.execute_batch(
        "CREATE TABLE sessions (model TEXT, started_at REAL, input_tokens INTEGER, output_tokens INTEGER, cache_read_tokens INTEGER, cache_write_tokens INTEGER, reasoning_tokens INTEGER);
         INSERT INTO sessions VALUES ('gpt-5', 1784364300, 80, 13, 8, 5, 2);",
    ).unwrap();
    drop(connection);
    assert_eq!(
        parse_usage_events(Harness::Hermes, &hermes).unwrap()[0]
            .usage
            .total_tokens(),
        108
    );

    let goose = dir.path().join("sessions.db");
    let connection = rusqlite::Connection::open(&goose).unwrap();
    connection.execute_batch(
        "CREATE TABLE sessions (model_config_json TEXT, created_at TEXT, total_tokens INTEGER, input_tokens INTEGER, output_tokens INTEGER, accumulated_total_tokens INTEGER, accumulated_input_tokens INTEGER, accumulated_output_tokens INTEGER);
         INSERT INTO sessions VALUES ('{\"model_name\":\"gpt-5\"}', '2026-07-18 08:50:00', 0, 0, 0, 120, 100, 16);",
    ).unwrap();
    drop(connection);
    assert_eq!(
        parse_usage_events(Harness::Goose, &goose).unwrap()[0]
            .usage
            .total_tokens(),
        120
    );

    let kilo = dir.path().join("kilo.db");
    let connection = rusqlite::Connection::open(&kilo).unwrap();
    connection
        .execute("CREATE TABLE message (data TEXT)", [])
        .unwrap();
    connection
        .execute(
            "INSERT INTO message VALUES (?1)",
            [r#"{"role":"assistant","modelID":"gpt-5","time":{"created":1784364900000},"tokens":{"input":110,"output":18,"reasoning":3,"cache":{"read":11,"write":6}}}"#],
        )
        .unwrap();
    drop(connection);
    assert_eq!(
        parse_usage_events(Harness::Kilo, &kilo).unwrap()[0]
            .usage
            .total_tokens(),
        148
    );

    let opencode = dir.path().join("opencode.db");
    let connection = rusqlite::Connection::open(&opencode).unwrap();
    connection
        .execute("CREATE TABLE message (data TEXT)", [])
        .unwrap();
    connection
        .execute(
            "INSERT INTO message VALUES (?1)",
            [r#"{"id":"msg-1","modelID":"gpt-5","time":{"created":1784361660000},"tokens":{"input":50,"output":9,"cache":{"read":5,"write":3},"total":67}}"#],
        )
        .unwrap();
    drop(connection);
    let events = parse_usage_events(Harness::OpenCode, &opencode).unwrap();
    assert_eq!(events[0].timestamp_ms, 1_784_361_660_000);
    assert_eq!(events[0].usage.total_tokens(), 67);
}

#[test]
fn grouped_cli_sections_are_inclusive_and_cache_appends_safely() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("claude");
    let session = root.join("projects/demo/session-one.jsonl");
    fs::create_dir_all(session.parent().unwrap()).unwrap();
    fs::write(
        &session,
        "{\"type\":\"assistant\",\"timestamp\":\"2026-07-18T10:00:00Z\",\"message\":{\"id\":\"m1\",\"model\":\"claude-test\",\"usage\":{\"input_tokens\":100,\"output_tokens\":10}}}\n",
    )
    .unwrap();
    let models = dir.path().join("cache/models.json");
    let command = || {
        Command::new(env!("CARGO_BIN_EXE_usct"))
            .args([
                "claude",
                "daily",
                "--json",
                "--no-cost",
                "--timezone",
                "UTC",
                "--since",
                "20260718",
                "--until",
                "2026-07-18",
                "--sections",
                "daily,weekly,monthly,session",
            ])
            .env("CLAUDE_CONFIG_DIR", &root)
            .env("USCT_MODELS_PATH", &models)
            .env("USCT_CONFIG", dir.path().join("missing-config.json"))
            .output()
            .unwrap()
    };
    let first = command();
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let first: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(first["daily"][0]["totalTokens"], 110);
    assert!(first["weekly"].is_array());
    assert!(first["monthly"].is_array());
    assert!(first["session"].is_array());

    fs::OpenOptions::new()
        .append(true)
        .open(&session)
        .unwrap()
        .write_all(b"{\"type\":\"assistant\",\"timestamp\":\"2026-07-18T11:00:00Z\",\"message\":{\"id\":\"m2\",\"model\":\"claude-test\",\"usage\":{\"input_tokens\":50,\"output_tokens\":5}}}\n")
        .unwrap();
    let second = command();
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    let second: serde_json::Value = serde_json::from_slice(&second.stdout).unwrap();
    assert_eq!(second["daily"][0]["totalTokens"], 165);
}

#[test]
fn statusline_accepts_reported_cost_and_visual_burn_options() {
    let dir = tempdir().unwrap();
    let session = dir.path().join("session.jsonl");
    let config = dir.path().join("config.json");
    fs::write(
        &session,
        "{\"type\":\"assistant\",\"timestamp\":\"2026-07-18T10:00:00Z\",\"message\":{\"id\":\"m1\",\"model\":\"claude-test\",\"usage\":{\"input_tokens\":100,\"output_tokens\":10}}}\n",
    )
    .unwrap();
    fs::write(
        &config,
        r#"{"prices":{"claude-test":{"input":3.0,"output":15.0}}}"#,
    )
    .unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_usct"))
        .args([
            "statusline",
            "--config",
            config.to_str().unwrap(),
            "--no-cache",
            "--cost-source",
            "both",
            "--visual-burn-rate",
            "text",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    write!(
        child.stdin.take().unwrap(),
        "{{\"transcript_path\":{},\"context_window\":{{\"used_percentage\":82}},\"cost\":{{\"total_cost_usd\":1.23}}}}",
        serde_json::to_string(session.to_str().unwrap()).unwrap()
    )
    .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "$0.0004 / $1.23 reported · 110 · 82% context · burn high\n"
    );
}

#[test]
fn omp_statusline_uses_the_omp_parser() {
    let dir = tempdir().unwrap();
    let session = dir.path().join("session.jsonl");
    let config = dir.path().join("config.json");
    fs::write(
        &session,
        concat!(
            "{\"type\":\"model_change\",\"model\":\"openai/gpt-test\"}\n",
            "{\"type\":\"message\",\"id\":\"m1\",\"message\":{\"role\":\"assistant\",\"details\":{\"response\":{\"usage\":{\"input\":100,\"output\":10}}}}}\n"
        ),
    )
    .unwrap();
    fs::write(
        &config,
        r#"{"prices":{"openai/gpt-test":{"input":1.0,"output":2.0}}}"#,
    )
    .unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_usct"))
        .args([
            "omp",
            "statusline",
            "--config",
            config.to_str().unwrap(),
            "--no-cache",
            "--cost-source",
            "calculated",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    write!(
        child.stdin.take().unwrap(),
        "{{\"transcript_path\":{},\"context_window\":{{\"used_percentage\":25}}}}",
        serde_json::to_string(session.to_str().unwrap()).unwrap()
    )
    .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "$0.0001 · 110 · 25% context\n"
    );
}

#[test]
fn grouped_custom_prices_work_without_a_catalog_and_apply_fast_tier() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("codex");
    let session = root.join("sessions/session.jsonl");
    let config = dir.path().join("config.json");
    fs::create_dir_all(session.parent().unwrap()).unwrap();
    fs::write(
        &session,
        concat!(
            "{\"timestamp\":\"2026-07-18T10:00:00Z\",\"type\":\"turn_context\",\"payload\":{\"model\":\"gpt-5\"}}\n",
            "{\"timestamp\":\"2026-07-18T10:01:00Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\"total_token_usage\":{\"input_tokens\":1000000,\"cached_input_tokens\":0,\"output_tokens\":100000}}}}\n"
        ),
    )
    .unwrap();
    fs::write(
        &config,
        r#"{"prices":{"gpt-5":{"input":2.0,"output":10.0}}}"#,
    )
    .unwrap();
    let missing_catalog = dir.path().join("cache/models.json");
    let output = Command::new(env!("CARGO_BIN_EXE_usct"))
        .args([
            "codex",
            "daily",
            "--json",
            "--timezone",
            "UTC",
            "--speed",
            "fast",
            "--config",
            config.to_str().unwrap(),
        ])
        .env("CODEX_HOME", &root)
        .env("USCT_MODELS_PATH", &missing_catalog)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["totals"]["totalCost"], 6.0);
}

#[test]
fn default_config_layers_grouped_report_options() {
    let dir = tempdir().unwrap();
    let home = dir.path().join("home");
    let session = home.join(".claude/projects/demo/session-one.jsonl");
    let config = home.join(".config/usct/config.json");
    fs::create_dir_all(session.parent().unwrap()).unwrap();
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    fs::write(
        &session,
        "{\"type\":\"assistant\",\"timestamp\":\"2026-07-18T10:00:00Z\",\"message\":{\"id\":\"m1\",\"model\":\"claude-test\",\"usage\":{\"input_tokens\":100,\"output_tokens\":10}}}\n",
    )
    .unwrap();
    fs::write(
        &config,
        r#"{"$schema":"https://example.invalid/usct.schema.json","source":"claude","timezone":"UTC","json":true,"noCost":true,"sections":["monthly","session"],"breakdown":true}"#,
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_usct"))
        .arg("daily")
        .env("HOME", &home)
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("USCT_CONFIG")
        .env_remove("CLAUDE_CONFIG_DIR")
        .env("USCT_MODELS_PATH", dir.path().join("missing/models.json"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(report.get("daily").is_none());
    assert_eq!(report["monthly"][0]["period"], "2026-07");
    assert_eq!(report["monthly"][0]["totalTokens"], 110);
    assert_eq!(report["session"][0]["period"], "session-one");
    let table = Command::new(env!("CARGO_BIN_EXE_usct"))
        .args(["daily", "--format", "table", "--no-color"])
        .env("HOME", &home)
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("USCT_CONFIG")
        .env_remove("CLAUDE_CONFIG_DIR")
        .env("USCT_MODELS_PATH", dir.path().join("missing/models.json"))
        .output()
        .unwrap();
    assert!(
        table.status.success(),
        "{}",
        String::from_utf8_lossy(&table.stderr)
    );
    let table = String::from_utf8(table.stdout).unwrap();
    assert!(table.starts_with("MONTHLY\n"));
}

#[test]
fn billing_blocks_emit_window_totals_and_limits() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("claude");
    let session = root.join("projects/demo/session-one.jsonl");
    fs::create_dir_all(session.parent().unwrap()).unwrap();
    fs::write(
        &session,
        "{\"type\":\"assistant\",\"timestamp\":\"2026-07-18T10:00:00Z\",\"message\":{\"id\":\"m1\",\"model\":\"claude-test\",\"usage\":{\"input_tokens\":100,\"output_tokens\":10}}}\n",
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_usct"))
        .args([
            "claude",
            "blocks",
            "--json",
            "--no-cost",
            "--timezone",
            "UTC",
            "--session-length",
            "5",
            "--token-limit",
            "1000",
        ])
        .env("CLAUDE_CONFIG_DIR", &root)
        .env("USCT_MODELS_PATH", dir.path().join("missing/models.json"))
        .env("USCT_CONFIG", dir.path().join("missing-config.json"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        report["blocks"][0]["startTime"],
        "2026-07-18T10:00:00+00:00"
    );
    assert_eq!(report["blocks"][0]["endTime"], "2026-07-18T15:00:00+00:00");
    assert_eq!(report["blocks"][0]["totalTokens"], 110);
    assert_eq!(report["blocks"][0]["tokenLimit"], 1000);
    assert_eq!(report["totals"]["totalTokens"], 110);
}

#[test]
fn reported_statusline_cost_does_not_require_a_catalog() {
    let dir = tempdir().unwrap();
    let session = dir.path().join("session.jsonl");
    let config = dir.path().join("config.json");
    fs::write(
        &session,
        "{\"type\":\"assistant\",\"timestamp\":\"2026-07-18T10:00:00Z\",\"message\":{\"id\":\"m1\",\"model\":\"claude-test\",\"usage\":{\"input_tokens\":100,\"output_tokens\":10}}}\n",
    )
    .unwrap();
    fs::write(&config, r#"{"costSource":"reported","cache":false}"#).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_usct"))
        .args(["statusline", "--config", config.to_str().unwrap()])
        .env("USCT_MODELS_PATH", dir.path().join("missing/models.json"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    write!(
        child.stdin.take().unwrap(),
        "{{\"transcript_path\":{},\"cost\":{{\"total_cost_usd\":1.23}}}}",
        serde_json::to_string(session.to_str().unwrap()).unwrap()
    )
    .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "$1.23 · 110\n");
}

#[test]
fn opencode_discovery_ignores_non_message_json() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("opencode");
    let message = root.join("storage/message/session-one/message.json");
    let metadata = root.join("storage/session/session-one.json");
    fs::create_dir_all(message.parent().unwrap()).unwrap();
    fs::create_dir_all(metadata.parent().unwrap()).unwrap();
    fs::write(
        &message,
        r#"{"id":"msg-1","sessionID":"session-one","modelID":"gpt-5","providerID":"openai","time":{"created":1784361660000},"tokens":{"input":50,"output":9,"cache":{"read":5,"write":3},"total":67}}"#,
    )
    .unwrap();
    fs::write(&metadata, r#"{"id":"session-one","title":"demo"}"#).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_usct"))
        .args([
            "opencode",
            "daily",
            "--json",
            "--no-cost",
            "--timezone",
            "UTC",
        ])
        .env("OPENCODE_DATA_DIR", &root)
        .env("USCT_MODELS_PATH", dir.path().join("missing/models.json"))
        .env("USCT_CONFIG", dir.path().join("missing-config.json"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["daily"][0]["totalTokens"], 67);
    assert_eq!(report["daily"][0]["metadata"]["sessionCount"], 1);
}

#[test]
fn empty_grouped_source_returns_zero_totals() {
    let dir = tempdir().unwrap();
    let home = dir.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_usct"))
        .args(["claude", "daily", "--json", "--timezone", "UTC"])
        .env("HOME", &home)
        .env_remove("CLAUDE_CONFIG_DIR")
        .env("USCT_MODELS_PATH", dir.path().join("missing/models.json"))
        .env("USCT_CONFIG", dir.path().join("missing-config.json"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["daily"], serde_json::json!([]));
    assert_eq!(report["totals"]["totalTokens"], 0);
    assert_eq!(report["totals"]["totalCost"], 0.0);
}

#[test]
fn grouped_cost_modes_use_per_message_reported_costs() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("claude");
    let session = root.join("projects/demo/session-one.jsonl");
    let config = dir.path().join("config.json");
    fs::create_dir_all(session.parent().unwrap()).unwrap();
    fs::write(
        &session,
        "{\"type\":\"assistant\",\"timestamp\":\"2026-07-18T10:00:00Z\",\"costUSD\":1.23,\"message\":{\"id\":\"m1\",\"model\":\"claude-test\",\"usage\":{\"input_tokens\":100,\"output_tokens\":10}}}\n",
    )
    .unwrap();
    fs::write(
        &config,
        r#"{"prices":{"claude-test":{"input":2.0,"output":10.0}}}"#,
    )
    .unwrap();
    for (mode, expected, source) in [
        ("auto", 1.23, "reported"),
        ("display", 1.23, "reported"),
        ("calculate", 0.0003, "config"),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_usct"))
            .args([
                "claude",
                "daily",
                "--json",
                "--timezone",
                "UTC",
                "--mode",
                mode,
                "--debug",
                "--debug-samples",
                "1",
            ])
            .env("CLAUDE_CONFIG_DIR", &root)
            .env("USCT_MODELS_PATH", dir.path().join("missing/models.json"))
            .env("USCT_CONFIG", &config)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{mode}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let actual = report["totals"]["totalCost"].as_f64().unwrap();
        assert!((actual - expected).abs() < 1e-12, "{mode}: {actual}");
        let diagnostics = String::from_utf8_lossy(&output.stderr);
        assert!(
            diagnostics.contains(&format!("cost_source={source}")),
            "{mode}"
        );
    }
}

#[test]
fn report_caches_include_resolved_source_roots() {
    let dir = tempdir().unwrap();
    let first_root = dir.path().join("first-claude");
    let second_root = dir.path().join("second-claude");
    let first_session = first_root.join("projects/demo/first.jsonl");
    let second_session = second_root.join("projects/demo/second.jsonl");
    fs::create_dir_all(first_session.parent().unwrap()).unwrap();
    fs::create_dir_all(second_session.parent().unwrap()).unwrap();
    fs::write(
        &first_session,
        "{\"type\":\"assistant\",\"timestamp\":\"2026-07-18T10:00:00Z\",\"message\":{\"id\":\"first\",\"model\":\"claude-test\",\"usage\":{\"input_tokens\":100,\"output_tokens\":10}}}\n",
    )
    .unwrap();
    fs::write(
        &second_session,
        "{\"type\":\"assistant\",\"timestamp\":\"2026-07-18T10:00:00Z\",\"message\":{\"id\":\"second\",\"model\":\"claude-test\",\"usage\":{\"input_tokens\":200,\"output_tokens\":20}}}\n",
    )
    .unwrap();
    let models = dir.path().join("cache/models.json");
    fs::create_dir_all(models.parent().unwrap()).unwrap();
    fs::write(
        &models,
        r#"{"anthropic":{"models":{"claude-test":{"id":"claude-test","cost":{"input":2.0,"output":10.0}}}}}"#,
    )
    .unwrap();
    let grouped = |root: &std::path::Path| {
        Command::new(env!("CARGO_BIN_EXE_usct"))
            .args([
                "claude",
                "daily",
                "--json",
                "--no-cost",
                "--timezone",
                "UTC",
            ])
            .env("CLAUDE_CONFIG_DIR", root)
            .env("USCT_MODELS_PATH", &models)
            .env("USCT_CONFIG", dir.path().join("missing-config.json"))
            .output()
            .unwrap()
    };
    let first = grouped(&first_root);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let first: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(first["totals"]["totalTokens"], 110);
    let second = grouped(&second_root);
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    let second: serde_json::Value = serde_json::from_slice(&second.stdout).unwrap();
    assert_eq!(second["totals"]["totalTokens"], 220);

    let aggregate = |root: &std::path::Path| {
        Command::new(env!("CARGO_BIN_EXE_usct"))
            .args(["--source", "claude", "--format", "json"])
            .env("CLAUDE_CONFIG_DIR", root)
            .env("USCT_MODELS_PATH", &models)
            .output()
            .unwrap()
    };
    let first = aggregate(&first_root);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let first: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(first["tokens"]["input"], 100);
    let second = aggregate(&second_root);
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    let second: serde_json::Value = serde_json::from_slice(&second.stdout).unwrap();
    assert_eq!(second["tokens"]["input"], 200);
}

#[test]
fn named_timezone_grouping_and_blocks_follow_dst_transitions() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("claude");
    let session = root.join("projects/demo/session.jsonl");
    fs::create_dir_all(session.parent().unwrap()).unwrap();
    fs::write(
        &session,
        concat!(
            "{\"type\":\"assistant\",\"timestamp\":\"2026-03-08T04:30:00Z\",\"message\":{\"id\":\"before\",\"model\":\"claude-test\",\"usage\":{\"input_tokens\":10,\"output_tokens\":1}}}\n",
            "{\"type\":\"assistant\",\"timestamp\":\"2026-03-08T07:30:00Z\",\"message\":{\"id\":\"after\",\"model\":\"claude-test\",\"usage\":{\"input_tokens\":20,\"output_tokens\":2}}}\n"
        ),
    )
    .unwrap();
    let models = dir.path().join("missing/models.json");
    let command = |extra: &[&str]| {
        let mut args = vec![
            "claude",
            "daily",
            "--json",
            "--no-cost",
            "--timezone",
            "America/New_York",
        ];
        args.extend_from_slice(extra);
        Command::new(env!("CARGO_BIN_EXE_usct"))
            .args(args)
            .env("CLAUDE_CONFIG_DIR", &root)
            .env("USCT_MODELS_PATH", &models)
            .env("USCT_CONFIG", dir.path().join("missing-config.json"))
            .output()
            .unwrap()
    };
    let output = command(&[]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["daily"][0]["period"], "2026-03-07");
    assert_eq!(report["daily"][1]["period"], "2026-03-08");
    assert_eq!(report["totals"]["totalTokens"], 33);

    let output = command(&["--since", "20260308", "--until", "20260308"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["daily"].as_array().unwrap().len(), 1);
    assert_eq!(report["daily"][0]["period"], "2026-03-08");
    assert_eq!(report["totals"]["totalTokens"], 22);

    let output = Command::new(env!("CARGO_BIN_EXE_usct"))
        .args([
            "blocks",
            "--json",
            "--no-cost",
            "--timezone",
            "America/New_York",
            "--session-length",
            "5",
        ])
        .env("CLAUDE_CONFIG_DIR", &root)
        .env("USCT_MODELS_PATH", &models)
        .env("USCT_CONFIG", dir.path().join("missing-config.json"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        report["blocks"][0]["startTime"],
        "2026-03-07T23:30:00-05:00"
    );
    assert_eq!(report["blocks"][0]["endTime"], "2026-03-08T05:30:00-04:00");
}

#[test]
fn provider_session_titles_preserve_stable_ids_and_table_alignment() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("omp");
    fs::create_dir_all(&root).unwrap();
    let write_session = |name: &str, id: &str, title: &str, input: u64, output: u64| {
        fs::write(
            root.join(name),
            format!(
                "{{\"type\":\"title\",\"title\":{title:?}}}\n\
                 {{\"type\":\"session\",\"id\":\"{id}\",\"timestamp\":\"2026-07-18T10:00:00Z\"}}\n\
                 {{\"type\":\"model_change\",\"model\":\"openai/gpt-test\"}}\n\
                 {{\"type\":\"message\",\"id\":\"message-{id}\",\"timestamp\":\"2026-07-18T10:00:00Z\",\"message\":{{\"role\":\"assistant\",\"usage\":{{\"input\":{input},\"output\":{output}}}}}}}\n"
            ),
        )
        .unwrap();
    };
    write_session(
        "one.jsonl",
        "stable-one",
        "A provider title that is substantially longer",
        100,
        10,
    );
    write_session("two.jsonl", "stable-two", "短い題名", 12_345, 1_234);
    let models = dir.path().join("cache/models.json");
    let config = dir.path().join("missing-config.json");

    let output = Command::new(env!("CARGO_BIN_EXE_usct"))
        .args([
            "omp",
            "session",
            "--no-cost",
            "--timezone",
            "UTC",
            "--no-color",
        ])
        .env("OMP_AGENT_SESSION_DIR", &root)
        .env("USCT_MODELS_PATH", &models)
        .env("USCT_CONFIG", &config)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let table = String::from_utf8(output.stdout).unwrap();
    let mut lines = table.lines();
    let header = lines.next().unwrap();
    let long = lines
        .clone()
        .find(|line| line.contains("A provider title"))
        .unwrap();
    let wide = lines.find(|line| line.contains("短い題名")).unwrap();
    let start =
        |line: &str, value: &str| UnicodeWidthStr::width(&line[..line.find(value).unwrap()]);
    let edge = |line: &str, value: &str| {
        UnicodeWidthStr::width(&line[..line.rfind(value).unwrap()]) + UnicodeWidthStr::width(value)
    };
    assert_eq!(start(header, "Agent"), start(long, "omp"));
    assert_eq!(start(header, "Agent"), start(wide, "omp"));
    assert_eq!(edge(header, "Total"), edge(long, "110"));
    assert_eq!(edge(header, "Total"), edge(wide, "13.6K"));
    assert!(!table.lines().any(|line| line.ends_with(' ')));

    let output = Command::new(env!("CARGO_BIN_EXE_usct"))
        .args(["omp", "session", "--json", "--no-cost", "--timezone", "UTC"])
        .env("OMP_AGENT_SESSION_DIR", &root)
        .env("USCT_MODELS_PATH", &models)
        .env("USCT_CONFIG", &config)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let rows = report["session"].as_array().unwrap();
    assert_eq!(rows[0]["period"], "stable-one");
    assert_eq!(
        rows[0]["metadata"]["sessionTitle"],
        "A provider title that is substantially longer"
    );
    assert_eq!(rows[1]["period"], "stable-two");
    assert_eq!(rows[1]["metadata"]["sessionTitle"], "短い題名");
}

#[test]
fn external_provider_session_indexes_supply_titles() {
    let dir = tempdir().unwrap();
    let models = dir.path().join("cache/models.json");
    let config = dir.path().join("missing-config.json");

    let codex = dir.path().join("codex");
    let codex_session = codex.join("sessions/2026/07/18/rollout.jsonl");
    fs::create_dir_all(codex_session.parent().unwrap()).unwrap();
    fs::write(
        &codex_session,
        concat!(
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"codex-stable\"}}\n",
            "{\"type\":\"turn_context\",\"payload\":{\"model\":\"gpt-test\"}}\n",
            "{\"type\":\"event_msg\",\"timestamp\":\"2026-07-18T10:00:00Z\",\"payload\":{\"type\":\"token_count\",\"info\":{\"total_token_usage\":{\"input_tokens\":100,\"output_tokens\":10}}}}\n"
        ),
    )
    .unwrap();
    fs::write(
        codex.join("session_index.jsonl"),
        "{\"id\":\"codex-stable\",\"thread_name\":\"Indexed Codex title\"}\n",
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_usct"))
        .args(["codex", "session", "--no-cost", "--no-color"])
        .env("CODEX_HOME", &codex)
        .env("USCT_MODELS_PATH", &models)
        .env("USCT_CONFIG", &config)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("Indexed Codex title")
    );

    let opencode = dir.path().join("opencode");
    let opencode_message = opencode.join("storage/message/open-stable/message.json");
    fs::create_dir_all(opencode_message.parent().unwrap()).unwrap();
    fs::create_dir_all(opencode.join("storage/session")).unwrap();
    fs::write(
        &opencode_message,
        "{\"id\":\"message\",\"sessionID\":\"open-stable\",\"modelID\":\"gpt-test\",\"created\":1784361600000,\"tokens\":{\"input\":100,\"output\":10}}\n",
    )
    .unwrap();
    fs::write(
        opencode.join("storage/session/open-stable.json"),
        "{\"id\":\"open-stable\",\"title\":\"Indexed OpenCode title\"}\n",
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_usct"))
        .args(["opencode", "session", "--json", "--no-cost"])
        .env("OPENCODE_DATA_DIR", &opencode)
        .env("USCT_MODELS_PATH", &models)
        .env("USCT_CONFIG", &config)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["session"][0]["period"], "open-stable");
    assert_eq!(
        report["session"][0]["metadata"]["sessionTitle"],
        "Indexed OpenCode title"
    );
}

#[test]
fn every_human_table_mode_aligns_variable_width_fields() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("claude");
    let session = root.join("projects/demo/session.jsonl");
    fs::create_dir_all(session.parent().unwrap()).unwrap();
    fs::write(
        &session,
        concat!(
            "{\"type\":\"assistant\",\"timestamp\":\"2026-07-18T10:00:00Z\",\"message\":{\"id\":\"one\",\"model\":\"a-short-model\",\"usage\":{\"input_tokens\":100,\"output_tokens\":10}}}\n",
            "{\"type\":\"assistant\",\"timestamp\":\"2026-07-18T10:01:00Z\",\"message\":{\"id\":\"two\",\"model\":\"model-with-an-extraordinarily-long-provider-specific-name\",\"usage\":{\"input_tokens\":200,\"output_tokens\":20}}}\n"
        ),
    )
    .unwrap();
    let models = dir.path().join("cache/models.json");
    let config = dir.path().join("missing-config.json");
    let run = |arguments: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_usct"))
            .args(arguments)
            .env("CLAUDE_CONFIG_DIR", &root)
            .env("USCT_MODELS_PATH", &models)
            .env("USCT_CONFIG", &config)
            .output()
            .unwrap()
    };
    let edge = |line: &str, value: &str| {
        UnicodeWidthStr::width(&line[..line.rfind(value).unwrap()]) + UnicodeWidthStr::width(value)
    };

    let full = run(&[
        "claude",
        "daily",
        "--breakdown",
        "--no-cost",
        "--no-color",
        "--timezone",
        "UTC",
    ]);
    assert!(
        full.status.success(),
        "{}",
        String::from_utf8_lossy(&full.stderr)
    );
    let full = String::from_utf8(full.stdout).unwrap();
    let full_lines: Vec<_> = full.lines().collect();
    assert_eq!(edge(full_lines[0], "Total"), edge(full_lines[1], "330"));
    assert_eq!(edge(full_lines[0], "Total"), edge(full_lines[2], "110"));
    assert_eq!(edge(full_lines[0], "Total"), edge(full_lines[3], "220"));

    let compact = run(&[
        "claude",
        "daily",
        "--compact",
        "--breakdown",
        "--no-cost",
        "--no-color",
        "--timezone",
        "UTC",
    ]);
    assert!(
        compact.status.success(),
        "{}",
        String::from_utf8_lossy(&compact.stderr)
    );
    let compact = String::from_utf8(compact.stdout).unwrap();
    let compact_lines: Vec<_> = compact.lines().collect();
    assert_eq!(edge(compact_lines[0], "330"), edge(compact_lines[1], "110"));
    assert_eq!(edge(compact_lines[0], "330"), edge(compact_lines[2], "220"));

    let blocks = run(&[
        "claude",
        "blocks",
        "--compact",
        "--breakdown",
        "--no-cost",
        "--no-color",
        "--timezone",
        "UTC",
    ]);
    assert!(
        blocks.status.success(),
        "{}",
        String::from_utf8_lossy(&blocks.stderr)
    );
    let blocks = String::from_utf8(blocks.stdout).unwrap();
    let block_lines: Vec<_> = blocks.lines().collect();
    assert_eq!(edge(block_lines[0], "330"), edge(block_lines[1], "110"));
    assert_eq!(edge(block_lines[0], "330"), edge(block_lines[2], "220"));
    for output in [&full, &compact, &blocks] {
        assert!(output.contains("model-with-an-extraordinarily-long-provider-specific-name"));
        assert!(!output.lines().any(|line| line.ends_with(' ')));
    }
}
