//! Anti-nuke: watch the audit log for bursts of destructive admin actions and
//! quarantine whoever is responsible.
//!
//! The audit-log action enum changes shape between serenity releases, so the
//! caller hands us a label (its `Debug` form) and we classify on that. It keeps
//! this module stable across library bumps and the labels are what we want to
//! show in the alert anyway.

use poise::serenity_prelude as serenity;
use serenity::{EditMember, GuildId, UserId};

use super::config;
use crate::logs::{self, Log};
use crate::modules::moderation::{self, Action, PunishRequest};
use crate::state::Data;
use crate::util::embed;

/// Audit-log actions that can wreck a server when done in bulk.
///
/// This is deliberately an explicit list. It used to end with "anything whose
/// label contains Delete", which swept up routine work - deleting old invites,
/// tidying up emoji, removing a stale automod rule - and four of those inside
/// the window was enough to strip a moderator of every role they had.
const DESTRUCTIVE: &[&str] = &[
    "ChannelDelete",
    "Channel(Delete)",
    "RoleDelete",
    "Role(Delete)",
    "MemberBanAdd",
    "Member(BanAdd)",
    "MemberKick",
    "Member(Kick)",
    "MemberPrune",
    "Member(Prune)",
    "WebhookCreate",
    "Webhook(Create)",
    "EmojiDelete",
    "Emoji(Delete)",
    "StickerDelete",
    "Sticker(Delete)",
    "IntegrationCreate",
    "Integration(Create)",
    "ChannelOverwriteDelete",
    "Channel(OverwriteDelete)",
];

fn is_destructive(label: &str) -> bool {
    DESTRUCTIVE.iter().any(|needle| label.contains(needle))
}

/// Called for each audit log entry. Returns true when the actor was quarantined.
pub async fn handle_audit_entry(
    sctx: &serenity::Context,
    data: &Data,
    guild_id: GuildId,
    actor: UserId,
    action_label: &str,
    target: Option<String>,
) -> bool {
    let guild_i64 = guild_id.get() as i64;

    let Some(cfg) = config(&data.db, guild_i64).await else {
        return false;
    };
    if cfg.antinuke == 0 {
        return false;
    }

    // Never act on ourselves or the server owner.
    if actor == crate::util::perms::bot_id(sctx) {
        return false;
    }
    if crate::util::perms::guild_owner(sctx, guild_id) == Some(actor) {
        return false;
    }
    if data.is_owner(actor.get()) {
        return false;
    }
    if !is_destructive(action_label) {
        return false;
    }

    // Record it for the report, and count it in the sliding window.
    let _ = sqlx::query(
        "INSERT INTO antinuke_log (guild_id, actor_id, action, target, created_at)
         VALUES (?, ?, ?, ?, datetime('now'))",
    )
    .bind(guild_i64)
    .bind(actor.get() as i64)
    .bind(action_label)
    .bind(target.clone().unwrap_or_default())
    .execute(&data.db)
    .await;

    let count = {
        let mut entry = data.nuke.entry((guild_i64 as u64, actor.get())).or_default();
        entry.bump(cfg.antinuke_window.max(5) as u64)
    };

    if (count as i64) < cfg.antinuke_threshold {
        return false;
    }

    data.nuke.remove(&(guild_i64 as u64, actor.get()));
    quarantine(sctx, data, guild_id, actor, count, &cfg.antinuke_window).await;
    true
}

async fn quarantine(
    sctx: &serenity::Context,
    data: &Data,
    guild_id: GuildId,
    actor: UserId,
    count: usize,
    window: &i64,
) {
    let guild_i64 = guild_id.get() as i64;

    // Step zero: write down what they had. Stripping the roles is the whole
    // point, but the alert asks staff to put them back, and nothing knew what
    // "back" was.
    let previous_roles: Vec<serenity::RoleId> = match guild_id
        .to_guild_cached(&sctx.cache)
        .and_then(|guild| guild.members.get(&actor).map(|member| member.roles.clone()))
    {
        Some(roles) => roles,
        None => guild_id
            .member(sctx, actor)
            .await
            .map(|member| member.roles.clone())
            .unwrap_or_default(),
    };

    if !previous_roles.is_empty() {
        let csv = previous_roles
            .iter()
            .map(|role| role.get().to_string())
            .collect::<Vec<_>>()
            .join(",");
        let _ = crate::db::kv_set_with_ttl(
            &data.db,
            guild_i64,
            &format!("antinuke.roles.{}", actor.get()),
            &csv,
            30 * 86_400,
        )
        .await;
    }

    // Step one: take every role off them so the damage stops immediately.
    let stripped = guild_id
        .edit_member(
            &sctx.http,
            actor,
            EditMember::new()
                .roles(Vec::<serenity::RoleId>::new())
                .audit_log_reason("Anti-nuke: quarantined"),
        )
        .await
        .is_ok();

    // Step two: record a case. Ban is deliberately not automatic - stripping
    // roles already stops the bleeding, and a false positive that bans a
    // senior admin is worse than one that needs a staff decision.
    let mut req = PunishRequest::new(
        guild_id,
        actor,
        Action::Timeout,
        format!(
            "Anti-nuke: {count} destructive actions within {window}s. Roles removed pending review."
        ),
    );
    req.duration = Some(24 * 3600);
    req.moderator_tag = "Anti-nuke".to_string();
    let case = moderation::punish(sctx, data, req)
        .await
        .map(|o| o.case_no)
        .unwrap_or(0);

    // Step three: shout about it in every log we have.
    let recent: Vec<(String, String)> = sqlx::query_as(
        "SELECT action, target FROM antinuke_log
         WHERE guild_id = ? AND actor_id = ? ORDER BY id DESC LIMIT 10",
    )
    .bind(guild_i64)
    .bind(actor.get() as i64)
    .fetch_all(&data.db)
    .await
    .unwrap_or_default();

    let trail = if recent.is_empty() {
        "-".to_string()
    } else {
        recent
            .iter()
            .map(|(action, target)| {
                if target.is_empty() {
                    format!("• {action}")
                } else {
                    format!("• {action} → {target}")
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    let roles_line = if previous_roles.is_empty() {
        "*(none recorded)*".to_string()
    } else {
        crate::util::text::truncate(
            &previous_roles
                .iter()
                .map(|role| format!("<@&{}>", role.get()))
                .collect::<Vec<_>>()
                .join(" "),
            900,
        )
    };

    let alert = embed::titled("🧨 ANTI-NUKE TRIGGERED", embed::DANGER)
        .description(format!(
            "<@{}> performed **{count}** destructive actions in **{window}s**.\n\n\
             • All of their roles have been removed{}\n\
             • They are timed out for 24 hours (case #{case})\n\n\
             Review and ban them, or give the roles below back with `/role add`.",
            actor.get(),
            if stripped {
                ""
            } else {
                " *(failed - my role may be below theirs)*"
            }
        ))
        .field("Roles they had", roles_line, false)
        .field("Recent actions", trail, false)
        .footer(embed::footer(format!("Actor ID: {}", actor.get())));

    if let Ok(cfg) = crate::db::automod_config(&data.db, guild_i64).await {
        if let Some(channel) = cfg.alert_channel {
            let _ = serenity::ChannelId::new(channel as u64)
                .send_message(&sctx.http, serenity::CreateMessage::new().embed(alert.clone()))
                .await;
        }
    }
    logs::send(sctx, &data.db, guild_i64, Log::Server, alert.clone()).await;

    // DM the owner: if this is a real nuke the log channels may be gone too.
    if let Some(owner) = crate::util::perms::guild_owner(sctx, guild_id) {
        if let Ok(channel) = owner.create_dm_channel(sctx).await {
            let _ = channel
                .send_message(&sctx.http, serenity::CreateMessage::new().embed(alert))
                .await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// These all used to count, because the old rule was "the label contains
    /// Delete". Four of them in half a minute quarantined a moderator.
    #[test]
    fn routine_work_is_not_a_nuke() {
        for label in [
            "InviteDelete",
            "Invite(Delete)",
            "MessageDelete",
            "MessageBulkDelete",
            "AutoModRuleDelete",
            "ScheduledEventDelete",
            "ThreadDelete",
            "GuildUpdate",
            "MemberRoleUpdate",
            "ChannelUpdate",
        ] {
            assert!(!is_destructive(label), "{label} counted as destructive");
        }
    }

    #[test]
    fn mass_destruction_still_counts() {
        for label in [
            "ChannelDelete",
            "Channel(Delete)",
            "RoleDelete",
            "Role(Delete)",
            "MemberBanAdd",
            "Member(BanAdd)",
            "MemberKick",
            "WebhookCreate",
            "EmojiDelete",
        ] {
            assert!(is_destructive(label), "{label} was missed");
        }
    }
}
