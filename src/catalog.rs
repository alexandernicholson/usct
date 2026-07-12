use crate::domain::Price;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, fs, path::Path};

#[derive(Debug, Deserialize, Serialize)]
struct Provider {
    #[serde(default)]
    models: HashMap<String, Model>,
}

#[derive(Debug, Deserialize, Serialize)]
struct Model {
    #[serde(default)]
    id: String,
    cost: Option<ModelCost>,
}

#[derive(Debug, Deserialize, Serialize)]
struct ModelCost {
    input: f64,
    output: f64,
    cache_read: Option<f64>,
    cache_write: Option<f64>,
    reasoning: Option<f64>,
}

pub trait PricingCatalog {
    fn find(&self, model: &str) -> Option<Price>;
}

pub struct ModelsDevCatalog {
    providers: HashMap<String, Provider>,
}

impl ModelsDevCatalog {
    pub fn from_slice(bytes: &[u8]) -> Result<Self, String> {
        serde_json::from_slice(bytes)
            .map(|providers| Self { providers })
            .map_err(|error| format!("invalid models.dev catalog: {error}"))
    }

    pub fn from_path(path: &Path) -> Result<Self, String> {
        let bytes =
            fs::read(path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        Self::from_slice(&bytes)
    }

    pub fn find(&self, model: &str) -> Option<Price> {
        <Self as PricingCatalog>::find(self, model)
    }

    pub fn to_compact_vec(&self) -> Result<Vec<u8>, String> {
        serde_json::to_vec(&self.providers)
            .map_err(|error| format!("cannot encode models.dev cache: {error}"))
    }
}

impl PricingCatalog for ModelsDevCatalog {
    fn find(&self, requested: &str) -> Option<Price> {
        let requested = requested.trim();
        let qualified = requested.split_once('/');
        let model = qualified.map_or(requested, |(_, model)| model);
        let provider = qualified.map(|(provider, _)| provider);

        self.providers.iter().find_map(|(provider_id, entry)| {
            if provider.is_some_and(|wanted| wanted != provider_id) {
                return None;
            }
            entry.models.iter().find_map(|(key, candidate)| {
                let exact = key == model || candidate.id == model;
                let normalized = normalize(key) == normalize(model)
                    || normalize(&candidate.id) == normalize(model);
                (exact || normalized)
                    .then_some(candidate.cost.as_ref())
                    .flatten()
                    .map(to_price)
            })
        })
    }
}

fn normalize(value: &str) -> String {
    value.to_ascii_lowercase().replace(['.', '_'], "-")
}

fn to_price(cost: &ModelCost) -> Price {
    Price {
        input: cost.input,
        output: cost.output,
        cache_read: cost.cache_read,
        cache_write: cost.cache_write,
        reasoning: cost.reasoning,
    }
}
