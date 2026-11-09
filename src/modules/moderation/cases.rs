//! Case lookup, warning history, moderator logs, escalation rules and appeals.

use poise::serenity_prelude as serenity;
use serenity::{User, UserId};
use sqlx::FromRow;

use crate::db;
use crate::logs::{self, Log};
use crate::state::{Ctx, Data, Error};
use crate::util::{embed, perms, text, time};

#[derive(Debug, Clone, FromRow)]
pub struct ModCase {
    pub case_no: i64,
    pub action: String,
    pub target_id: i64,
    pub target_tag: String,
    pub mod_id: i64,
    pub mod_tag: String,
    pub reason: String,
    pub duration: Option<i64>,
    pub active: i64,
    pub automated: i64,
    pub created_at: String,
}

fn line(case: &ModCase) -> String {
    let when = time::from_sql(&case.created_at)
        .map(|t| time::discord_ts(t, 'd'))
        .unwrap_or_else(|| case.created_at.clone());
    let mut row = format!(
        "`#{:<4}` **{}** · {when}",
        case.case_no,
        case.action.to_uppercase()
    );
    if let Some(secs) = case.duration {
        row.push_str(&format!(" · `{}`", time::humanize(secs)));
    }
    if case.active == 0 {
        row.push_str(" · *expired*");
    }
    if case.automated == 1 {
        row.push_str(" · 🤖");
    }
    row.push_str(&format!(
        "\n└ {} — by <@{}>",
        text::truncate(&text::escape_md(&case.reason), 140),
        case.mod_id
    ));
    row
}

// ===========================================================================
// /case
// ===========================================================================

/// Look up, edit or remove a moderation case.
#[poise::command(
    slash_command,
    guild_only,
    category = "Moderation",
    subcommands("case_view", "case_reason", "case_remove", "case_recent"),
    check = "perms::staff_only"
)]
pub async fn case(_ctx: Ctx<'_>) -> Result<(), Error> {
    Ok(())
}

/// Show one case in full.
#[poise::command(slash_command, rename = "view", guild_only)]
pub async fn case_view(
    ctx: Ctx<'_>,
    #[description = "Case number"] number: i64,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let found: Option<ModCase> =
        sqlx::query_as("SELECT * FROM mod_cases WHERE guild_id = ? AND case_no = ?")
            .bind(guild_id)
            .bind(number)
            .fetch_optional(&ctx.data().db)
            .await?;

    let Some(case) = found else {
        ctx.send(
            poise::CreateReply::default()
                .embed(embed::err(format!("There is no case #{number} here."))),
        )
        .await?;
        return Ok(());
    };

    let created = time::from_sql(&case.created_at)
        .map(time::full_and_relative)
        .unwrap_or_else(|| case.created_at.clone());

    let mut e = embed::titled(
        format!("Case #{} · {}", case.case_no, case.action.to_uppercase()),
        if case.active == 1 {
            embed::WARN
        } else {
            embed::MUTED
        },
    )
    .field(
        "Member",
        format!("<@{}>\n`{}`", case.target_id, case.target_tag),
        true,
    )
    .field(
        "Moderator",
        format!("<@{}>\n`{}`", case.mod_id, case.mod_tag),
        true,
    )
    .field("Status", if case.active == 1 { "Active" } else { "Closed" }, true)
    .field("Reason", text::truncate(&case.reason, 1000), false)
    .field("When", created, false);

    if let Some(secs) = case.duration {
        e = e.field("Duration", time::humanize(secs), true);
    }
    if case.automated == 1 {
        e = e.footer(embed::footer("Issued automatically by automod"));
    }

    ctx.send(poise::CreateReply::default().embed(e)).await?;
    Ok(())
}

/// Rewrite the reason on an existing case.
#[poise::command(slash_command, rename = "reason", guild_only)]
pub async fn case_reason(
    ctx: Ctx<'_>,
    #[description = "Case number"] number: i64,
    #[description = "New reason"] reason: String,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let result = sqlx::query("UPDATE mod_cases SET reason = ? WHERE guild_id = ? AND case_no = ?")
        .bind(&reason)
        .bind(guild_id)
        .bind(number)
        .execute(&ctx.data().db)
        .await?;

    if result.rows_affected() == 0 {
        ctx.send(
            poise::CreateReply::default()
                .embed(embed::err(format!("There is no case #{number} here."))),
        )
        .await?;
        return Ok(());
    }

    logs::send(
        ctx.serenity_context(),
        &ctx.data().db,
        guild_id,
        Log::Mod,
        embed::titled(format!("📝 Case #{number} edited"), embed::INFO)
            .description(format!("New reason: {}", text::truncate(&reason, 900)))
            .footer(embed::footer(format!("Edited by {}", ctx.author().tag()))),
    )
    .await;

    ctx.send(poise::CreateReply::default().embed(embed::ok(format!("Case #{number} updated."))))
        .await?;
    Ok(())
}

/// Delete a case outright. Use sparingly - prefer `/unwarn` to retire a warning.
#[poise::command(slash_command, rename = "remove", guild_only)]
pub async fn case_remove(
    ctx: Ctx<'_>,
    #[description = "Case number"] number: i64,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let result = sqlx::query("DELETE FROM mod_cases WHERE guild_id = ? AND case_no = ?")
        .bind(guild_id)
        .bind(number)
        .execute(&ctx.data().db)
        .await?;

    if result.rows_affected() == 0 {
        ctx.send(
            poise::CreateReply::default()
                .embed(embed::err(format!("There is no case #{number} here."))),
        )
        .await?;
        return Ok(());
    }

    logs::send(
        ctx.serenity_context(),
        &ctx.data().db,
        guild_id,
        Log::Mod,
        embed::titled(format!("🗑️ Case #{number} deleted"), embed::DANGER).description(format!(
            "Deleted by <@{}>",
            ctx.author().id.get()
        )),
    )
    .await;

    ctx.send(poise::CreateReply::default().embed(embed::ok(format!("Case #{number} deleted."))))
        .await?;
    Ok(())
}

/// The most recent cases in this server.
#[poise::command(slash_command, rename = "recent", guild_only)]
pub async fn case_recent(
    ctx: Ctx<'_>,
    #[description = "How many to show (max 15)"]
    #[min = 1]
    #[max = 15]
    limit: Option<u8>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let limit = limit.unwrap_or(10) as i64;
    let cases: Vec<ModCase> = sqlx::query_as(
        "SELECT * FROM mod_cases WHERE guild_id = ? ORDER BY case_no DESC LIMIT ?",
    )
    .bind(guild_id)
    .bind(limit)
    .fetch_all(&ctx.data().db)
    .await?;

    let body = if cases.is_empty() {
        "No cases recorded yet.".to_string()
    } else {
        cases.iter().map(line).collect::<Vec<_>>().join("\n\n")
    };

    ctx.send(
        poise::CreateReply::default()
            .embed(embed::titled("Recent moderation cases", embed::INFO).description(body)),
    )
    .await?;
    Ok(())
}

// ===========================================================================
// History
// ===========================================================================

/// Every case recorded against a member.
#[poise::command(
    slash_command,
    guild_only,
    category = "Moderation",
    check = "perms::staff_only"
)]
pub async fn history(
    ctx: Ctx<'_>,
    #[description = "Member to look up"] user: User,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let cases: Vec<ModCase> = sqlx::query_as(
        "SELECT * FROM mod_cases WHERE guild_id = ? AND target_id = ?
         ORDER BY case_no DESC LIMIT 15",
    )
    .bind(guild_id)
    .bind(user.id.get() as i64)
    .fetch_all(&ctx.data().db)
    .await?;

    let total: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM mod_cases WHERE guild_id = ? AND target_id = ?",
    )
    .bind(guild_id)
    .bind(user.id.get() as i64)
    .fetch_one(&ctx.data().db)
    .await?;

    let active_warns: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM mod_cases
         WHERE guild_id = ? AND target_id = ? AND action = 'warn' AND active = 1",
    )
    .bind(guild_id)
    .bind(user.id.get() as i64)
    .fetch_one(&ctx.data().db)
    .await?;

    let body = if cases.is_empty() {
        "A clean record - no cases at all.".to_string()
    } else {
        cases.iter().map(line).collect::<Vec<_>>().join("\n\n")
    };

    ctx.send(
        poise::CreateReply::default().embed(
            embed::titled(format!("History for {}", user.tag()), embed::INFO)
                .description(body)
                .field("Total cases", total.to_string(), true)
                .field("Active warnings", active_warns.to_string(), true)
                .thumbnail(user.face())
                .footer(embed::footer(format!(
                    "User ID: {} · showing up to 15 most recent",
                    user.id.get()
                ))),
        ),
    )
    .await?;
    Ok(())
}

/// A member's active warnings only.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    category = "Moderation",
    check = "perms::staff_only"
)]
pub async fn warnings(
    ctx: Ctx<'_>,
    #[description = "Member to look up"] user: User,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let cases: Vec<ModCase> = sqlx::query_as(
        "SELECT * FROM mod_cases
         WHERE guild_id = ? AND target_id = ? AND action = 'warn' AND active = 1
         ORDER BY case_no DESC",
    )
    .bind(guild_id)
    .bind(user.id.get() as i64)
    .fetch_all(&ctx.data().db)
    .await?;

    let body = if cases.is_empty() {
        "No active warnings.".to_string()
    } else {
        cases.iter().map(line).collect::<Vec<_>>().join("\n\n")
    };

    // Show where they sit on the escalation ladder.
    let next: Option<(i64, String)> = sqlx::query_as(
        "SELECT warn_count, action FROM escalation_rules
         WHERE guild_id = ? AND warn_count > ? ORDER BY warn_count ASC LIMIT 1",
    )
    .bind(guild_id)
    .bind(cases.len() as i64)
    .fetch_optional(&ctx.data().db)
    .await?;

    let mut e = embed::titled(
        format!("{} · {} active warning(s)", user.tag(), cases.len()),
        if cases.is_empty() {
            embed::SUCCESS
        } else {
            embed::WARN
        },
    )
    .description(body)
    .thumbnail(user.face());

    if let Some((count, action)) = next {
        e = e.footer(embed::footer(format!(
            "Next step: {action} at {count} warnings"
        )));
    }

    ctx.send(poise::CreateReply::default().embed(e)).await?;
    Ok(())
}

/// Retire a warning so it no longer counts towards escalation.
#[poise::command(
    slash_command,
    guild_only,
    category = "Moderation",
    check = "perms::staff_only"
)]
pub async fn unwarn(
    ctx: Ctx<'_>,
    #[description = "Case number of the warning"] case_number: i64,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let result = sqlx::query(
        "UPDATE mod_cases SET active = 0
         WHERE guild_id = ? AND case_no = ? AND action = 'warn'",
    )
    .bind(guild_id)
    .bind(case_number)
    .execute(&ctx.data().db)
    .await?;

    let body = if result.rows_affected() == 0 {
        embed::err(format!("Case #{case_number} is not an active warning."))
    } else {
        embed::ok(format!(
            "Warning #{case_number} retired. It no longer counts towards escalation."
        ))
    };
    ctx.send(poise::CreateReply::default().embed(body)).await?;
    Ok(())
}

/// Clear every active warning for a member.
#[poise::command(
    slash_command,
    guild_only,
    category = "Moderation",
    check = "perms::admin_only"
)]
pub async fn clearwarns(
    ctx: Ctx<'_>,
    #[description = "Member whose warnings should be cleared"] user: User,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let result = sqlx::query(
        "UPDATE mod_cases SET active = 0
         WHERE guild_id = ? AND target_id = ? AND action = 'warn' AND active = 1",
    )
    .bind(guild_id)
    .bind(user.id.get() as i64)
    .execute(&ctx.data().db)
    .await?;

    ctx.send(poise::CreateReply::default().embed(embed::ok(format!(
        "Cleared **{}** warning(s) for {}.",
        result.rows_affected(),
        user.tag()
    ))))
    .await?;
    Ok(())
}

/// Everything a moderator has done here.
#[poise::command(
    slash_command,
    guild_only,
    category = "Moderation",
    check = "perms::staff_only"
)]
pub async fn modlogs(
    ctx: Ctx<'_>,
    #[description = "Moderator to audit"] moderator: User,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let mod_id = moderator.id.get() as i64;

    let breakdown: Vec<(String, i64)> = sqlx::query_as(
        "SELECT action, COUNT(*) FROM mod_cases
         WHERE guild_id = ? AND mod_id = ? GROUP BY action ORDER BY COUNT(*) DESC",
    )
    .bind(guild_id)
    .bind(mod_id)
    .fetch_all(&ctx.data().db)
    .await?;

    let recent: Vec<ModCase> = sqlx::query_as(
        "SELECT * FROM mod_cases WHERE guild_id = ? AND mod_id = ?
         ORDER BY case_no DESC LIMIT 10",
    )
    .bind(guild_id)
    .bind(mod_id)
    .fetch_all(&ctx.data().db)
    .await?;

    let total: i64 = breakdown.iter().map(|(_, n)| n).sum();
    let summary = if breakdown.is_empty() {
        "No actions recorded.".to_string()
    } else {
        breakdown
            .iter()
            .map(|(action, count)| format!("**{action}** · {count}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let recent_body = if recent.is_empty() {
        "-".to_string()
    } else {
        recent.iter().map(line).collect::<Vec<_>>().join("\n\n")
    };

    ctx.send(
        poise::CreateReply::default().embed(
            embed::titled(format!("Moderator log · {}", moderator.tag()), embed::INFO)
                .field("Total actions", total.to_string(), true)
                .field("Breakdown", summary, true)
                .field("Latest", text::truncate(&recent_body, 2000), false)
                .thumbnail(moderator.face()),
        ),
    )
    .await?;
    Ok(())
}

// ===========================================================================
// Escalation
// ===========================================================================

/// Configure what happens automatically at a given warning count.
#[poise::command(
    slash_command,
    guild_only,
    category = "Moderation",
    subcommands("escalation_set", "escalation_list", "escalation_remove"),
    check = "perms::admin_only"
)]
pub async fn escalation(_ctx: Ctx<'_>) -> Result<(), Error> {
    Ok(())
}

#[derive(Debug, poise::ChoiceParameter)]
pub enum EscalationAction {
    #[name = "timeout"]
    Timeout,
    #[name = "kick"]
    Kick,
    #[name = "ban"]
    Ban,
}

impl EscalationAction {
    fn key(&self) -> &'static str {
        match self {
            EscalationAction::Timeout => "timeout",
            EscalationAction::Kick => "kick",
            EscalationAction::Ban => "ban",
        }
    }
}

/// Add or replace a rung on the ladder.
#[poise::command(slash_command, rename = "set", guild_only)]
pub async fn escalation_set(
    ctx: Ctx<'_>,
    #[description = "Number of active warnings that triggers this"]
    #[min = 1]
    #[max = 50]
    warnings: i64,
    #[description = "What should happen"] action: EscalationAction,
    #[description = "Timeout length, e.g. 1h (timeout only)"] duration: Option<String>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let secs = duration
        .as_deref()
        .and_then(time::parse_duration)
        .map(|d| d.as_secs() as i64);

    sqlx::query(
        "INSERT INTO escalation_rules (guild_id, warn_count, action, duration)
         VALUES (?, ?, ?, ?)
         ON CONFLICT(guild_id, warn_count) DO UPDATE SET
            action = excluded.action, duration = excluded.duration",
    )
    .bind(guild_id)
    .bind(warnings)
    .bind(action.key())
    .bind(secs)
    .execute(&ctx.data().db)
    .await?;

    let tail = match secs {
        Some(s) => format!(" for `{}`", time::humanize(s)),
        None => String::new(),
    };
    ctx.send(poise::CreateReply::default().embed(embed::ok(format!(
        "At **{warnings}** active warnings I will **{}**{tail}.",
        action.key()
    ))))
    .await?;
    Ok(())
}

/// Show the ladder.
#[poise::command(slash_command, rename = "list", guild_only)]
pub async fn escalation_list(ctx: Ctx<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let rules: Vec<(i64, String, Option<i64>)> = sqlx::query_as(
        "SELECT warn_count, action, duration FROM escalation_rules
         WHERE guild_id = ? ORDER BY warn_count ASC",
    )
    .bind(guild_id)
    .fetch_all(&ctx.data().db)
    .await?;

    let body = if rules.is_empty() {
        "No escalation configured. Warnings will only be recorded.\nAdd a rung with `/escalation set`."
            .to_string()
    } else {
        rules
            .iter()
            .map(|(count, action, duration)| {
                let tail = duration
                    .map(|d| format!(" for `{}`", time::humanize(d)))
                    .unwrap_or_default();
                format!("**{count}** warnings → **{action}**{tail}")
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    ctx.send(
        poise::CreateReply::default()
            .embed(embed::titled("Automatic punishment escalation", embed::INFO).description(body)),
    )
    .await?;
    Ok(())
}

/// Remove a rung.
#[poise::command(slash_command, rename = "remove", guild_only)]
pub async fn escalation_remove(
    ctx: Ctx<'_>,
    #[description = "Warning count to remove"] warnings: i64,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    sqlx::query("DELETE FROM escalation_rules WHERE guild_id = ? AND warn_count = ?")
        .bind(guild_id)
        .bind(warnings)
        .execute(&ctx.data().db)
        .await?;
    ctx.send(
        poise::CreateReply::default()
            .embed(embed::ok(format!("Removed the rule for {warnings} warnings."))),
    )
    .await?;
    Ok(())
}

// ===========================================================================
// Appeals
// ===========================================================================

/// Appeal a moderation decision, or review appeals as staff.
#[poise::command(
    slash_command,
    guild_only,
    category = "Moderation",
    subcommands("appeal_submit", "appeal_list", "appeal_decide")
)]
pub async fn appeal(_ctx: Ctx<'_>) -> Result<(), Error> {
    Ok(())
}

/// Submit an appeal for one of your cases.
#[poise::command(slash_command, rename = "submit", guild_only)]
pub async fn appeal_submit(
    ctx: Ctx<'_>,
    #[description = "Case number you are appealing"] case_number: Option<i64>,
    #[description = "Why should this be reconsidered?"] message: String,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let author = ctx.author().id.get() as i64;

    let open: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM appeals WHERE guild_id = ? AND user_id = ? AND status = 'pending'",
    )
    .bind(guild_id)
    .bind(author)
    .fetch_one(&ctx.data().db)
    .await?;

    if open >= 2 {
        ctx.send(
            poise::CreateReply::default()
                .embed(embed::err(
                    "You already have two appeals waiting for review. Please wait for staff to get to them.",
                ))
                .ephemeral(true),
        )
        .await?;
        return Ok(());
    }

    // Only allow appealing a case that is actually theirs.
    if let Some(number) = case_number {
        let owns: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM mod_cases WHERE guild_id = ? AND case_no = ? AND target_id = ?",
        )
        .bind(guild_id)
        .bind(number)
        .bind(author)
        .fetch_one(&ctx.data().db)
        .await?;
        if owns == 0 {
            ctx.send(
                poise::CreateReply::default()
                    .embed(embed::err("That case number is not one of yours."))
                    .ephemeral(true),
            )
            .await?;
            return Ok(());
        }
    }

    let id = sqlx::query(
        "INSERT INTO appeals (guild_id, case_no, user_id, content, created_at)
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(guild_id)
    .bind(case_number)
    .bind(author)
    .bind(&message)
    .bind(time::sql_now())
    .execute(&ctx.data().db)
    .await?
    .last_insert_rowid();

    // Route to the appeal channel if one is set, otherwise the mod log.
    let cfg = db::guild_config(&ctx.data().db, guild_id).await?;
    let notice = embed::titled(format!("📬 Appeal #{id}"), embed::INFO)
        .field("From", format!("<@{author}>"), true)
        .field(
            "Case",
            case_number
                .map(|n| format!("#{n}"))
                .unwrap_or_else(|| "not specified".to_string()),
            true,
        )
        .field("Message", text::truncate(&message, 1500), false)
        .footer(embed::footer("Review with /appeal decide"));

    if let Some(channel) = cfg.appeal_channel {
        let _ = serenity::ChannelId::new(channel as u64)
            .send_message(
                ctx.http(),
                serenity::CreateMessage::new().embed(notice.clone()),
            )
            .await;
    } else {
        logs::send(
            ctx.serenity_context(),
            &ctx.data().db,
            guild_id,
            Log::Mod,
            notice,
        )
        .await;
    }

    ctx.send(
        poise::CreateReply::default()
            .embed(embed::ok(format!(
                "Appeal **#{id}** submitted. Staff will review it and you will hear back by DM."
            )))
            .ephemeral(true),
    )
    .await?;
    Ok(())
}

/// List appeals awaiting review.
#[poise::command(slash_command, rename = "list", guild_only, check = "perms::staff_only")]
pub async fn appeal_list(ctx: Ctx<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let rows: Vec<(i64, Option<i64>, i64, String, String)> = sqlx::query_as(
        "SELECT id, case_no, user_id, content, created_at FROM appeals
         WHERE guild_id = ? AND status = 'pending' ORDER BY id ASC LIMIT 10",
    )
    .bind(guild_id)
    .fetch_all(&ctx.data().db)
    .await?;

    let body = if rows.is_empty() {
        "Nothing waiting. Good job.".to_string()
    } else {
        rows.iter()
            .map(|(id, case_no, user_id, content, created)| {
                let when = time::from_sql(created)
                    .map(|t| time::discord_ts(t, 'R'))
                    .unwrap_or_default();
                format!(
                    "**#{id}** · <@{user_id}> · case {} · {when}\n└ {}",
                    case_no
                        .map(|c| format!("#{c}"))
                        .unwrap_or_else(|| "n/a".into()),
                    text::truncate(&text::escape_md(content), 200)
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    };

    ctx.send(
        poise::CreateReply::default()
            .embed(embed::titled("Pending appeals", embed::INFO).description(body)),
    )
    .await?;
    Ok(())
}

#[derive(Debug, poise::ChoiceParameter)]
pub enum AppealVerdict {
    #[name = "accept"]
    Accept,
    #[name = "deny"]
    Deny,
}

/// Accept or deny an appeal. Accepting a ban appeal also lifts the ban.
#[poise::command(slash_command, rename = "decide", guild_only, check = "perms::staff_only")]
pub async fn appeal_decide(
    ctx: Ctx<'_>,
    #[description = "Appeal number"] appeal_id: i64,
    #[description = "Decision"] verdict: AppealVerdict,
    #[description = "Note sent to the member"] note: Option<String>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let found: Option<(i64, Option<i64>, String)> = sqlx::query_as(
        "SELECT user_id, case_no, status FROM appeals WHERE guild_id = ? AND id = ?",
    )
    .bind(guild_id)
    .bind(appeal_id)
    .fetch_optional(&ctx.data().db)
    .await?;

    let Some((user_id, case_no, status)) = found else {
        ctx.send(poise::CreateReply::default().embed(embed::err("No such appeal.")))
            .await?;
        return Ok(());
    };
    if status != "pending" {
        ctx.send(poise::CreateReply::default().embed(embed::err(format!(
            "That appeal was already **{status}**."
        ))))
        .await?;
        return Ok(());
    }

    let accepted = matches!(verdict, AppealVerdict::Accept);
    let status_text = if accepted { "accepted" } else { "denied" };

    sqlx::query(
        "UPDATE appeals SET status = ?, reviewer = ?, note = ?, decided_at = ? WHERE id = ?",
    )
    .bind(status_text)
    .bind(ctx.author().id.get() as i64)
    .bind(&note)
    .bind(time::sql_now())
    .bind(appeal_id)
    .execute(&ctx.data().db)
    .await?;

    let mut extra = String::new();
    if accepted {
        // Retire the case and reverse it where we can.
        if let Some(number) = case_no {
            let action: Option<String> = sqlx::query_scalar(
                "SELECT action FROM mod_cases WHERE guild_id = ? AND case_no = ?",
            )
            .bind(guild_id)
            .bind(number)
            .fetch_optional(&ctx.data().db)
            .await?;

            sqlx::query("UPDATE mod_cases SET active = 0 WHERE guild_id = ? AND case_no = ?")
                .bind(guild_id)
                .bind(number)
                .execute(&ctx.data().db)
                .await?;

            let guild = ctx.guild_id().unwrap();
            let target = UserId::new(user_id as u64);
            match action.as_deref() {
                Some("ban") => {
                    if crate::compat::unban(&ctx.serenity_context().http, guild, target)
                        .await
                        .is_ok()
                    {
                        extra = " The ban has been lifted.".to_string();
                    }
                }
                Some("timeout") => {
                    if crate::compat::clear_timeout(
                        &ctx.serenity_context().http,
                        guild,
                        target,
                        "Appeal accepted",
                    )
                    .await
                    .is_ok()
                    {
                        extra = " The timeout has been lifted.".to_string();
                    }
                }
                _ => {
                    extra = " The case has been retired.".to_string();
                }
            }
        }
    }

    // Tell the member.
    let dm = embed::titled(
        format!("Your appeal was {status_text}"),
        if accepted {
            embed::SUCCESS
        } else {
            embed::DANGER
        },
    )
    .description(format!(
        "Server: **{}**{extra}",
        super::guild_name(ctx.serenity_context(), ctx.guild_id().unwrap())
    ))
    .field(
        "Staff note",
        note.clone().unwrap_or_else(|| "No note provided.".into()),
        false,
    );

    let delivered = match UserId::new(user_id as u64)
        .create_dm_channel(ctx.serenity_context())
        .await
    {
        Ok(channel) => channel
            .send_message(ctx.http(), serenity::CreateMessage::new().embed(dm))
            .await
            .is_ok(),
        Err(_) => false,
    };

    ctx.send(poise::CreateReply::default().embed(embed::ok(format!(
        "Appeal #{appeal_id} **{status_text}**.{extra}{}",
        if delivered {
            ""
        } else {
            " I could not DM them."
        }
    ))))
    .await?;
    Ok(())
}

pub fn commands() -> Vec<poise::Command<Data, Error>> {
    vec![
        case(),
        history(),
        warnings(),
        unwarn(),
        clearwarns(),
        modlogs(),
        escalation(),
        appeal(),
    ]
}
