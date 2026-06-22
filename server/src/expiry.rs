use chrono::{DateTime, Days, Local, LocalResult, Months, NaiveDate, NaiveDateTime, TimeZone};
use serde::{Deserialize, Deserializer, Serialize};

fn default_days() -> Vec<i64> {
    vec![30, 14, 7, 3, 1, 0]
}

fn default_interval() -> u64 {
    86_400
}

fn deserialize_string<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let value = toml::Value::deserialize(deserializer)?;
    Ok(match value {
        toml::Value::String(v) => v,
        toml::Value::Integer(v) => v.to_string(),
        toml::Value::Float(v) => v.to_string(),
        toml::Value::Boolean(v) => v.to_string(),
        toml::Value::Datetime(v) => v.to_string(),
        _ => String::new(),
    })
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ExpireNotifyConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_days")]
    pub days: Vec<i64>,
    #[serde(default = "default_interval")]
    pub interval: u64,
}

impl Default for ExpireNotifyConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            days: default_days(),
            interval: default_interval(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct BillingConfig {
    #[serde(default, alias = "startDate")]
    pub start_date: String,
    #[serde(default, alias = "endDate")]
    pub end_date: String,
    #[serde(default, alias = "autoRenewal", deserialize_with = "deserialize_string")]
    pub auto_renewal: String,
    #[serde(default)]
    pub cycle: String,
    #[serde(default)]
    pub amount: String,
}

impl BillingConfig {
    fn auto_renewal_enabled(&self) -> bool {
        matches!(
            self.auto_renewal.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpireInfo {
    pub configured: bool,
    pub source: String,
    pub raw: String,
    pub original_date: String,
    pub original_timestamp: i64,
    pub date: String,
    pub timestamp: i64,
    pub days_left: i64,
    pub status: String,
    pub label: String,
    pub start_date: String,
    pub auto_renewal: bool,
    pub auto_renewed: bool,
    pub renewal_count: u32,
    pub renewal_status: String,
    pub cycle: String,
    pub amount: String,
}

impl Default for ExpireInfo {
    fn default() -> Self {
        Self {
            configured: false,
            source: String::new(),
            raw: String::new(),
            original_date: String::new(),
            original_timestamp: 0,
            date: String::new(),
            timestamp: 0,
            days_left: 0,
            status: "none".to_string(),
            label: String::new(),
            start_date: String::new(),
            auto_renewal: false,
            auto_renewed: false,
            renewal_count: 0,
            renewal_status: "off".to_string(),
            cycle: String::new(),
            amount: String::new(),
        }
    }
}

enum RenewalCycle {
    Days(u64),
    Months(u32),
}

enum ParsedExpire {
    Permanent,
    Timestamp(i64),
}

pub fn build_expire_info(expire: &str, billing: &BillingConfig, labels: &str) -> ExpireInfo {
    let (raw, source) = expire_source(expire, billing, labels);
    let mut info = ExpireInfo {
        configured: !raw.is_empty(),
        source,
        raw,
        start_date: billing.start_date.clone(),
        auto_renewal: billing.auto_renewal_enabled(),
        cycle: billing.cycle.clone(),
        amount: billing.amount.clone(),
        ..Default::default()
    };

    if !info.configured {
        return info;
    }

    match parse_expire(&info.raw) {
        Some(ParsedExpire::Permanent) => {
            info.status = "permanent".to_string();
            info.label = "permanent".to_string();
        }
        Some(ParsedExpire::Timestamp(timestamp)) => {
            info.original_timestamp = timestamp;
            info.original_date = date_label(timestamp);
            let timestamp = apply_auto_renewal(&mut info);
            apply_timestamp(&mut info, timestamp);
        }
        None => {
            info.status = "unknown".to_string();
            info.label = "invalid expire date".to_string();
        }
    }

    info
}

pub fn refresh_expire_info(info: &mut ExpireInfo) {
    if info.original_timestamp > 0 {
        let timestamp = apply_auto_renewal(info);
        apply_timestamp(info, timestamp);
    } else if info.timestamp > 0 {
        apply_timestamp(info, info.timestamp);
    }
}

pub fn alert_marker(info: &ExpireInfo, days: &[i64]) -> Option<String> {
    if !info.configured || info.status == "permanent" || info.status == "unknown" {
        return None;
    }

    if info.days_left < 0 {
        return Some(format!("{}:expired", info.timestamp));
    }

    let mut matched_days = days
        .iter()
        .copied()
        .filter(|day| *day >= 0 && info.days_left <= *day)
        .collect::<Vec<_>>();
    matched_days.sort_unstable();

    matched_days.first().map(|day| format!("{}:{day}", info.timestamp))
}

fn expire_source(expire: &str, billing: &BillingConfig, labels: &str) -> (String, String) {
    if !expire.trim().is_empty() {
        return (expire.trim().to_string(), "expire".to_string());
    }

    if !billing.end_date.trim().is_empty() {
        return (billing.end_date.trim().to_string(), "billing.end_date".to_string());
    }

    label_value(labels, &["expire", "expires", "end_date", "enddate", "ndd"]).map_or_else(
        || (String::new(), String::new()),
        |(key, value)| (value, format!("labels.{key}")),
    )
}

fn label_value(labels: &str, keys: &[&str]) -> Option<(String, String)> {
    for part in labels.split(';') {
        let Some((key, value)) = part.split_once('=') else {
            continue;
        };
        let key_normalized = key.trim().to_ascii_lowercase();
        if keys.contains(&key_normalized.as_str()) {
            return Some((key_normalized, value.trim().to_string()));
        }
    }

    None
}

fn parse_expire(raw: &str) -> Option<ParsedExpire> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let raw_lower = raw.to_ascii_lowercase();
    if raw.starts_with("0000-00-00") || matches!(raw_lower.as_str(), "never" | "permanent" | "lifetime" | "forever") {
        return Some(ParsedExpire::Permanent);
    }

    if let Ok(dt) = DateTime::parse_from_rfc3339(raw) {
        return Some(ParsedExpire::Timestamp(dt.timestamp()));
    }

    for fmt in ["%Y-%m-%d %H:%M:%S", "%Y/%m/%d %H:%M:%S", "%Y.%m.%d %H:%M:%S"] {
        if let Ok(dt) = NaiveDateTime::parse_from_str(raw, fmt) {
            return local_timestamp(dt).map(ParsedExpire::Timestamp);
        }
    }

    for fmt in ["%Y-%m-%d", "%Y/%m/%d", "%Y.%m.%d", "%Y%m%d"] {
        if let Ok(date) = NaiveDate::parse_from_str(raw, fmt) {
            let dt = date.and_hms_opt(23, 59, 59)?;
            return local_timestamp(dt).map(ParsedExpire::Timestamp);
        }
    }

    None
}

fn apply_timestamp(info: &mut ExpireInfo, timestamp: i64) {
    info.timestamp = timestamp;

    if let Some(expire_dt) = DateTime::from_timestamp(timestamp, 0) {
        let expire_date = expire_dt.with_timezone(&Local).date_naive();
        info.date = expire_date.format("%Y-%m-%d").to_string();

        let today = Local::now().date_naive();
        info.days_left = expire_date.signed_duration_since(today).num_days();
        if info.days_left < 0 {
            info.status = "expired".to_string();
            info.label = format!("expired {} day(s) ago", info.days_left.abs());
        } else if info.days_left == 0 {
            info.status = "warning".to_string();
            info.label = if info.auto_renewal {
                "auto renews today".to_string()
            } else {
                "expires today".to_string()
            };
        } else if info.days_left <= 7 {
            info.status = "warning".to_string();
            info.label = if info.auto_renewal {
                format!("auto renews in {} day(s)", info.days_left)
            } else {
                format!("expires in {} day(s)", info.days_left)
            };
        } else {
            info.status = "normal".to_string();
            info.label = if info.auto_renewal {
                format!("auto renews in {} day(s)", info.days_left)
            } else {
                format!("expires in {} day(s)", info.days_left)
            };
        }
    }
}

fn apply_auto_renewal(info: &mut ExpireInfo) -> i64 {
    info.auto_renewed = false;
    info.renewal_count = 0;

    if !info.auto_renewal {
        info.renewal_status = "off".to_string();
        return info.original_timestamp;
    }

    let Some(cycle) = parse_renewal_cycle(&info.cycle) else {
        info.renewal_status = if info.cycle.trim().is_empty() {
            "missing_cycle".to_string()
        } else {
            "invalid_cycle".to_string()
        };
        return info.original_timestamp;
    };

    let Some(initial_date) = local_date(info.original_timestamp) else {
        info.renewal_status = "invalid_date".to_string();
        return info.original_timestamp;
    };

    let today = Local::now().date_naive();
    let mut next_date = initial_date;
    let mut renewal_count = 0_u32;

    while next_date < today {
        let Some(date) = add_cycle(next_date, &cycle) else {
            info.renewal_status = "invalid_cycle".to_string();
            return info.original_timestamp;
        };
        if date <= next_date {
            info.renewal_status = "invalid_cycle".to_string();
            return info.original_timestamp;
        }
        next_date = date;
        renewal_count += 1;
        if renewal_count > 1200 {
            info.renewal_status = "too_many_cycles".to_string();
            return info.original_timestamp;
        }
    }

    info.renewal_count = renewal_count;
    info.auto_renewed = renewal_count > 0;
    info.renewal_status = if info.auto_renewed { "renewed" } else { "waiting" }.to_string();

    local_timestamp(next_date.and_hms_opt(23, 59, 59).unwrap_or_default()).unwrap_or(info.original_timestamp)
}

fn parse_renewal_cycle(cycle: &str) -> Option<RenewalCycle> {
    let normalized = cycle.trim().to_ascii_lowercase().replace([' ', '_', '-'], "");
    if normalized.is_empty() {
        return None;
    }

    match normalized.as_str() {
        "day" | "daily" | "d" => return Some(RenewalCycle::Days(1)),
        "week" | "weekly" | "w" => return Some(RenewalCycle::Days(7)),
        "month" | "monthly" | "m" | "mo" => return Some(RenewalCycle::Months(1)),
        "quarter" | "quarterly" | "q" | "season" => return Some(RenewalCycle::Months(3)),
        "halfyear" | "semiannual" | "semiannually" => return Some(RenewalCycle::Months(6)),
        "year" | "yearly" | "annual" | "annually" | "y" => return Some(RenewalCycle::Months(12)),
        _ => {}
    }

    let (number, unit) = split_cycle_number_unit(&normalized)?;
    if number == 0 {
        return None;
    }

    match unit {
        "" | "d" | "day" | "days" => Some(RenewalCycle::Days(number.into())),
        "w" | "week" | "weeks" => Some(RenewalCycle::Days(u64::from(number) * 7)),
        "m" | "mo" | "month" | "months" => Some(RenewalCycle::Months(number)),
        "q" | "quarter" | "quarters" => number.checked_mul(3).map(RenewalCycle::Months),
        "y" | "year" | "years" => number.checked_mul(12).map(RenewalCycle::Months),
        _ => None,
    }
}

fn split_cycle_number_unit(value: &str) -> Option<(u32, &str)> {
    let digit_len = value.chars().take_while(char::is_ascii_digit).map(char::len_utf8).sum();
    if digit_len == 0 {
        return None;
    }

    let number = value[..digit_len].parse::<u32>().ok()?;
    Some((number, &value[digit_len..]))
}

fn add_cycle(date: NaiveDate, cycle: &RenewalCycle) -> Option<NaiveDate> {
    match cycle {
        RenewalCycle::Days(days) => date.checked_add_days(Days::new(*days)),
        RenewalCycle::Months(months) => date.checked_add_months(Months::new(*months)),
    }
}

fn local_date(timestamp: i64) -> Option<NaiveDate> {
    DateTime::from_timestamp(timestamp, 0).map(|dt| dt.with_timezone(&Local).date_naive())
}

fn date_label(timestamp: i64) -> String {
    local_date(timestamp).map_or_else(String::new, |date| date.format("%Y-%m-%d").to_string())
}

fn local_timestamp(dt: NaiveDateTime) -> Option<i64> {
    match Local.from_local_datetime(&dt) {
        LocalResult::Single(dt) | LocalResult::Ambiguous(dt, _) => Some(dt.timestamp()),
        LocalResult::None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{alert_marker, build_expire_info, BillingConfig};

    #[test]
    fn parses_next_due_date_label() {
        let info = build_expire_info("", &BillingConfig::default(), "os=linux;ndd=2099/01/02;spec=1C");

        assert!(info.configured);
        assert_eq!(info.source, "labels.ndd");
        assert_eq!(info.date, "2099-01-02");
        assert_eq!(info.status, "normal");
    }

    #[test]
    fn handles_permanent_expire_marker() {
        let info = build_expire_info("0000-00-00", &BillingConfig::default(), "");

        assert!(info.configured);
        assert_eq!(info.status, "permanent");
        assert!(alert_marker(&info, &[30, 7, 1, 0]).is_none());
    }

    #[test]
    fn auto_renews_past_due_date_by_cycle() {
        let billing = BillingConfig {
            end_date: "2000-01-01".to_string(),
            auto_renewal: "true".to_string(),
            cycle: "Year".to_string(),
            ..Default::default()
        };

        let info = build_expire_info("", &billing, "");

        assert!(info.auto_renewal);
        assert!(info.auto_renewed);
        assert!(info.renewal_count > 0);
        assert_eq!(info.original_date, "2000-01-01");
        assert!(info.days_left >= 0);
    }
}
