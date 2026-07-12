use std::{fs, process::Command};
use tempfile::tempdir;
use usct::{
    app,
    catalog::ModelsDevCatalog,
    domain::{Price, TokenUsage},
    session::{Harness, parse_session},
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
