use chrono::{
    DateTime, Datelike, Duration, Local, LocalResult, NaiveDate, NaiveDateTime, TimeZone, Timelike,
    Utc,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeRange {
    pub label: String,
    pub start_ms: i64,
    pub end_ms: Option<i64>,
}

impl TimeRange {
    pub fn contains(&self, timestamp_ms: i64) -> bool {
        timestamp_ms >= self.start_ms && self.end_ms.is_none_or(|end| timestamp_ms < end)
    }

    pub fn cache_key(&self) -> String {
        format!(
            "{}:{}:{}",
            self.label,
            self.start_ms,
            self.end_ms
                .map_or_else(|| "open".to_owned(), |value| value.to_string())
        )
    }

    pub fn start_rfc3339(&self) -> String {
        DateTime::<Utc>::from_timestamp_millis(self.start_ms)
            .expect("valid range timestamp")
            .to_rfc3339()
    }

    pub fn end_rfc3339(&self) -> Option<String> {
        self.end_ms.map(|value| {
            DateTime::<Utc>::from_timestamp_millis(value)
                .expect("valid range timestamp")
                .to_rfc3339()
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Period {
    All,
    Session,
    Hour,
    Day,
    Week,
    Month,
    Year,
}

impl Period {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "all" => Ok(Self::All),
            "session" => Ok(Self::Session),
            "hour" => Ok(Self::Hour),
            "day" => Ok(Self::Day),
            "week" => Ok(Self::Week),
            "month" => Ok(Self::Month),
            "year" => Ok(Self::Year),
            _ => Err(format!("unsupported period '{value}'")),
        }
    }

    pub fn range(self) -> Result<Option<TimeRange>, String> {
        let now = Local::now();
        let (label, date, hour) = match self {
            Self::All | Self::Session => return Ok(None),
            Self::Hour => ("hour", now.date_naive(), Some(now.hour())),
            Self::Day => ("day", now.date_naive(), None),
            Self::Week => {
                let date =
                    now.date_naive() - Duration::days(now.weekday().num_days_from_monday().into());
                ("week", date, None)
            }
            Self::Month => (
                "month",
                NaiveDate::from_ymd_opt(now.year(), now.month(), 1).expect("valid month"),
                None,
            ),
            Self::Year => (
                "year",
                NaiveDate::from_ymd_opt(now.year(), 1, 1).expect("valid year"),
                None,
            ),
        };
        let naive = date
            .and_hms_opt(hour.unwrap_or(0), 0, 0)
            .expect("valid local boundary");
        Ok(Some(TimeRange {
            label: label.to_owned(),
            start_ms: local_timestamp(naive)?,
            end_ms: None,
        }))
    }
}

pub fn custom_range(from: &str, to: Option<&str>) -> Result<TimeRange, String> {
    let start_ms = parse_boundary(from)?;
    let end_ms = to.map(parse_boundary).transpose()?;
    if end_ms.is_some_and(|end| end <= start_ms) {
        return Err("custom range end must be later than start".to_owned());
    }
    Ok(TimeRange {
        label: "custom".to_owned(),
        start_ms,
        end_ms,
    })
}

pub fn parse_timestamp(value: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|value| value.timestamp_millis())
}

fn parse_boundary(value: &str) -> Result<i64, String> {
    if let Ok(timestamp) = DateTime::parse_from_rfc3339(value) {
        return Ok(timestamp.timestamp_millis());
    }
    if let Ok(date) = NaiveDate::parse_from_str(value, "%Y-%m-%d") {
        return local_timestamp(date.and_hms_opt(0, 0, 0).expect("valid midnight"));
    }
    if let Ok(timestamp) = NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S") {
        return local_timestamp(timestamp);
    }
    Err(format!("invalid date or RFC 3339 timestamp '{value}'"))
}

fn local_timestamp(value: NaiveDateTime) -> Result<i64, String> {
    match Local.from_local_datetime(&value) {
        LocalResult::Single(timestamp) => Ok(timestamp.timestamp_millis()),
        LocalResult::Ambiguous(earlier, _) => Ok(earlier.timestamp_millis()),
        LocalResult::None => Err(format!("local time '{value}' does not exist")),
    }
}
