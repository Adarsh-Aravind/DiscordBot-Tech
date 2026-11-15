//! Per-message automod checks. The first match wins so a member never gets
//! punished twice for one message.
//!
//! Everything these checks read comes from [`super::state`], which is cached,
//! so scanning a message costs no database round trips at all.

use std::sync::Arc;
use std::time::Instant;

use once_cell::sync::Lazy;
use poise::serenity_prelude as serenity;
use regex::{Regex, RegexBuilder};
use serenity::{GuildId, Message};

use super::{AutomodState, Violation};
use crate::state::{Data, Error, LastMessage};
use crate::util::cache::TtlCache;
use crate::util::text;

/// Entry point from the message event handler.
///
/// Returns true when the message (or its author) was removed, so the caller
/// stops handing out XP and running custom commands for it.
pub async fn handle_message(
    sctx: &serenity::Context,
    data: &Data,
    message: &Message,
) -> Result<bool, Error> {
    let Some(guild_id) = message.guild_id else {
        return Ok(false);
    };
    if message.author.bot {
        return Ok(false);
    }

    let Some(state) = super::state(&data.db, guild_id.get() as i64).await else {
        return Ok(false);
    };

    if super::is_exempt(sctx, data, guild_id, message, &state).await {
        return Ok(false);
    }

    if let Some(violation) = run_checks(data, guild_id, message, &state) {
        return super::enforce(sctx, data, guild_id, message, violation, &state.cfg).await;
    }
    Ok(false)
}

fn run_checks(
    data: &Data,
    guild_id: GuildId,
    message: &Message,
    state: &AutomodState,
) -> Option<Violation> {
    let cfg = &state.cfg;
    let content = &message.content;

    // ---- links: phishing first, then the allowlist -----------------------
    let domains = text::find_domains(content);
    if !domains.is_empty() {
        if cfg.antiphish == 1 {
            // A domain on the list is a known scam: ban. The heuristic below
            // works on brand names and will see false positives, so it only
            // ever deletes and alerts - a wrong guess must not ban a member.
            let listed = {
                let list = data.phish_domains.read();
                domains.iter().find(|d| list.contains(d.as_str())).cloned()
            };
            if let Some(domain) = listed {
                return Some(Violation::ban(
                    "antiphish",
                    format!("Known phishing or scam domain: `{domain}`"),
                ));
            }

            if let Some(domain) = domains.iter().find(|d| looks_like_phish(d)) {
                return Some(Violation::delete_only(
                    "antiphish",
                    format!("`{domain}` looks like it is impersonating a known brand"),
                ));
            }
        }

        // The allowlist is a feature of its own: it applies whether or not the
        // phishing checks are switched on.
        if !state.allowlist.is_empty() {
            if let Some(bad) = domains.iter().find(|d| {
                !state
                    .allowlist
                    .iter()
                    .any(|a| d.as_str() == a.as_str() || d.ends_with(&format!(".{a}")))
            }) {
                return Some(Violation::delete_only(
                    "linkfilter",
                    format!("Link to `{bad}` is not on the allowlist"),
                ));
            }
        }
    }

    // ---- discord invites -------------------------------------------------
    if cfg.antiinvite == 1 {
        let invites = text::find_invites(content);
        if !invites.is_empty() {
            return Some(Violation::warn(
                "antiinvite",
                format!("Posted a Discord invite (`{}`)", invites.join("`, `")),
            ));
        }
    }

    // ---- mention spam ----------------------------------------------------
    if cfg.antimention == 1 {
        let mentions = text::count_mentions(content);
        if mentions as i64 > cfg.mention_limit {
            return Some(Violation::timeout(
                "antimention",
                format!(
                    "Mentioned {mentions} users or roles in one message (limit {})",
                    cfg.mention_limit
                ),
                3600,
            ));
        }
    }

    // ---- bad word filter -------------------------------------------------
    if cfg.badwords_enabled == 1 && !state.badwords.is_empty() {
        let normalised = text::normalize_leet(content);
        if let Some(hit) = state.badwords.iter().find(|w| {
            let w = w.to_lowercase();
            // Whole words only, or "ass" would match "classic". A filter entry
            // that is itself a phrase is matched as a substring.
            if w.contains(' ') {
                normalised.contains(&w)
            } else {
                text::contains_word(&normalised, &w)
            }
        }) {
            return Some(Violation::warn(
                "badwords",
                format!("Filtered word: `{}`", text::truncate(hit, 40)),
            ));
        }
    }

    // ---- custom rules ----------------------------------------------------
    if let Some(violation) = custom_rules(message, state) {
        return Some(violation);
    }

    // ---- duplicate messages ---------------------------------------------
    if cfg.duplicate_enabled == 1 && content.chars().count() > 6 {
        let key = (guild_id.get(), message.author.id.get());
        let repeats = {
            let mut entry = data.duplicates.entry(key).or_insert_with(|| LastMessage {
                content: content.clone(),
                repeats: 0,
                at: Instant::now(),
            });
            if text::similarity(&entry.content, content) > 0.92 {
                entry.repeats += 1;
            } else {
                entry.content = content.clone();
                entry.repeats = 1;
            }
            entry.at = Instant::now();
            entry.repeats
        };
        if repeats as i64 >= cfg.duplicate_limit {
            data.duplicates.remove(&key);
            return Some(Violation::timeout(
                "duplicates",
                format!("Sent the same message {repeats} times in a row"),
                600,
            ));
        }
    }

    // ---- shouting --------------------------------------------------------
    if cfg.caps_enabled == 1 {
        let ratio = text::caps_ratio(content);
        if ratio as i64 >= cfg.caps_percent {
            return Some(Violation::delete_only(
                "caps",
                format!("{ratio}% of the message was uppercase"),
            ));
        }
    }

    // ---- anti-spam (per member) -----------------------------------------
    if cfg.antispam == 1 {
        let key = (guild_id.get(), message.author.id.get());
        let count = {
            let mut entry = data.spam.entry(key).or_default();
            entry.bump(cfg.antispam_window.max(1) as u64)
        };
        if count as i64 >= cfg.antispam_msgs {
            data.spam.remove(&key);
            return Some(Violation::timeout(
                "antispam",
                format!("{count} messages in {} seconds", cfg.antispam_window),
                900,
            ));
        }
    }

    // ---- anti-flood (per channel) ---------------------------------------
    if cfg.antiflood == 1 {
        let count = {
            let mut entry = data.flood.entry(message.channel_id.get()).or_default();
            entry.bump(cfg.antiflood_window.max(1) as u64)
        };
        if count as i64 >= cfg.antiflood_msgs {
            data.flood.remove(&message.channel_id.get());
            return Some(Violation {
                kind: "antiflood",
                detail: format!(
                    "Channel took {count} messages in {} seconds - slowmode applied",
                    cfg.antiflood_window
                ),
                action: None,
                duration: None,
                delete: false,
            });
        }
    }

    None
}

// ---------------------------------------------------------------------------
// Brand impersonation heuristics
// ---------------------------------------------------------------------------

/// Domains that legitimately belong to the brands below. A domain that is one
/// of these, or a subdomain of one, is never flagged.
///
/// `discordapp.net` matters more than it looks: `media.discordapp.net` is what
/// Discord itself serves attachments from, so anyone pasting an image link
/// trips a naive "contains discord" rule.
const REAL: &[&str] = &[
    "discord.com",
    "discord.gg",
    "discord.gift",
    "discord.new",
    "discord.media",
    "discord.dev",
    "discord.co",
    "discordapp.com",
    "discordapp.net",
    "discordstatus.com",
    "steamcommunity.com",
    "steampowered.com",
    "steamstatic.com",
    "youtube.com",
    "youtu.be",
    "twitch.tv",
    "roblox.com",
];

/// The registrable labels of those brands, for typo detection.
const PROTECTED_LABELS: &[&str] = &[
    "discord",
    "discordapp",
    "steamcommunity",
    "steampowered",
    "youtube",
    "twitch",
    "roblox",
];

/// Brand words that a scam domain tends to borrow.
const BRANDS: &[&str] = &["discord", "steam", "nitro", "youtube", "twitch", "roblox"];

/// Words that turn a brand mention into a lure.
const LURES: &[&str] = &[
    "gift", "nitro", "free", "airdrop", "giveaway", "claim", "promo", "reward", "bonus", "verify",
    "login", "unlock", "redeem",
];

fn is_real_domain(domain: &str) -> bool {
    REAL.iter()
        .any(|real| domain == *real || domain.ends_with(&format!(".{real}")))
}

/// Edit distance with adjacent transpositions (optimal string alignment), so
/// `discrod` counts as one mistake away from `discord` rather than two.
fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let (n, m) = (a.len(), b.len());
    if n == 0 {
        return m;
    }
    if m == 0 {
        return n;
    }

    let mut grid = vec![vec![0usize; m + 1]; n + 1];
    for (i, row) in grid.iter_mut().enumerate() {
        row[0] = i;
    }
    for j in 0..=m {
        grid[0][j] = j;
    }

    for i in 1..=n {
        for j in 1..=m {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            let mut best = (grid[i - 1][j] + 1)
                .min(grid[i][j - 1] + 1)
                .min(grid[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                best = best.min(grid[i - 2][j - 2] + 1);
            }
            grid[i][j] = best;
        }
    }
    grid[n][m]
}

/// Heuristics for domains that are not on any list yet.
///
/// This only ever triggers a delete-and-alert, never a ban, so it is allowed to
/// be a little eager - but not so eager that ordinary links like
/// `discord.js.org` or `media.discordapp.net` are caught.
pub fn looks_like_phish(domain: &str) -> bool {
    if is_real_domain(domain) {
        return false;
    }

    // A real domain used as a label of someone else's: discord.com.claim-it.ru
    if REAL.iter().any(|real| domain.contains(&format!("{real}."))) {
        return true;
    }

    // A brand plus a lure, ignoring the punctuation scammers hide behind:
    // discord-gift.com, nitro-discord.info, free-steam-keys.xyz
    let compact: String = domain.chars().filter(|c| c.is_alphanumeric()).collect();
    if BRANDS.iter().any(|brand| compact.contains(brand))
        && LURES.iter().any(|lure| compact.contains(lure))
    {
        return true;
    }

    // Typo-squats on the registrable label: dliscord.com, steamcomunnity.com.
    let labels: Vec<&str> = domain.split('.').collect();
    if labels.len() >= 2 {
        let label = labels[labels.len() - 2];
        for protected in PROTECTED_LABELS {
            // The right name on the wrong domain, e.g. discord.net.
            if label == *protected {
                return true;
            }
            // A label that contains the brand name verbatim is a compound, not
            // a typo (youtuber.com, discordpy...), and the lure rule above has
            // already had its say on those.
            if label.contains(protected) {
                continue;
            }
            let allowed = if label.len() >= 8 { 2 } else { 1 };
            if label.len() >= 5 && edit_distance(label, protected) <= allowed {
                return true;
            }
        }
    }

    false
}

// ---------------------------------------------------------------------------
// Custom rules
// ---------------------------------------------------------------------------

/// Compiled rule patterns, keyed by the pattern itself.
///
/// Patterns are validated when the rule is created, so this is purely about not
/// paying for `Regex::new` on every message of every rule.
static REGEX_CACHE: Lazy<TtlCache<String, Option<Arc<Regex>>>> =
    Lazy::new(|| TtlCache::new(3600, 512));

fn compiled(pattern: &str) -> Option<Arc<Regex>> {
    if let Some(hit) = REGEX_CACHE.get(pattern) {
        return hit;
    }
    let built = RegexBuilder::new(pattern)
        // Bound what one rule can cost us, however it was written.
        .size_limit(1 << 20)
        .dfa_size_limit(1 << 20)
        .build()
        .ok()
        .map(Arc::new);
    REGEX_CACHE.put(pattern.to_string(), built.clone());
    built
}

fn custom_rules(message: &Message, state: &AutomodState) -> Option<Violation> {
    if state.rules.is_empty() {
        return None;
    }
    let lowered = message.content.to_lowercase();

    for rule in &state.rules {
        let matched = match rule.trigger_kind.as_str() {
            "contains" => lowered.contains(&rule.pattern.to_lowercase()),
            "startswith" => lowered.starts_with(&rule.pattern.to_lowercase()),
            "endswith" => lowered.ends_with(&rule.pattern.to_lowercase()),
            "wordlist" => rule
                .pattern
                .split(',')
                .map(|w| w.trim().to_lowercase())
                .any(|w| !w.is_empty() && text::contains_word(&lowered, &w)),
            "regex" => compiled(&rule.pattern)
                .map(|re| re.is_match(&message.content))
                .unwrap_or(false),
            "attachment" => !message.attachments.is_empty(),
            "sticker" => !message.sticker_items.is_empty(),
            _ => false,
        };

        if !matched {
            continue;
        }

        let detail = format!("Custom rule **{}** matched", rule.name);
        return Some(match rule.action.as_str() {
            "warn" => Violation::warn("customrule", detail),
            "mute" | "timeout" => {
                Violation::timeout("customrule", detail, rule.duration.unwrap_or(600))
            }
            "kick" => Violation {
                kind: "customrule",
                detail,
                action: Some(crate::modules::moderation::Action::Kick),
                duration: None,
                delete: true,
            },
            "ban" => Violation::ban("customrule", detail),
            "alert" => Violation {
                kind: "customrule",
                detail,
                action: None,
                duration: None,
                delete: false,
            },
            _ => Violation::delete_only("customrule", detail),
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_discord_links_are_never_phishing() {
        for domain in [
            "discord.com",
            "media.discordapp.net",
            "cdn.discordapp.com",
            "images-ext-1.discordapp.net",
            "discord.gg",
            "discord.gift",
            "discordstatus.com",
            "steamcommunity.com",
            "youtube.com",
            "youtu.be",
        ] {
            assert!(!looks_like_phish(domain), "{domain} was flagged");
        }
    }

    #[test]
    fn ordinary_community_links_are_not_phishing() {
        for domain in [
            "discord.js.org",
            "discordpy.readthedocs.io",
            "github.com",
            "stackoverflow.com",
            "twitter.com",
            "youtuber.com",
            "en.wikipedia.org",
        ] {
            assert!(!looks_like_phish(domain), "{domain} was flagged");
        }
    }

    #[test]
    fn impersonation_is_caught() {
        for domain in [
            "discordgift.site",
            "discord-gift.com",
            "discordnitro.info",
            "dliscord.com",
            "discorcl.com",
            "discordapp.io",
            "discord-nitro.org",
            "steamcommunity.ru",
            "steamcomunnity.com",
            "nitro-discord.info",
            "discord-airdrop.com",
            "discrod.gift",
            "discord.com.claim-nitro.ru",
            "discord.net",
        ] {
            assert!(looks_like_phish(domain), "{domain} was missed");
        }
    }

    #[test]
    fn transpositions_count_as_one_mistake() {
        assert_eq!(edit_distance("discrod", "discord"), 1);
        assert_eq!(edit_distance("discord", "discord"), 0);
        assert_eq!(edit_distance("dliscord", "discord"), 1);
    }
}
