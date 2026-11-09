//! Duration parsing, humanising and the single timestamp format used in SQLite.

use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};

pub const SQL_FMT: &str = "%Y-%m-%d %H:%M:%S";

/// The format SQLite's `datetime('now')` produces, so Rust and SQL agree.
pub fn sql_now() -> String {
    Utc::now().format(SQL_FMT).to_string()
}

pub fn to_sql(ts: DateTime<Utc>) -> String {
    ts.format(SQL_FMT).to_string()
}

pub fn from_sql(raw: &str) -> Option<DateTime<Utc>> {
    let raw = raw.trim();
    // Accept both "2026-01-01 12:00:00" and RFC3339, whichever wrote the row.
    if let Ok(ndt) = NaiveDateTime::parse_from_str(raw, SQL_FMT) {
        return Some(Utc.from_utc_datetime(&ndt));
    }
    if let Ok(ndt) = NaiveDateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M:%S") {
        return Some(Utc.from_utc_datetime(&ndt));
    }
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

/// Parse a compact duration such as `10s`, `15m`, `2h30m`, `3d`, `1w`.
/// A bare number is read as minutes.
pub fn parse_duration(input: &str) -> Option<std::time::Duration> {
    let s = input.trim().to_lowercase();
    if s.is_empty() {
        return None;
    }
    if let Ok(mins) = s.parse::<u64>() {
        return Some(std::time::Duration::from_secs(mins * 60));
    }

    let mut total: u64 = 0;
    let mut number = String::new();
    let mut matched_any = false;

    for ch in s.chars() {
        if ch.is_ascii_digit() {
            number.push(ch);
            continue;
        }
        if ch.is_whitespace() {
            continue;
        }
        let value: u64 = match number.parse() {
            Ok(v) => v,
            Err(_) => return None,
        };
        number.clear();
        let secs = match ch {
            's' => 1,
            'm' => 60,
            'h' => 3_600,
            'd' => 86_400,
            'w' => 604_800,
            'y' => 31_536_000,
            _ => return None,
        };
        total = total.checked_add(value.checked_mul(secs)?)?;
        matched_any = true;
    }

    if !matched_any || total == 0 {
        return None;
    }
    Some(std::time::Duration::from_secs(total))
}

/// `93784` -> `1d 2h 3m 4s`
pub fn humanize(total_secs: i64) -> String {
    if total_secs <= 0 {
        return "0s".to_string();
    }
    let secs = total_secs as u64;
    let days = secs / 86_400;
    let hours = (secs % 86_400) / 3_600;
    let minutes = (secs % 3_600) / 60;
    let seconds = secs % 60;

    let mut parts = Vec::new();
    if days > 0 {
        parts.push(format!("{days}d"));
    }
    if hours > 0 {
        parts.push(format!("{hours}h"));
    }
    if minutes > 0 {
        parts.push(format!("{minutes}m"));
    }
    if seconds > 0 && days == 0 {
        parts.push(format!("{seconds}s"));
    }
    if parts.is_empty() {
        parts.push("0s".to_string());
    }
    parts.join(" ")
}

/// Long form for uptime: `3 days, 4 hours, 12 minutes`.
pub fn humanize_long(total_secs: i64) -> String {
    let secs = total_secs.max(0) as u64;
    let days = secs / 86_400;
    let hours = (secs % 86_400) / 3_600;
    let minutes = (secs % 3_600) / 60;
    let seconds = secs % 60;
    let mut parts = Vec::new();
    if days > 0 {
        parts.push(format!("{days} day{}", plural(days)));
    }
    if hours > 0 {
        parts.push(format!("{hours} hour{}", plural(hours)));
    }
    if minutes > 0 {
        parts.push(format!("{minutes} minute{}", plural(minutes)));
    }
    if parts.is_empty() {
        parts.push(format!("{seconds} second{}", plural(seconds)));
    }
    parts.join(", ")
}

fn plural(n: u64) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

/// `<t:1760000000:F>` - rendered in the reader's own timezone by Discord.
pub fn discord_ts(ts: DateTime<Utc>, style: char) -> String {
    format!("<t:{}:{}>", ts.timestamp(), style)
}

/// Absolute timestamp plus a relative one, which is what most embeds want.
pub fn full_and_relative(ts: DateTime<Utc>) -> String {
    format!("{} ({})", discord_ts(ts, 'F'), discord_ts(ts, 'R'))
}

/// Accept either a duration (`2h`) or an absolute `YYYY-MM-DD HH:MM` / `YYYY-MM-DD`.
pub fn parse_when(input: &str) -> Option<DateTime<Utc>> {
    if let Some(d) = parse_duration(input) {
        return Some(Utc::now() + chrono::Duration::seconds(d.as_secs() as i64));
    }
    let raw = input.trim();
    if let Ok(ndt) = NaiveDateTime::parse_from_str(raw, "%Y-%m-%d %H:%M:%S") {
        return Some(Utc.from_utc_datetime(&ndt));
    }
    if let Ok(ndt) = NaiveDateTime::parse_from_str(raw, "%Y-%m-%d %H:%M") {
        return Some(Utc.from_utc_datetime(&ndt));
    }
    if let Ok(date) = chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d") {
        let ndt = date.and_hms_opt(12, 0, 0)?;
        return Some(Utc.from_utc_datetime(&ndt));
    }
    None
}

pub fn today() -> String {
    Utc::now().format("%Y-%m-%d").to_string()
}

pub fn day_offset(days: i64) -> String {
    (Utc::now() - chrono::Duration::days(days))
        .format("%Y-%m-%d")
        .to_string()
}
