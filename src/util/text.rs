//! String helpers shared by automod, welcome messages, leveling and the utility commands.

use once_cell::sync::Lazy;
use regex::Regex;

pub static INVITE_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)(?:discord(?:app)?\.com/invite|discord\.gg|discord\.me|dsc\.gg|invite\.gg|discord\.io)/([a-z0-9\-_]+)")
        .expect("invite regex")
});

/// Captures the whole authority of a URL - everything between `://` and the
/// path, query or fragment - so [`find_domains`] can work out which part of it
/// is really the host.
pub static URL_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)\bhttps?://([^\s/?#]+)").expect("url regex"));

pub static MENTION_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"<@!?(\d+)>").expect("mention regex"));

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
        .filter_map(|m| host_of(m.as_str()))
        .collect()
}

/// The host out of a URL authority.
///
/// `https://discord.com@evil.site/free-nitro` has an authority of
/// `discord.com@evil.site`, and a browser goes to `evil.site` - everything
/// before the last `@` is a username. Reading the host as "the text right after
/// `https://`" meant a scam link could name any domain it liked and sail past
/// both the phishing list and the link allowlist.
fn host_of(authority: &str) -> Option<String> {
    // Userinfo can itself contain '@', so the host is after the *last* one.
    let after_userinfo = authority.rsplit('@').next()?;

    // Strip the port, and the brackets around an IPv6 literal.
    let host = if let Some(rest) = after_userinfo.strip_prefix('[') {
        rest.split(']').next()?
    } else {
        after_userinfo.split(':').next()?
    };

    let host = host.trim_end_matches('.').to_lowercase();
    if host.is_empty() {
        return None;
    }
    let host = host.strip_prefix("www.").unwrap_or(&host).to_string();
    if host.is_empty() {
        None
    } else {
        Some(host)
    }
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

/// Split a long body into Discord-safe chunks on line boundaries.
///
/// A single line longer than `max` is broken up as well. Passing it through
/// whole produced a "page" over the limit, which Discord then rejected - the
/// one case where chunking mattered most.
pub fn chunk(body: &str, max: usize) -> Vec<String> {
    let max = max.max(1);
    let mut pages = Vec::new();
    let mut current = String::new();

    for line in body.lines() {
        for piece in split_long_line(line, max) {
            if current.chars().count() + piece.chars().count() + 1 > max && !current.is_empty() {
                pages.push(std::mem::take(&mut current));
            }
            current.push_str(&piece);
            current.push('\n');
        }
    }

    if !current.trim().is_empty() {
        pages.push(current);
    }
    if pages.is_empty() {
        pages.push("*(nothing to show)*".to_string());
    }
    pages
}

/// Break one over-long line into pieces of at most `max` characters, on a word
/// boundary where there is one.
fn split_long_line(line: &str, max: usize) -> Vec<String> {
    if line.chars().count() <= max {
        return vec![line.to_string()];
    }

    let mut pieces = Vec::new();
    let mut current = String::new();

    for word in line.split_inclusive(char::is_whitespace) {
        // A single word longer than the whole limit has to be cut mid-word.
        if word.chars().count() > max {
            if !current.is_empty() {
                pieces.push(std::mem::take(&mut current));
            }
            let mut piece = String::new();
            for ch in word.chars() {
                if piece.chars().count() == max {
                    pieces.push(std::mem::take(&mut piece));
                }
                piece.push(ch);
            }
            current = piece;
            continue;
        }

        if current.chars().count() + word.chars().count() > max && !current.is_empty() {
            pieces.push(std::mem::take(&mut current));
        }
        current.push_str(word);
    }

    if !current.is_empty() {
        pieces.push(current);
    }
    pieces
}

/// Digits only, for the counting game.
pub fn parse_leading_number(content: &str) -> Option<i64> {
    let token = content.split_whitespace().next()?;
    token.trim().parse::<i64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_host_not_the_username() {
        // The classic trick: the part before '@' is a username, so this link
        // actually goes to evil.site.
        assert_eq!(
            find_domains("look https://discord.com@evil.site/free-nitro"),
            vec!["evil.site".to_string()]
        );
        assert_eq!(
            find_domains("https://a@b@steamcommunity.com.scam.ru/x"),
            vec!["steamcommunity.com.scam.ru".to_string()]
        );
    }

    #[test]
    fn reads_ordinary_links() {
        assert_eq!(
            find_domains("see https://www.Example.COM/path?x=1 and http://media.discordapp.net/a.png"),
            vec!["example.com".to_string(), "media.discordapp.net".to_string()]
        );
        assert_eq!(
            find_domains("https://example.com:8443/x"),
            vec!["example.com".to_string()]
        );
        assert_eq!(
            find_domains("https://[2606:4700::1111]/x"),
            vec!["2606:4700::1111".to_string()]
        );
        assert!(find_domains("no links here").is_empty());
    }

    #[test]
    fn finds_invites() {
        assert_eq!(find_invites("join discord.gg/abc123"), vec!["abc123"]);
        assert!(find_invites("nothing").is_empty());
    }

    #[test]
    fn chunks_stay_within_the_limit() {
        let body = (0..50)
            .map(|i| format!("line number {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        for page in chunk(&body, 60) {
            assert!(page.chars().count() <= 61, "page too long: {page:?}");
        }
    }

    #[test]
    fn a_single_long_line_is_split() {
        let body = "x".repeat(500);
        let pages = chunk(&body, 100);
        assert!(pages.len() >= 5, "got {} pages", pages.len());
        for page in &pages {
            assert!(page.chars().count() <= 101, "page too long: {}", page.len());
        }
        // Nothing is lost in the splitting.
        let rebuilt: String = pages.iter().map(|p| p.trim_end_matches('\n')).collect();
        assert_eq!(rebuilt, body);
    }

    #[test]
    fn long_lines_break_on_words_when_they_can() {
        let body = "alpha beta gamma delta epsilon zeta eta theta";
        let pages = chunk(body, 20);
        for page in &pages {
            assert!(page.chars().count() <= 21);
        }
        assert!(pages[0].starts_with("alpha"));
    }

    #[test]
    fn empty_body_still_says_something() {
        assert_eq!(chunk("", 100).len(), 1);
    }

    #[test]
    fn whole_words_only() {
        assert!(contains_word("that is classic", "classic"));
        assert!(!contains_word("that is classic", "ass"));
        assert!(contains_word("an ass of a word", "ass"));
    }

    #[test]
    fn truncate_counts_characters() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello", 3).chars().count(), 3);
        // Multi-byte input must not be cut mid-character.
        let long = "é".repeat(50);
        assert_eq!(truncate(&long, 10).chars().count(), 10);
    }

    #[test]
    fn caps_ratio_ignores_short_messages() {
        assert_eq!(caps_ratio("HI"), 0);
        assert_eq!(caps_ratio("SHOUTING LOUDLY"), 100);
        assert_eq!(caps_ratio("quiet enough here"), 0);
    }

    #[test]
    fn counts_unique_mentions() {
        assert_eq!(count_mentions("<@1> <@1> <@2>"), 2);
        assert_eq!(count_mentions("hey @everyone"), 1);
        assert_eq!(count_mentions("nobody"), 0);
    }
}
