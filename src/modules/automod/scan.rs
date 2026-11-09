//! Per-message automod checks. The first match wins so a member never gets
//! punished twice for one message.

use poise::serenity_prelude as serenity;
use serenity::{GuildId, Message};

use super::{config, enforce, is_exempt, Violation};
use crate::db::AutomodConfig;
use crate::state::{Data, Error};
use crate::util::text;

/// Entry point from the message event handler.
pub async fn handle_message(
    sctx: &serenity::Context,
    data: &Data,
    message: &Message,
) -> Result<(), Error> {
    let Some(guild_id) = message.guild_id else {
        return Ok(());
    };
    if message.author.bot {
        return Ok(());
    }

    let Some(cfg) = config(&data.db, guild_id.get() as i64).await else {
        return Ok(());
    };

    if is_exempt(sctx, data, guild_id, message).await {
        return Ok(());
    }

    if let Some(violation) = run_checks(data, guild_id, message, &cfg).await {
        enforce(sctx, data, guild_id, message, violation, &cfg).await?;
    }
    Ok(())
}

async fn run_checks(
    data: &Data,
    guild_id: GuildId,
    message: &Message,
    cfg: &AutomodConfig,
) -> Option<Violation> {
    let guild_i64 = guild_id.get() as i64;
    let content = &message.content;

    // ---- phishing --------------------------------------------------------
    if cfg.antiphish == 1 {
        let domains = text::find_domains(content);
        if !domains.is_empty() {
            let blocked = {
                let list = data.phish_domains.read();
                domains
                    .iter()
                    .find(|d| list.contains(d.as_str()) || looks_like_phish(d))
                    .cloned()
            };
            if let Some(domain) = blocked {
                return Some(Violation::ban(
                    "antiphish",
                    format!("Known phishing or scam domain: `{domain}`"),
                ));
            }

            // Links outside the allowlist, when the allowlist is in use.
            let allow: Vec<String> =
                sqlx::query_scalar("SELECT domain FROM link_allowlist WHERE guild_id = ?")
                    .bind(guild_i64)
                    .fetch_all(&data.db)
                    .await
                    .unwrap_or_default();
            if !allow.is_empty() {
                if let Some(bad) = domains.iter().find(|d| {
                    !allow
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
    if cfg.badwords_enabled == 1 {
        let words: Vec<String> = sqlx::query_scalar("SELECT word FROM badwords WHERE guild_id = ?")
            .bind(guild_i64)
            .fetch_all(&data.db)
            .await
            .unwrap_or_default();
        if !words.is_empty() {
            let normalised = text::normalize_leet(content);
            if let Some(hit) = words.iter().find(|w| {
                let w = w.to_lowercase();
                text::contains_word(&normalised, &w) || normalised.contains(&w)
            }) {
                return Some(Violation::warn(
                    "badwords",
                    format!("Filtered word: `{}`", text::truncate(hit, 40)),
                ));
            }
        }
    }

    // ---- custom rules ----------------------------------------------------
    if let Some(violation) = custom_rules(data, guild_i64, message).await {
        return Some(violation);
    }

    // ---- duplicate messages ---------------------------------------------
    if cfg.duplicate_enabled == 1 && content.chars().count() > 6 {
        let key = (guild_i64 as u64, message.author.id.get());
        let repeats = {
            let mut entry = data
                .duplicates
                .entry(key)
                .or_insert_with(|| (content.clone(), 0));
            if text::similarity(&entry.0, content) > 0.92 {
                entry.1 += 1;
            } else {
                entry.0 = content.clone();
                entry.1 = 1;
            }
            entry.1
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
        let key = (guild_i64 as u64, message.author.id.get());
        let count = {
            let mut entry = data.spam.entry(key).or_default();
            entry.bump(cfg.antispam_window.max(1) as u64)
        };
        if count as i64 >= cfg.antispam_msgs {
            data.spam.remove(&key);
            return Some(Violation::timeout(
                "antispam",
                format!(
                    "{count} messages in {} seconds",
                    cfg.antispam_window
                ),
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

/// Heuristics for domains that are not on any list yet.
fn looks_like_phish(domain: &str) -> bool {
    const BAIT: [&str; 6] = ["discord", "steam", "nitro", "airdrop", "giveaway", "gift"];
    const REAL: [&str; 7] = [
        "discord.com",
        "discord.gg",
        "discordapp.com",
        "discordstatus.com",
        "steamcommunity.com",
        "steampowered.com",
        "youtube.com",
    ];

    if REAL.iter().any(|r| domain == *r || domain.ends_with(&format!(".{r}"))) {
        return false;
    }
    // Something impersonating a known brand on a domain that is not theirs.
    let bait_hits = BAIT.iter().filter(|b| domain.contains(*b)).count();
    bait_hits >= 2 || (domain.contains("discord") && !domain.ends_with("discord.com"))
}

async fn custom_rules(data: &Data, guild_id: i64, message: &Message) -> Option<Violation> {
    #[derive(sqlx::FromRow)]
    struct Rule {
        name: String,
        trigger_kind: String,
        pattern: String,
        action: String,
        duration: Option<i64>,
    }

    let rules: Vec<Rule> = sqlx::query_as(
        "SELECT name, trigger_kind, pattern, action, duration FROM automod_rules
         WHERE guild_id = ? AND enabled = 1",
    )
    .bind(guild_id)
    .fetch_all(&data.db)
    .await
    .unwrap_or_default();

    let lowered = message.content.to_lowercase();

    for rule in rules {
        let matched = match rule.trigger_kind.as_str() {
            "contains" => lowered.contains(&rule.pattern.to_lowercase()),
            "startswith" => lowered.starts_with(&rule.pattern.to_lowercase()),
            "endswith" => lowered.ends_with(&rule.pattern.to_lowercase()),
            "wordlist" => rule
                .pattern
                .split(',')
                .map(|w| w.trim().to_lowercase())
                .any(|w| !w.is_empty() && text::contains_word(&lowered, &w)),
            "regex" => regex::Regex::new(&rule.pattern)
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
