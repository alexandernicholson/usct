use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveDateTime, Timelike, Utc};
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

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
        if matches!(self, Self::All | Self::Session) {
            return Ok(None);
        }
        let (today, current_hour) = local_now()?;
        let (label, date, hour) = match self {
            Self::Hour => ("hour", today, Some(current_hour)),
            Self::Day => ("day", today, None),
            Self::Week => {
                let date = today - Duration::days(today.weekday().num_days_from_monday().into());
                ("week", date, None)
            }
            Self::Month => (
                "month",
                NaiveDate::from_ymd_opt(today.year(), today.month(), 1).expect("valid month"),
                None,
            ),
            Self::Year => (
                "year",
                NaiveDate::from_ymd_opt(today.year(), 1, 1).expect("valid year"),
                None,
            ),
            Self::All | Self::Session => unreachable!("handled above"),
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

fn local_now() -> Result<(NaiveDate, u32), String> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("system clock predates Unix epoch: {error}"))?
        .as_secs();
    let timestamp = libc::time_t::try_from(seconds)
        .map_err(|_| "system clock exceeds local time range".to_owned())?;
    let local = local_tm(timestamp)?;
    let date = NaiveDate::from_ymd_opt(
        local.tm_year + 1900,
        u32::try_from(local.tm_mon + 1).expect("valid local month"),
        u32::try_from(local.tm_mday).expect("valid local day"),
    )
    .ok_or_else(|| "system returned invalid local date".to_owned())?;
    Ok((
        date,
        u32::try_from(local.tm_hour).expect("valid local hour"),
    ))
}

fn local_timestamp(value: NaiveDateTime) -> Result<i64, String> {
    let mut earliest: Option<libc::time_t> = None;
    for is_dst in [0, 1] {
        let mut local: libc::tm = unsafe { std::mem::zeroed() };
        local.tm_sec = i32::try_from(value.second()).expect("valid second");
        local.tm_min = i32::try_from(value.minute()).expect("valid minute");
        local.tm_hour = i32::try_from(value.hour()).expect("valid hour");
        local.tm_mday = i32::try_from(value.day()).expect("valid day");
        local.tm_mon = i32::try_from(value.month0()).expect("valid month");
        local.tm_year = value.year() - 1900;
        local.tm_isdst = is_dst;
        // SAFETY: `local` is initialized, writable, and contains bounded calendar fields.
        let timestamp = unsafe { libc::mktime(&mut local) };
        if tm_matches(&local, value) {
            earliest = Some(earliest.map_or(timestamp, |current| current.min(timestamp)));
        }
    }
    earliest
        .and_then(|timestamp| timestamp.checked_mul(1000))
        .ok_or_else(|| format!("local time '{value}' does not exist"))
}

fn local_tm(timestamp: libc::time_t) -> Result<libc::tm, String> {
    let mut local: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: both pointers are valid for the duration of the call and do not alias.
    let result = unsafe { libc::localtime_r(&timestamp, &mut local) };
    if result.is_null() {
        Err("cannot resolve current local time".to_owned())
    } else {
        Ok(local)
    }
}

fn tm_matches(local: &libc::tm, value: NaiveDateTime) -> bool {
    local.tm_sec == i32::try_from(value.second()).expect("valid second")
        && local.tm_min == i32::try_from(value.minute()).expect("valid minute")
        && local.tm_hour == i32::try_from(value.hour()).expect("valid hour")
        && local.tm_mday == i32::try_from(value.day()).expect("valid day")
        && local.tm_mon == i32::try_from(value.month0()).expect("valid month")
        && local.tm_year == value.year() - 1900
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_timestamp_round_trips_calendar_fields() {
        let value = NaiveDate::from_ymd_opt(2026, 7, 16)
            .unwrap()
            .and_hms_opt(12, 34, 56)
            .unwrap();
        let timestamp = local_timestamp(value).unwrap() / 1000;
        assert!(tm_matches(&local_tm(timestamp).unwrap(), value));
    }

    #[test]
    fn day_period_starts_at_local_midnight() {
        let range = Period::Day.range().unwrap().unwrap();
        let local = local_tm(range.start_ms / 1000).unwrap();
        assert_eq!((local.tm_hour, local.tm_min, local.tm_sec), (0, 0, 0));
    }
}
