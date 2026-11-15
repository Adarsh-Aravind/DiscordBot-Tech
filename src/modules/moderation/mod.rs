//! Moderation: the case engine plus every staff-facing command.
//!
//! Every punishment in the bot - manual, automod or escalation - goes through
//! [`punish`], so there is exactly one place that writes a case, notifies the
//! member and posts to the mod log.

pub mod actions;
pub mod cases;
pub mod lockdown;
pub mod purge;

use chrono::{Duration, Utc};
use poise::serenity_prelude as serenity;
use serenity::{CreateEmbed, CreateMessage, GuildId, UserId};

use crate::compat;
use crate::db::{self, Db};
use crate::logs::{self, Log};
use crate::state::{Data, Error};
use crate::util::{embed, time};

/// The punishments the bot can hand out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Note,
    Warn,
    Timeout,
    Untimeout,
    Kick,
    Softban,
    Ban,
    Unban,
}

impl Action {
    pub fn as_str(self) -> &'static str {
        match self {
            Action::Note => "note",
            Action::Warn => "warn",
            Action::Timeout => "timeout",
            Action::Untimeout => "untimeout",
            Action::Kick => "kick",
            Action::Softban => "softban",
            Action::Ban => "ban",
            Action::Unban => "unban",
        }
    }

    pub fn from_name(raw: &str) -> Option<Self> {
        Some(match raw {
            "note" => Action::Note,
            "warn" => Action::Warn,
            "timeout" | "mute" => Action::Timeout,
            "untimeout" | "unmute" => Action::Untimeout,
            "kick" => Action::Kick,
            "softban" => Action::Softban,
            "ban" => Action::Ban,
            "unban" => Action::Unban,
            _ => return None,
        })
    }

    pub fn past_tense(self) -> &'static str {
        match self {
            Action::Note => "noted",
            Action::Warn => "warned",
            Action::Timeout => "timed out",
            Action::Untimeout => "untimed out",
            Action::Kick => "kicked",
            Action::Softban => "softbanned",
            Action::Ban => "banned",
            Action::Unban => "unbanned",
        }
    }

    pub fn emoji(self) -> &'static str {
        match self {
            Action::Note => "📝",
            Action::Warn => "⚠️",
            Action::Timeout => "🔇",
            Action::Untimeout => "🔊",
            Action::Kick => "👢",
            Action::Softban => "🧹",
            Action::Ban => "🔨",
            Action::Unban => "♻️",
        }
    }

    pub fn colour(self) -> u32 {
        match self {
            Action::Note => embed::MUTED,
            Action::Warn => embed::WARN,
            Action::Timeout | Action::Kick | Action::Softban => 0xE67E22,
            Action::Ban => embed::DANGER,
            Action::Untimeout | Action::Unban => embed::SUCCESS,
        }
    }

    /// Whether this action removes the member, so the DM must go out first.
    fn dm_before(self) -> bool {
        matches!(self, Action::Kick | Action::Ban | Action::Softban)
    }
}

pub struct PunishRequest {
    pub guild_id: GuildId,
    pub target: UserId,
    pub target_tag: String,
    pub moderator: UserId,
    pub moderator_tag: String,
    pub action: Action,
    pub reason: String,
    /// Timeout duration in seconds; ignored by other actions.
    pub duration: Option<i64>,
    /// Days of the target's messages to delete when banning (0-7).
    pub delete_days: u8,
    /// True when the bot decided on this itself (automod / escalation).
    pub automated: bool,
    /// Skip the DM, e.g. during a raid when we are actioning dozens of accounts.
    pub silent: bool,
}

impl PunishRequest {
    pub fn new(
        guild_id: GuildId,
        target: UserId,
        action: Action,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            guild_id,
            target,
            target_tag: target.to_string(),
            moderator: UserId::new(1),
            moderator_tag: "Automod".to_string(),
            action,
            reason: reason.into(),
            duration: None,
            delete_days: 0,
            automated: true,
            silent: false,
        }
    }
}

pub struct PunishOutcome {
    pub case_no: i64,
    pub dm_delivered: bool,
    /// Set when automatic escalation fired off the back of this case.
    pub escalated: Option<String>,
}

/// Carry out a punishment: notify, action, record a case, log it, then escalate.
pub async fn punish(
    sctx: &serenity::Context,
    data: &Data,
    mut req: PunishRequest,
) -> Result<PunishOutcome, Error> {
    let guild_i64 = req.guild_id.get() as i64;
    let http = &sctx.http;
    let guild_name = guild_name(sctx, req.guild_id);

    // Clamp once, here, so the duration we apply is the duration we store and
    // the one we tell the member about. Discord's own ceiling for a timeout is
    // 28 days; a case that claimed 60 days would have left `expires_at` months
    // after the member could actually talk again.
    if req.action == Action::Timeout {
        req.duration = Some(req.duration.unwrap_or(600).clamp(60, 28 * 86_400));
    }

    let case_no = db::next_number(&data.db, "mod_cases", "case_no", guild_i64).await?;

    let expires_at = req
        .duration
        .map(|secs| time::to_sql(Utc::now() + Duration::seconds(secs)));

    // DM first for actions that remove the member, otherwise we lose the channel.
    let mut dm_delivered = false;
    if !req.silent && req.action.dm_before() {
        dm_delivered = notify_member(sctx, data, &req, case_no, &guild_name).await;
    }

    // ---- carry out the action -------------------------------------------
    match req.action {
        Action::Note | Action::Warn => {}
        Action::Timeout => {
            let secs = req.duration.unwrap_or(600);
            compat::timeout_until(
                http,
                req.guild_id,
                req.target,
                Utc::now() + Duration::seconds(secs),
                &audit_reason(&req),
            )
            .await?;
        }
        Action::Untimeout => {
            compat::clear_timeout(http, req.guild_id, req.target, &audit_reason(&req)).await?;
        }
        Action::Kick => {
            compat::kick(http, req.guild_id, req.target, &audit_reason(&req)).await?;
        }
        Action::Ban => {
            compat::ban(
                http,
                req.guild_id,
                req.target,
                req.delete_days,
                &audit_reason(&req),
            )
            .await?;
        }
        Action::Softban => {
            compat::ban(
                http,
                req.guild_id,
                req.target,
                req.delete_days.max(1),
                &audit_reason(&req),
            )
            .await?;
            compat::unban(http, req.guild_id, req.target).await?;
        }
        Action::Unban => {
            compat::unban(http, req.guild_id, req.target).await?;
        }
    }

    // ---- record ----------------------------------------------------------
    // The action above has already happened. If writing the case fails we have
    // a punished member and no record of it, which is bad - but telling the
    // moderator "that did not work" would be worse, because they would try
    // again. Shout in the log and carry on.
    let recorded = sqlx::query(
        "INSERT INTO mod_cases
            (guild_id, case_no, action, target_id, target_tag, mod_id, mod_tag,
             reason, duration, expires_at, active, automated, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1, ?, ?)",
    )
    .bind(guild_i64)
    .bind(case_no)
    .bind(req.action.as_str())
    .bind(req.target.get() as i64)
    .bind(&req.target_tag)
    .bind(req.moderator.get() as i64)
    .bind(&req.moderator_tag)
    .bind(&req.reason)
    .bind(req.duration)
    .bind(&expires_at)
    .bind(i64::from(req.automated))
    .bind(time::sql_now())
    .execute(&data.db)
    .await;

    if let Err(why) = recorded {
        tracing::error!(
            guild = guild_i64,
            case = case_no,
            target = req.target.get(),
            action = req.action.as_str(),
            "the punishment was carried out but the case could not be recorded: {why}"
        );
    }

    // Reversals close the matching open case.
    if matches!(req.action, Action::Untimeout | Action::Unban) {
        let closes = if req.action == Action::Untimeout {
            "timeout"
        } else {
            "ban"
        };
        sqlx::query(
            "UPDATE mod_cases SET active = 0
             WHERE guild_id = ? AND target_id = ? AND action = ? AND active = 1",
        )
        .bind(guild_i64)
        .bind(req.target.get() as i64)
        .bind(closes)
        .execute(&data.db)
        .await?;
    }

    // DM after the fact for actions that keep the member around.
    if !req.silent && !req.action.dm_before() {
        dm_delivered = notify_member(sctx, data, &req, case_no, &guild_name).await;
    }

    // ---- log -------------------------------------------------------------
    logs::send(
        sctx,
        &data.db,
        guild_i64,
        Log::Mod,
        case_embed(&req, case_no, dm_delivered),
    )
    .await;

    // ---- escalation ------------------------------------------------------
    let escalated = if req.action == Action::Warn {
        escalate(sctx, data, &req).await?
    } else {
        None
    };

    Ok(PunishOutcome {
        case_no,
        dm_delivered,
        escalated,
    })
}

fn audit_reason(req: &PunishRequest) -> String {
    let who = if req.automated {
        "Automod".to_string()
    } else {
        req.moderator_tag.clone()
    };
    crate::util::text::truncate(&format!("{who}: {}", req.reason), 400)
}

pub fn guild_name(sctx: &serenity::Context, guild_id: GuildId) -> String {
    guild_id
        .to_guild_cached(&sctx.cache)
        .map(|g| g.name.clone())
        .unwrap_or_else(|| "the server".to_string())
}

fn case_embed(req: &PunishRequest, case_no: i64, dm_delivered: bool) -> CreateEmbed {
    let mut e = embed::titled(
        format!(
            "{} Case #{case_no} · {}",
            req.action.emoji(),
            req.action.as_str().to_uppercase()
        ),
        req.action.colour(),
    )
    .field(
        "Member",
        format!("<@{}> `{}`", req.target.get(), req.target_tag),
        true,
    )
    .field(
        "Moderator",
        if req.automated {
            "Automod".to_string()
        } else {
            format!("<@{}>", req.moderator.get())
        },
        true,
    )
    .field("Reason", crate::util::text::truncate(&req.reason, 1000), false);

    if let Some(secs) = req.duration {
        e = e.field("Duration", time::humanize(secs), true);
        e = e.field(
            "Expires",
            time::discord_ts(Utc::now() + Duration::seconds(secs), 'R'),
            true,
        );
    }
    if !matches!(req.action, Action::Note) {
        e = e.field(
            "Member notified",
            if dm_delivered { "Yes" } else { "No (DMs closed)" },
            true,
        );
    }
    e.footer(embed::footer(format!("User ID: {}", req.target.get())))
        .timestamp(serenity::Timestamp::now())
}

/// DM the member what happened and how to appeal. Returns whether it landed.
async fn notify_member(
    sctx: &serenity::Context,
    data: &Data,
    req: &PunishRequest,
    case_no: i64,
    guild_name: &str,
) -> bool {
    if req.action == Action::Note {
        return false;
    }

    let mut e = embed::titled(
        format!("{} You were {} in {guild_name}", req.action.emoji(), req.action.past_tense()),
        req.action.colour(),
    )
    .field("Reason", crate::util::text::truncate(&req.reason, 1000), false)
    .field("Case", format!("#{case_no}"), true);

    if let Some(secs) = req.duration {
        e = e.field("Duration", time::humanize(secs), true);
    }

    let appealable = matches!(
        req.action,
        Action::Warn | Action::Timeout | Action::Kick | Action::Ban
    );
    if appealable {
        e = e.footer(embed::footer(
            "You can appeal with /appeal submit in the server, or by replying to this bot if the server allows it.",
        ));
    }

    let _ = data; // reserved for per-guild DM templates
    match req.target.create_dm_channel(sctx).await {
        Ok(channel) => channel
            .send_message(&sctx.http, CreateMessage::new().embed(e))
            .await
            .is_ok(),
        Err(_) => false,
    }
}

/// Count active warnings and apply the configured escalation rule, if any.
async fn escalate(
    sctx: &serenity::Context,
    data: &Data,
    req: &PunishRequest,
) -> Result<Option<String>, Error> {
    let guild_i64 = req.guild_id.get() as i64;
    let warns: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM mod_cases
         WHERE guild_id = ? AND target_id = ? AND action = 'warn' AND active = 1",
    )
    .bind(guild_i64)
    .bind(req.target.get() as i64)
    .fetch_one(&data.db)
    .await?;

    let rule: Option<(String, Option<i64>)> = sqlx::query_as(
        "SELECT action, duration FROM escalation_rules
         WHERE guild_id = ? AND warn_count = ?",
    )
    .bind(guild_i64)
    .bind(warns)
    .fetch_optional(&data.db)
    .await?;

    let Some((action_raw, duration)) = rule else {
        return Ok(None);
    };
    let Some(action) = Action::from_name(&action_raw) else {
        return Ok(None);
    };

    // Escalating a warning into another warning would raise the active warning
    // count and re-enter this function, walking up the whole ladder from one
    // `/warn`. A rule like that is a configuration mistake, not an instruction.
    if matches!(action, Action::Warn | Action::Note) {
        tracing::warn!(
            guild = guild_i64,
            warns,
            "ignoring an escalation rule that escalates a warning into another warning"
        );
        return Ok(None);
    }

    let follow_up = PunishRequest {
        guild_id: req.guild_id,
        target: req.target,
        target_tag: req.target_tag.clone(),
        moderator: req.moderator,
        moderator_tag: "Automatic escalation".to_string(),
        action,
        reason: format!("Automatic escalation: reached {warns} active warnings"),
        duration: if action == Action::Timeout {
            Some(duration.unwrap_or(3600))
        } else {
            None
        },
        delete_days: 0,
        automated: true,
        silent: false,
    };

    // Boxed because `punish` recurses into itself through escalation.
    let outcome = Box::pin(punish(sctx, data, follow_up)).await?;
    Ok(Some(format!(
        "{} (case #{})",
        action.past_tense(),
        outcome.case_no
    )))
}

/// Re-check expired timeouts and bans on a timer, closing stale cases.
pub async fn sweep_expired(sctx: &serenity::Context, db: &Db) {
    let now = time::sql_now();
    let rows: Vec<(i64, i64, i64, String)> = match sqlx::query_as(
        "SELECT id, guild_id, target_id, action FROM mod_cases
         WHERE active = 1 AND expires_at IS NOT NULL AND expires_at <= ?",
    )
    .bind(&now)
    .fetch_all(db)
    .await
    {
        Ok(r) => r,
        Err(why) => {
            tracing::warn!("expiry sweep query failed: {why}");
            return;
        }
    };

    for (id, guild_id, target_id, action) in rows {
        if action == "ban" {
            // Temporary ban: lift it.
            let _ = compat::unban(
                &sctx.http,
                GuildId::new(guild_id as u64),
                UserId::new(target_id as u64),
            )
            .await;
            logs::send(
                sctx,
                db,
                guild_id,
                Log::Mod,
                embed::titled("♻️ Temporary ban expired", embed::SUCCESS).description(format!(
                    "<@{target_id}> was unbanned automatically."
                )),
            )
            .await;
        }
        let _ = sqlx::query("UPDATE mod_cases SET active = 0 WHERE id = ?")
            .bind(id)
            .execute(db)
            .await;
    }
}

pub fn commands() -> Vec<poise::Command<Data, Error>> {
    let mut all = actions::commands();
    all.extend(cases::commands());
    all.extend(purge::commands());
    all.extend(lockdown::commands());
    all
}
