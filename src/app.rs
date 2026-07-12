use crate::{
    catalog::ModelsDevCatalog,
    domain::{Price, UsageRecord},
    session::{Harness, parse_session_in_range},
    time_range::TimeRange,
};
use std::path::Path;

#[derive(Debug)]
pub struct CostReport {
    pub harness: Harness,
    pub path: String,
    pub record: UsageRecord,
    pub price: Price,
    pub cost: f64,
}

pub fn calculate(
    harness: Harness,
    path: &Path,
    catalog: &ModelsDevCatalog,
) -> Result<CostReport, String> {
    calculate_in_range(harness, path, catalog, None)
}

pub fn calculate_in_range(
    harness: Harness,
    path: &Path,
    catalog: &ModelsDevCatalog,
    range: Option<&TimeRange>,
) -> Result<CostReport, String> {
    let record = parse_session_in_range(harness, path, range)?;
    price_record(harness, path, catalog, record)
}

pub fn price_record(
    harness: Harness,
    path: &Path,
    catalog: &ModelsDevCatalog,
    record: UsageRecord,
) -> Result<CostReport, String> {
    let pricing_id = pricing_id(harness, &record.model);
    let price = catalog.find(&pricing_id).ok_or_else(|| {
        format!(
            "model '{}' is absent from the models.dev cache",
            record.model
        )
    })?;
    let cost = price.cost(record.usage);
    Ok(CostReport {
        harness,
        path: path.display().to_string(),
        record,
        price,
        cost,
    })
}

#[derive(Debug)]
pub struct AggregateReport {
    pub sources: Vec<String>,
    pub session_count: usize,
    pub usage: crate::domain::TokenUsage,
    pub cost: f64,
}

pub fn calculate_many(
    sessions: &[(Harness, std::path::PathBuf)],
    catalog: &ModelsDevCatalog,
) -> Result<AggregateReport, String> {
    let mut sources = Vec::new();
    let mut usage = crate::domain::TokenUsage::default();
    let mut cost = 0.0;
    let mut session_count = 0;
    for (harness, path) in sessions {
        let report = match calculate(*harness, path, catalog) {
            Ok(report) => report,
            Err(error) if error == "session contains no token usage" => continue,
            Err(error) => return Err(format!("{}: {error}", path.display())),
        };
        let source = harness.name().to_owned();
        if !sources.contains(&source) {
            sources.push(source);
        }
        usage.add_assign(report.record.usage);
        cost += report.cost;
        session_count += 1;
    }
    if session_count == 0 {
        return Err("sessions contain no token usage".to_owned());
    }
    sources.sort();
    Ok(AggregateReport {
        sources,
        session_count,
        usage,
        cost,
    })
}

fn pricing_id(harness: Harness, model: &str) -> String {
    if model.contains('/') {
        return model.to_owned();
    }
    let provider = if model.starts_with("claude") {
        Some("anthropic")
    } else if model.starts_with("gpt") || model.starts_with('o') {
        Some("openai")
    } else if model.starts_with("gemini") {
        Some("google")
    } else {
        match harness {
            Harness::Claude => Some("anthropic"),
            Harness::Codex | Harness::Omp => Some("openai"),
            Harness::Gemini => Some("google"),
            Harness::Pi | Harness::OpenCode | Harness::Amp => None,
        }
    };
    provider.map_or_else(
        || model.to_owned(),
        |provider| format!("{provider}/{model}"),
    )
}
