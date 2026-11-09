//! Anti-raid and suspicious-account handling, driven by member joins.

use chrono::Utc;
use poise::serenity_prelude as serenity;
use serenity::{GuildId, Member, UserId};

use super::config;
use crate::logs::{self, Log};
use crate::modules::moderation::{self, lockdown, Action, PunishRequest};
use crate::state::{Data, Error};
use crate::util::{embed, time};

/// How long raid mode stays on after the last burst.
const RAID_MODE_MINUTES: i64 = 15;

pub struct JoinVerdict {
    /// The member was removed, so the welcome flow should not run.
    pub blocked: bool,
    /// A note for the join log.
    pub note: Option<String>,
}

pub async fn handle_join(
    sctx: &serenity::Context,
    data: &Data,
    member: &Member,
) -> Result<JoinVerdict, Error> {
    let guild_id = member.guild_id;
    let guild_i64 = guild_id.get() as i64;

    let Some(cfg) = config(&data.db, guild_i64).await else {
        return Ok(JoinVerdict {
            blocked: false,
            note: None,
        });
    };

    let mut note = None;

    // ---- raid burst detection -------------------------------------------
    if cfg.antiraid == 1 {
        let count = {
            let mut entry = data.joins.entry(guild_id.get()).or_default();
            entry.bump(cfg.antiraid_window.max(1) as u64)
        };

        let already_in_raid = data
            .raid_mode
            .get(&guild_id.get())
            .map(|until| *until > Utc::now())
            .unwrap_or(false);

        if count as i64 >= cfg.antiraid_joins && !already_in_raid {
            data.raid_mode.insert(
                guild_id.get(),
                Utc::now() + chrono::Duration::minutes(RAID_MODE_MINUTES),
            );
            trigger_raid_response(sctx, data, guild_id, count, &cfg.antiraid_action).await;
        }

        if already_in_raid {
            note = Some("joined during raid mode".to_string());
            match cfg.antiraid_action.as_str() {
                "kick" | "ban" => {
                    let action = if cfg.antiraid_action == "ban" {
                        Action::Ban
                    } else {
                        Action::Kick
                    };
                    let mut req = PunishRequest::new(
                        guild_id,
                        member.user.id,
                        action,
                        "Anti-raid: joined during an active raid",
                    );
                    req.target_tag = member.user.tag();
                    req.silent = true;
                    let _ = moderation::punish(sctx, data, req).await;
                    return Ok(JoinVerdict {
                        blocked: true,
                        note: Some(format!("auto-{} (raid mode)", cfg.antiraid_action)),
                    });
                }
                _ => {}
            }
        }
    }

    // ---- suspicious account ---------------------------------------------
    if cfg.suspicious_accounts == 1 {
        let created = member.user.id.created_at().unix_timestamp();
        let age_secs = Utc::now().timestamp() - created;
        if age_secs < cfg.suspicious_min_age {
            let detail = format!(
                "Account is only **{}** old (threshold {}).",
                time::humanize(age_secs),
                time::humanize(cfg.suspicious_min_age)
            );
            let alert = embed::titled("🕵️ Suspicious account joined", embed::WARN)
                .field("Member", format!("<@{}>", member.user.id.get()), true)
                .field("Account created", format!("<t:{created}:R>"), true)
                .description(detail.clone())
                .footer(embed::footer(format!("User ID: {}", member.user.id.get())));

            match cfg.suspicious_action.as_str() {
                "kick" => {
                    let mut req = PunishRequest::new(
                        guild_id,
                        member.user.id,
                        Action::Kick,
                        format!("Suspicious account: {detail}"),
                    );
                    req.target_tag = member.user.tag();
                    let _ = moderation::punish(sctx, data, req).await;
                    return Ok(JoinVerdict {
                        blocked: true,
                        note: Some("kicked: account too new".to_string()),
                    });
                }
                _ => {
                    if let Some(channel) = cfg.alert_channel {
                        let _ = serenity::ChannelId::new(channel as u64)
                            .send_message(&sctx.http, serenity::CreateMessage::new().embed(alert))
                            .await;
                    } else {
                        logs::send(sctx, &data.db, guild_i64, Log::Join, alert).await;
                    }
                    note = Some("flagged: new account".to_string());
                }
            }
        }
    }

    Ok(JoinVerdict {
        blocked: false,
        note,
    })
}

async fn trigger_raid_response(
    sctx: &serenity::Context,
    data: &Data,
    guild_id: GuildId,
    joins: usize,
    action: &str,
) {
    let guild_i64 = guild_id.get() as i64;
    let mut detail = format!(
        "**{joins}** accounts joined in a very short window. Raid mode is on for {RAID_MODE_MINUTES} minutes."
    );

    if action == "lockdown" {
        let locked = lockdown::auto_lockdown(
            sctx,
            &data.db,
            guild_id,
            "Automatic anti-raid lockdown",
        )
        .await;
        detail.push_str(&format!("\nLocked **{locked}** channel(s) automatically."));
        detail.push_str("\nUse `/lockdown end` once it is over.");
    } else {
        detail.push_str(&format!("\nResponse mode: **{action}**."));
    }

    let alert = embed::titled("🚨 Raid detected", embed::DANGER).description(detail);

    if let Ok(cfg) = crate::db::automod_config(&data.db, guild_i64).await {
        if let Some(channel) = cfg.alert_channel {
            let _ = serenity::ChannelId::new(channel as u64)
                .send_message(&sctx.http, serenity::CreateMessage::new().embed(alert.clone()))
                .await;
        }
    }
    logs::send(sctx, &data.db, guild_i64, Log::Server, alert).await;
}

/// Is this guild currently in raid mode?
pub fn in_raid_mode(data: &Data, guild_id: GuildId) -> bool {
    data.raid_mode
        .get(&guild_id.get())
        .map(|until| *until > Utc::now())
        .unwrap_or(false)
}

/// Force raid mode on or off from a command.
pub fn set_raid_mode(data: &Data, guild_id: GuildId, on: bool, minutes: i64) {
    if on {
        data.raid_mode.insert(
            guild_id.get(),
            Utc::now() + chrono::Duration::minutes(minutes),
        );
    } else {
        data.raid_mode.remove(&guild_id.get());
    }
}

/// Mass-kick accounts that joined inside a time window. Last resort cleanup.
pub async fn purge_recent_joins(
    sctx: &serenity::Context,
    data: &Data,
    guild_id: GuildId,
    minutes: i64,
    ban: bool,
) -> usize {
    let cutoff = time::to_sql(Utc::now() - chrono::Duration::minutes(minutes));
    let rows: Vec<i64> = sqlx::query_scalar(
        "SELECT user_id FROM members WHERE guild_id = ? AND last_join >= ?",
    )
    .bind(guild_id.get() as i64)
    .bind(&cutoff)
    .fetch_all(&data.db)
    .await
    .unwrap_or_default();

    let mut removed = 0usize;
    for user_id in rows {
        let mut req = PunishRequest::new(
            guild_id,
            UserId::new(user_id as u64),
            if ban { Action::Ban } else { Action::Kick },
            format!("Raid cleanup: joined in the last {minutes} minute(s)"),
        );
        req.silent = true;
        if moderation::punish(sctx, data, req).await.is_ok() {
            removed += 1;
        }
    }
    removed
}
