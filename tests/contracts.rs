use std::{fs, io::Write, process::Command};
use tempfile::tempdir;
use usct::{
    app, cache,
    catalog::ModelsDevCatalog,
    domain::{Price, TokenUsage},
    session::{Harness, parse_session, parse_session_in_range, parse_session_incremental},
    time_range::{Period, custom_range},
};

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
    assert_eq!(record.model, "claude-sonnet-4");
    assert_eq!(record.usage.input, 105);
    assert_eq!(record.usage.output, 23);
    assert_eq!(record.usage.cache_read, 50);
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
    assert_eq!(record.usage.input, 110);
    assert_eq!(record.usage.cache_read, 60);
    assert_eq!(record.usage.output, 27);
    assert_eq!(record.usage.reasoning, 8);
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
        assert_eq!(record.model, "shared-test");
        assert_eq!(
            record.usage.input,
            if harness == Harness::Gemini { 50 } else { 80 }
        );
        assert_eq!(record.usage.cache_read, 30);
        assert_eq!(record.usage.output, 15);
        assert_eq!(record.usage.reasoning, 5);
        assert_eq!(record.usage.cache_write, 4);
    }
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
    assert_eq!(record.usage.input, 160);
    assert_eq!(record.usage.output, 40);
    assert_eq!(record.usage.cache_read, 60);
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
    assert_eq!(record.usage.input, 100);
    assert_eq!(record.usage.output, 30);
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
    assert_eq!(record.usage.input, 50);
    assert_eq!(record.usage.cache_read, 30);
    assert_eq!(record.usage.output, 15);
}

#[test]
fn incremental_omp_parser_reads_only_appended_records() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    let first = "{\"type\":\"message\",\"timestamp\":\"2026-07-12T00:00:00Z\",\"message\":{\"role\":\"assistant\",\"model\":\"gpt-test\",\"usage\":{\"input\":100,\"output\":10}}}\n";
    let second = "{\"type\":\"message\",\"timestamp\":\"2026-07-12T00:01:00Z\",\"message\":{\"role\":\"assistant\",\"model\":\"gpt-test\",\"usage\":{\"input\":50,\"output\":5}}}\n";
    fs::write(&path, first).unwrap();
    let (record, state) = parse_session_incremental(Harness::Omp, &path, None, None).unwrap();
    assert_eq!(record.usage.input, 100);
    fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(second.as_bytes())
        .unwrap();
    let (record, _) = parse_session_incremental(Harness::Omp, &path, None, state).unwrap();
    assert_eq!(record.usage.input, 150);
    assert_eq!(record.usage.output, 15);
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
    assert_eq!(record.usage.input, 150);
    assert_eq!(record.usage.output, 15);
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
    assert_eq!(record.usage.input, 80);
    assert_eq!(record.usage.cache_read, 20);
    assert_eq!(record.usage.output, 10);
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
    assert_eq!(record.usage.input, 900);
    assert_eq!(record.usage.output, 90);
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
    assert_eq!(record.usage.input, 50);
    fs::OpenOptions::new().append(true).open(&path).unwrap().write_all(
        b"{\"timestamp\":\"2026-07-12T00:02:00Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\"total_token_usage\":{\"input_tokens\":250,\"cached_input_tokens\":70,\"output_tokens\":30}}}}\n"
    ).unwrap();
    let (record, _) =
        parse_session_incremental(Harness::Codex, &path, Some(&range), state).unwrap();
    assert_eq!(record.usage.input, 100);
    assert_eq!(record.usage.cache_read, 50);
    assert_eq!(record.usage.output, 20);
}

#[test]
fn incremental_parser_defers_partial_trailing_jsonl_records() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    let complete = "{\"type\":\"message\",\"message\":{\"role\":\"assistant\",\"model\":\"gpt-test\",\"usage\":{\"input\":100,\"output\":10}}}\n";
    let partial = "{\"type\":\"message\",\"message\":{\"role\":\"assistant\",\"model\":\"gpt-test\",\"usage\":{\"input\":50";
    fs::write(&path, format!("{complete}{partial}")).unwrap();
    let (record, state) = parse_session_incremental(Harness::Omp, &path, None, None).unwrap();
    assert_eq!(record.usage.input, 100);
    fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b",\"output\":5}}}\n")
        .unwrap();
    let (record, _) = parse_session_incremental(Harness::Omp, &path, None, state).unwrap();
    assert_eq!(record.usage.input, 150);
    assert_eq!(record.usage.output, 15);
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
    assert_eq!(record.usage.input, 100);
    assert_eq!(record.usage.output, 10);
    assert_eq!(record.usage.cache_read, 20);
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
    assert_eq!(record.usage.input, 150);
    assert_eq!(record.usage.output, 9);
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
    assert_eq!(record.usage.input, 70);
    assert_eq!(record.usage.output, 7);
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
            model: "gpt-test".to_owned(),
            usage,
            price,
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
            model: "gpt-test".to_owned(),
            usage,
            price,
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
    assert_eq!(previous.model, "gpt-test");
    assert_eq!(previous.price, price);
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
            model: "gpt-test".to_owned(),
            usage: TokenUsage::default(),
            price,
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
