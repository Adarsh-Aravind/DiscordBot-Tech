//! String helpers shared by automod, welcome messages, leveling and the utility commands.

use once_cell::sync::Lazy;
use regex::Regex;

pub static INVITE_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)(?:discord(?:app)?\.com/invite|discord\.gg|discord\.me|dsc\.gg|invite\.gg|discord\.io)/([a-z0-9\-_]+)")
        .expect("invite regex")
});

pub static URL_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)\bhttps?://([a-z0-9\-._~%]+)(?:[:/][^\s]*)?").expect("url regex")
});

pub static MENTION_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"<@!?(\d+)>").expect("mention regex"));

pub static EMOJI_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"<a?:([a-zA-Z0-9_]+):(\d+)>").expect("emoji regex"));

/// Cut a string to `max` characters, appending an ellipsis when it was cut.
pub fn truncate(input: &str, max: usize) -> String {
    if input.chars().count() <= max {
        return input.to_string();
    }
    let kept: String = input.chars().take(max.saturating_sub(1)).collect();
    format!("{kept}…")
}

/// Escape the markdown control characters so user content cannot break an embed.
pub fn escape_md(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        if matches!(ch, '*' | '_' | '~' | '`' | '>' | '|' | '\\' | '[' | ']') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// Substitute `{placeholder}` tokens. Unknown tokens are left untouched.
pub fn render(template: &str, vars: &[(&str, String)]) -> String {
    let mut out = template.to_string();
    for (key, value) in vars {
        out = out.replace(&format!("{{{key}}}"), value);
    }
    out
}

/// `████████░░ 80%` style bar for rank cards and progress embeds.
pub fn progress_bar(current: i64, total: i64, width: usize) -> String {
    if total <= 0 {
        return "░".repeat(width);
    }
    let ratio = (current as f64 / total as f64).clamp(0.0, 1.0);
    let filled = (ratio * width as f64).round() as usize;
    let mut bar = String::new();
    for i in 0..width {
        bar.push(if i < filled { '█' } else { '░' });
    }
    bar
}

pub fn count_mentions(content: &str) -> usize {
    let unique: std::collections::HashSet<&str> = MENTION_RE
        .captures_iter(content)
        .filter_map(|c| c.get(1).map(|m| m.as_str()))
        .collect();
    let mut total = unique.len();
    if content.contains("@everyone") {
        total += 1;
    }
    if content.contains("@here") {
        total += 1;
    }
    total
}

/// Share of alphabetic characters that are uppercase, 0-100.
pub fn caps_ratio(content: &str) -> u32 {
    let letters: Vec<char> = content.chars().filter(|c| c.is_alphabetic()).collect();
    if letters.len() < 8 {
        return 0;
    }
    let upper = letters.iter().filter(|c| c.is_uppercase()).count();
    ((upper as f64 / letters.len() as f64) * 100.0).round() as u32
}

pub fn find_invites(content: &str) -> Vec<String> {
    INVITE_RE
        .captures_iter(content)
        .filter_map(|c| c.get(1).map(|m| m.as_str().to_string()))
        .collect()
}

/// Hostnames of every link in the message, lowercased and without `www.`.
pub fn find_domains(content: &str) -> Vec<String> {
    URL_RE
        .captures_iter(content)
        .filter_map(|c| c.get(1))
        .map(|m| {
            let host = m.as_str().to_lowercase();
            host.trim_start_matches("www.").to_string()
        })
        .collect()
}

/// Fold common letter substitutions so `fr33` matches `free` in the word filter.
pub fn normalize_leet(input: &str) -> String {
    input
        .to_lowercase()
        .chars()
        .map(|c| match c {
            '0' => 'o',
            '1' | '!' | '|' => 'i',
            '3' => 'e',
            '4' | '@' => 'a',
            '5' | '$' => 's',
            '7' => 't',
            '8' => 'b',
            '9' => 'g',
            other => other,
        })
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect()
}

/// True when `needle` appears in `haystack` as a whole word.
pub fn contains_word(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    haystack
        .split(|c: char| !c.is_alphanumeric())
        .any(|word| word == needle)
}

/// Rough similarity (0.0-1.0) used by duplicate-message detection.
pub fn similarity(a: &str, b: &str) -> f64 {
    if a == b {
        return 1.0;
    }
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let a_chars: Vec<char> = a.to_lowercase().chars().collect();
    let b_chars: Vec<char> = b.to_lowercase().chars().collect();
    let max_len = a_chars.len().max(b_chars.len());
    let matching = a_chars
        .iter()
        .zip(b_chars.iter())
        .filter(|(x, y)| x == y)
        .count();
    matching as f64 / max_len as f64
}

/// Human readable byte size.
pub fn bytes(size: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = size as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{size} B")
    } else {
        format!("{value:.2} {}", UNITS[unit])
    }
}

/// Split a long body into Discord-safe chunks on line boundaries.
pub fn chunk(body: &str, max: usize) -> Vec<String> {
    let mut pages = Vec::new();
    let mut current = String::new();
    for line in body.lines() {
        if current.chars().count() + line.chars().count() + 1 > max && !current.is_empty() {
            pages.push(std::mem::take(&mut current));
        }
        current.push_str(line);
        current.push('\n');
    }
    if !current.trim().is_empty() {
        pages.push(current);
    }
    if pages.is_empty() {
        pages.push("*(nothing to show)*".to_string());
    }
    pages
}

/// Digits only, for the counting game.
pub fn parse_leading_number(content: &str) -> Option<i64> {
    let token = content.split_whitespace().next()?;
    token.trim().parse::<i64>().ok()
}
