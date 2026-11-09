//! Automod: message scanning, raid detection, nuke protection and the rules UI.
//!
//! `scan::handle_message` is called for every guild message. It returns at most
//! one [`Violation`], which is then enforced in one place so punishment,
//! deletion and alerting always stay consistent.

pub mod nuke;
pub mod raid;
pub mod rules;
pub mod scan;

use std::time::{Duration, Instant};

use dashmap::DashMap;
use once_cell::sync::Lazy;
use poise::serenity_prelude as serenity;
use serenity::{GuildId, Message, UserId};

use crate::db::{self, AutomodConfig, Db};
use crate::logs::{self, Log};
use crate::modules::moderation::{self, Action, PunishRequest};
use crate::state::{Data, Error};
use crate::util::{embed, text};

/// What a check found.
pub struct Violation {
    /// Short machine name, e.g. `antispam`.
    pub kind: &'static str,
    /// Human readable line for the alert and the case reason.
    pub detail: String,
    /// What to do to the author. `None` means delete only.
    pub action: Option<Action>,
    /// Timeout length when `action` is `Timeout`.
    pub duration: Option<i64>,
    /// Remove the offending message.
    pub delete: bool,
}

impl Violation {
    pub fn delete_only(kind: &'static str, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
            action: None,
            duration: None,
            delete: true,
        }
    }

    pub fn warn(kind: &'static str, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
            action: Some(Action::Warn),
            duration: None,
            delete: true,
        }
    }

    pub fn timeout(kind: &'static str, detail: impl Into<String>, secs: i64) -> Self {
        Self {
            kind,
            detail: detail.into(),
            action: Some(Action::Timeout),
            duration: Some(secs),
            delete: true,
        }
    }

    pub fn ban(kind: &'static str, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
            action: Some(Action::Ban),
            duration: None,
            delete: true,
        }
    }
}

// ---------------------------------------------------------------------------
// Config cache: automod reads config on every message, so hold it briefly.
// ---------------------------------------------------------------------------

static CONFIG_CACHE: Lazy<DashMap<i64, (AutomodConfig, Instant)>> = Lazy::new(DashMap::new);
const CACHE_TTL: Duration = Duration::from_secs(30);

pub async fn config(db: &Db, guild_id: i64) -> Option<AutomodConfig> {
    if let Some(entry) = CONFIG_CACHE.get(&guild_id) {
        if entry.1.elapsed() < CACHE_TTL {
            return Some(entry.0.clone());
        }
    }
    let fresh = db::automod_config(db, guild_id).await.ok()?;
    CONFIG_CACHE.insert(guild_id, (fresh.clone(), Instant::now()));
    Some(fresh)
}

/// Drop the cached config after a settings change so it takes effect at once.
pub fn invalidate(guild_id: i64) {
    CONFIG_CACHE.remove(&guild_id);
}

// ---------------------------------------------------------------------------
// Exemptions
// ---------------------------------------------------------------------------

/// Staff, exempt roles, exempt channels and exempt users all skip automod.
pub async fn is_exempt(
    sctx: &serenity::Context,
    data: &Data,
    guild_id: GuildId,
    message: &Message,
) -> bool {
    let guild_i64 = guild_id.get() as i64;
    let author = message.author.id;

    if data.is_owner(author.get()) {
        return true;
    }
    if crate::util::perms::guild_owner(sctx, guild_id) == Some(author) {
        return true;
    }

    // Anyone who can manage messages is treated as staff.
    if let Some(member) = &message.member {
        let role_ids: Vec<i64> = member.roles.iter().map(|r| r.get() as i64).collect();
        if let Ok(cfg) = db::guild_config(&data.db, guild_i64).await {
            let staff: Vec<i64> = db::csv_ids(&cfg.mod_roles)
                .into_iter()
                .chain(db::csv_ids(&cfg.admin_roles))
                .chain(db::csv_ids(&cfg.staff_roles))
                .collect();
            if role_ids.iter().any(|r| staff.contains(r)) {
                return true;
            }
        }

        let exempt_roles: Vec<i64> = sqlx::query_scalar(
            "SELECT target FROM automod_exempt WHERE guild_id = ? AND kind = 'role'",
        )
        .bind(guild_i64)
        .fetch_all(&data.db)
        .await
        .unwrap_or_default();
        if role_ids.iter().any(|r| exempt_roles.contains(r)) {
            return true;
        }
    }

    let exempt_rows: Vec<(String, i64)> =
        sqlx::query_as("SELECT kind, target FROM automod_exempt WHERE guild_id = ?")
            .bind(guild_i64)
            .fetch_all(&data.db)
            .await
            .unwrap_or_default();

    for (kind, target) in exempt_rows {
        match kind.as_str() {
            "user" if target == author.get() as i64 => return true,
            "channel" if target == message.channel_id.get() as i64 => return true,
            _ => {}
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Enforcement
// ---------------------------------------------------------------------------

/// Apply a violation: delete the message, punish, alert.
pub async fn enforce(
    sctx: &serenity::Context,
    data: &Data,
    guild_id: GuildId,
    message: &Message,
    violation: Violation,
    cfg: &AutomodConfig,
) -> Result<(), Error> {
    let guild_i64 = guild_id.get() as i64;

    if violation.delete && cfg.delete_violations == 1 {
        let _ = message
            .channel_id
            .delete_message(&sctx.http, message.id, Some("Automod"))
            .await;
    }

    let mut case_line = String::new();
    if let Some(action) = violation.action {
        let req = PunishRequest {
            guild_id,
            target: message.author.id,
            target_tag: message.author.tag(),
            moderator: UserId::new(1),
            moderator_tag: "Automod".to_string(),
            action,
            reason: format!("Automod ({}): {}", violation.kind, violation.detail),
            duration: violation.duration,
            delete_days: 0,
            automated: true,
            silent: false,
        };
        match moderation::punish(sctx, data, req).await {
            Ok(outcome) => {
                case_line = format!("Case **#{}**", outcome.case_no);
                if let Some(extra) = outcome.escalated {
                    case_line.push_str(&format!(" · escalation: {extra}"));
                }
            }
            Err(why) => {
                tracing::warn!("automod punishment failed: {why}");
                case_line = "Punishment failed - check my permissions".to_string();
            }
        }
    }

    // Alert channel gets the detail including the original content.
    let alert = embed::titled(
        format!("🛡️ Automod · {}", violation.kind),
        if violation.action.is_some() {
            embed::WARN
        } else {
            embed::MUTED
        },
    )
    .field("Member", format!("<@{}>", message.author.id.get()), true)
    .field("Channel", format!("<#{}>", message.channel_id.get()), true)
    .field(
        "Action",
        violation
            .action
            .map(|a| a.past_tense().to_string())
            .unwrap_or_else(|| "message deleted".to_string()),
        true,
    )
    .field("Trigger", violation.detail.clone(), false)
    .field(
        "Message",
        if message.content.trim().is_empty() {
            "*(no text)*".to_string()
        } else {
            format!(
                "```\n{}\n```",
                text::truncate(&message.content.replace('`', "'"), 900)
            )
        },
        false,
    )
    .footer(embed::footer(if case_line.is_empty() {
        format!("User ID: {}", message.author.id.get())
    } else {
        case_line
    }));

    if let Some(channel) = cfg.alert_channel {
        let _ = serenity::ChannelId::new(channel as u64)
            .send_message(&sctx.http, serenity::CreateMessage::new().embed(alert))
            .await;
    } else {
        logs::send(sctx, &data.db, guild_i64, Log::Mod, alert).await;
    }

    Ok(())
}

pub fn commands() -> Vec<poise::Command<Data, Error>> {
    rules::commands()
}
