use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub reasoning: u64,
}

impl TokenUsage {
    pub fn add_assign(&mut self, other: Self) {
        self.input = self.input.saturating_add(other.input);
        self.output = self.output.saturating_add(other.output);
        self.cache_read = self.cache_read.saturating_add(other.cache_read);
        self.cache_write = self.cache_write.saturating_add(other.cache_write);
        self.reasoning = self.reasoning.saturating_add(other.reasoning);
    }

    pub fn saturating_sub(self, earlier: Self) -> Self {
        Self {
            input: self.input.saturating_sub(earlier.input),
            output: self.output.saturating_sub(earlier.output),
            cache_read: self.cache_read.saturating_sub(earlier.cache_read),
            cache_write: self.cache_write.saturating_sub(earlier.cache_write),
            reasoning: self.reasoning.saturating_sub(earlier.reasoning),
        }
    }

    pub fn is_empty(self) -> bool {
        self == Self::default()
    }
}

impl TokenUsage {
    pub fn delta_from(self, previous: Self) -> Self {
        if self.input < previous.input
            || self.output < previous.output
            || self.cache_read < previous.cache_read
            || self.cache_write < previous.cache_write
            || self.reasoning < previous.reasoning
        {
            self
        } else {
            self.saturating_sub(previous)
        }
    }

    pub fn total(items: impl IntoIterator<Item = Self>) -> Self {
        let mut total = Self::default();
        for item in items {
            total.add_assign(item);
        }
        total
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelUsage {
    pub model: String,
    pub usage: TokenUsage,
}

impl ModelUsage {
    pub fn add_to(models: &mut Vec<Self>, model: &str, usage: TokenUsage) {
        if usage.is_empty() {
            return;
        }
        if let Some(existing) = models.iter_mut().find(|item| item.model == model) {
            existing.usage.add_assign(usage);
        } else {
            models.push(Self {
                model: model.to_owned(),
                usage,
            });
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Price {
    pub input: f64,
    pub output: f64,
    pub cache_read: Option<f64>,
    pub cache_write: Option<f64>,
    pub reasoning: Option<f64>,
}

impl Price {
    pub fn cost(self, usage: TokenUsage) -> f64 {
        let million = 1_000_000.0;
        (usage.input as f64 * self.input
            + usage.output as f64 * self.output
            + usage.cache_read as f64 * self.cache_read.unwrap_or(self.input)
            + usage.cache_write as f64 * self.cache_write.unwrap_or(self.input)
            + usage.reasoning as f64 * self.reasoning.unwrap_or(self.output))
            / million
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PricedModelUsage {
    pub model: String,
    pub usage: TokenUsage,
    pub price: Price,
    pub cost_usd: f64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageRecord {
    pub models: Vec<ModelUsage>,
}

impl UsageRecord {
    pub fn usage(&self) -> TokenUsage {
        TokenUsage::total(self.models.iter().map(|item| item.usage))
    }
}
