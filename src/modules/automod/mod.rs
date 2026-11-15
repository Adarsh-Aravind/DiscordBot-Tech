//! Automod: message scanning, raid detection, nuke protection and the rules UI.
//!
//! `scan::handle_message` is called for every guild message. It finds at most
//! one [`Violation`], which is then enforced in one place so punishment,
//! deletion and alerting always stay consistent, and reports back whether the
//! message survived.

pub mod nuke;
pub mod raid;
pub mod rules;
pub mod scan;

use std::sync::Arc;

use once_cell::sync::Lazy;
use poise::serenity_prelude as serenity;
use serenity::{GuildId, Message, RoleId, UserId};

use crate::db::{self, AutomodConfig, Db};
use crate::logs::{self, Log};
use crate::modules::moderation::{self, Action, PunishRequest};
use crate::state::{Data, Error};
use crate::util::cache::TtlCache;
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
// Per-guild state cache
// ---------------------------------------------------------------------------

/// One row of `automod_rules`, as the scanner needs it.
#[derive(Clone, Debug, sqlx::FromRow)]
pub struct Rule {
    pub name: String,
    pub trigger_kind: String,
    pub pattern: String,
    pub action: String,
    pub duration: Option<i64>,
}

/// Everything automod needs in order to judge one message.
///
/// Reading this used to be six separate queries per message - settings, bad
/// words, custom rules, two exemption lookups and the link allowlist. It is now
/// one bundle behind a short TTL, dropped by [`invalidate`] whenever any automod
/// command changes something, so edits still take effect immediately.
#[derive(Clone, Debug)]
pub struct AutomodState {
    pub cfg: AutomodConfig,
    pub badwords: Vec<String>,
    pub rules: Vec<Rule>,
    pub exempt_roles: Vec<i64>,
    pub exempt_users: Vec<i64>,
    pub exempt_channels: Vec<i64>,
    pub allowlist: Vec<String>,
}

static STATE_CACHE: Lazy<TtlCache<i64, Arc<AutomodState>>> = Lazy::new(|| TtlCache::new(30, 2048));

pub async fn state(db: &Db, guild_id: i64) -> Option<Arc<AutomodState>> {
    if let Some(hit) = STATE_CACHE.get(&guild_id) {
        return Some(hit);
    }

    let cfg = db::automod_config(db, guild_id).await.ok()?;

    let badwords: Vec<String> = sqlx::query_scalar("SELECT word FROM badwords WHERE guild_id = ?")
        .bind(guild_id)
        .fetch_all(db)
        .await
        .unwrap_or_default();

    let rules: Vec<Rule> = sqlx::query_as(
        "SELECT name, trigger_kind, pattern, action, duration FROM automod_rules
         WHERE guild_id = ? AND enabled = 1 ORDER BY id",
    )
    .bind(guild_id)
    .fetch_all(db)
    .await
    .unwrap_or_default();

    let exemptions: Vec<(String, i64)> =
        sqlx::query_as("SELECT kind, target FROM automod_exempt WHERE guild_id = ?")
            .bind(guild_id)
            .fetch_all(db)
            .await
            .unwrap_or_default();

    let allowlist: Vec<String> =
        sqlx::query_scalar("SELECT domain FROM link_allowlist WHERE guild_id = ?")
            .bind(guild_id)
            .fetch_all(db)
            .await
            .unwrap_or_default();

    let mut exempt_roles = Vec::new();
    let mut exempt_users = Vec::new();
    let mut exempt_channels = Vec::new();
    for (kind, target) in exemptions {
        match kind.as_str() {
            "role" => exempt_roles.push(target),
            "user" => exempt_users.push(target),
            "channel" => exempt_channels.push(target),
            _ => {}
        }
    }

    let state = Arc::new(AutomodState {
        cfg,
        badwords,
        rules,
        exempt_roles,
        exempt_users,
        exempt_channels,
        allowlist,
    });
    STATE_CACHE.put(guild_id, Arc::clone(&state));
    Some(state)
}

/// Just the settings, for the callers that do not scan messages.
pub async fn config(db: &Db, guild_id: i64) -> Option<AutomodConfig> {
    state(db, guild_id).await.map(|s| s.cfg.clone())
}

/// Drop the cached state after a settings change so it takes effect at once.
pub fn invalidate(guild_id: i64) {
    STATE_CACHE.invalidate(&guild_id);
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
    state: &AutomodState,
) -> bool {
    let guild_i64 = guild_id.get() as i64;
    let author = message.author.id;

    if data.is_owner(author.get()) {
        return true;
    }
    if crate::util::perms::guild_owner(sctx, guild_id) == Some(author) {
        return true;
    }
    if state.exempt_users.contains(&(author.get() as i64)) {
        return true;
    }
    if state
        .exempt_channels
        .contains(&(message.channel_id.get() as i64))
    {
        return true;
    }

    let roles: Vec<RoleId> = message
        .member
        .as_ref()
        .map(|m| m.roles.clone())
        .unwrap_or_default();
    let role_ids: Vec<i64> = roles.iter().map(|r| r.get() as i64).collect();

    if role_ids.iter().any(|r| state.exempt_roles.contains(r)) {
        return true;
    }

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

    // A moderator who holds the Discord permission but was never given one of
    // the configured roles is still staff. Without this, automod punishes the
    // people who are meant to be running it.
    crate::util::perms::roles_are_staff(sctx, guild_id, &roles)
}

// ---------------------------------------------------------------------------
// Enforcement
// ---------------------------------------------------------------------------

/// Apply a violation: delete the message, punish, alert.
///
/// Returns true when the message (or its author) is gone, so the rest of the
/// message pipeline - XP, custom commands, games - can stop rather than reward
/// someone for a message automod just removed.
pub async fn enforce(
    sctx: &serenity::Context,
    data: &Data,
    guild_id: GuildId,
    message: &Message,
    violation: Violation,
    cfg: &AutomodConfig,
) -> Result<bool, Error> {
    let guild_i64 = guild_id.get() as i64;
    let mut removed = false;

    if violation.delete && cfg.delete_violations == 1 {
        removed = crate::compat::delete_message(&sctx.http, message.channel_id, message.id, Some("Automod"))
            .await
            .is_ok();
    }

    // Anything that takes the member out of the server counts as removed too.
    if matches!(
        violation.action,
        Some(Action::Kick) | Some(Action::Ban) | Some(Action::Softban)
    ) {
        removed = true;
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

    Ok(removed)
}

pub fn commands() -> Vec<poise::Command<Data, Error>> {
    rules::commands()
}
