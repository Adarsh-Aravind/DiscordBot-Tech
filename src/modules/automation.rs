//! Automation: scheduled messages, reminders, custom commands, webhook
//! notifications, weekly reports and the workflow engine.

use chrono::Utc;
use poise::serenity_prelude as serenity;
use serenity::{ChannelId, CreateMessage, GuildChannel, GuildId, Role, RoleId, UserId};

use crate::db::{self, Db};
use crate::state::{Ctx, Data, Error};
use crate::util::{embed, perms, text, time};

// ===========================================================================
// Scheduled messages
// ===========================================================================

/// Post a message now or on a schedule.
#[poise::command(
    slash_command,
    guild_only,
    category = "Automation",
    subcommands("schedule_add", "schedule_list", "schedule_remove", "schedule_toggle"),
    check = "perms::admin_only"
)]
pub async fn schedule(_ctx: Ctx<'_>) -> Result<(), Error> {
    Ok(())
}

/// Schedule a message, optionally repeating.
#[poise::command(slash_command, rename = "add", guild_only)]
pub async fn schedule_add(
    ctx: Ctx<'_>,
    #[description = "Channel to post in"] channel: GuildChannel,
    #[description = "Message text. Use \\n for new lines."] message: String,
    #[description = "When to send first: a delay like 1h, or 2026-03-01 18:00 UTC"] when: String,
    #[description = "Repeat every, e.g. 24h. Leave empty for one-off."] every: Option<String>,
    #[description = "Send as an embed"] as_embed: Option<bool>,
    #[description = "Embed title"] title: Option<String>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;

    let Some(first_run) = time::parse_when(&when) else {
        ctx.send(poise::CreateReply::default().embed(embed::err(
            "I could not read that time. Use `1h`, or `2026-03-01 18:00` (UTC).",
        )))
        .await?;
        return Ok(());
    };

    let interval = match every.as_deref() {
        Some(raw) if !raw.trim().is_empty() => match time::parse_duration(raw) {
            Some(d) if d.as_secs() >= 300 => d.as_secs() as i64,
            Some(_) => {
                ctx.send(poise::CreateReply::default().embed(embed::err(
                    "The repeat interval must be at least 5 minutes.",
                )))
                .await?;
                return Ok(());
            }
            None => {
                ctx.send(
                    poise::CreateReply::default()
                        .embed(embed::err("I could not read that repeat interval.")),
                )
                .await?;
                return Ok(());
            }
        },
        _ => 0,
    };

    let id = sqlx::query(
        "INSERT INTO scheduled_messages
            (guild_id, channel_id, content, as_embed, title, next_run, interval_secs, created_by)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(guild_id)
    .bind(channel.id.get() as i64)
    .bind(message.replace("\\n", "\n"))
    .bind(i64::from(as_embed.unwrap_or(false)))
    .bind(&title)
    .bind(time::to_sql(first_run))
    .bind(interval)
    .bind(ctx.author().id.get() as i64)
    .execute(&ctx.data().db)
    .await?
    .last_insert_rowid();

    ctx.send(
        poise::CreateReply::default().embed(
            embed::ok(format!(
                "Scheduled message **#{id}** for <#{}>, first run {}.",
                channel.id.get(),
                time::discord_ts(first_run, 'F')
            ))
            .footer(embed::footer(if interval > 0 {
                format!("Repeats every {}", time::humanize(interval))
            } else {
                "One-off".to_string()
            })),
        ),
    )
    .await?;
    Ok(())
}

#[poise::command(slash_command, rename = "list", guild_only)]
pub async fn schedule_list(ctx: Ctx<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let rows: Vec<(i64, i64, String, String, i64, i64, i64)> = sqlx::query_as(
        "SELECT id, channel_id, content, next_run, interval_secs, enabled, runs
         FROM scheduled_messages WHERE guild_id = ? ORDER BY next_run ASC",
    )
    .bind(guild_id)
    .fetch_all(&ctx.data().db)
    .await?;

    let body = if rows.is_empty() {
        "Nothing scheduled.".to_string()
    } else {
        rows.iter()
            .map(|(id, channel_id, content, next_run, interval, enabled, runs)| {
                let next = time::from_sql(next_run)
                    .map(|t| time::discord_ts(t, 'R'))
                    .unwrap_or_default();
                format!(
                    "{} **#{id}** <#{channel_id}> · next {next} · {} · {runs} sent\n└ {}",
                    if *enabled == 1 { "🟢" } else { "🔴" },
                    if *interval > 0 {
                        format!("every {}", time::humanize(*interval))
                    } else {
                        "one-off".to_string()
                    },
                    text::truncate(&text::escape_md(content), 90)
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    ctx.send(
        poise::CreateReply::default()
            .embed(embed::titled("⏰ Scheduled messages", embed::INFO).description(body)),
    )
    .await?;
    Ok(())
}

#[poise::command(slash_command, rename = "remove", guild_only)]
pub async fn schedule_remove(
    ctx: Ctx<'_>,
    #[description = "Schedule number"] id: i64,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let result = sqlx::query("DELETE FROM scheduled_messages WHERE id = ? AND guild_id = ?")
        .bind(id)
        .bind(guild_id)
        .execute(&ctx.data().db)
        .await?;
    let body = if result.rows_affected() == 0 {
        embed::err("No schedule with that number.")
    } else {
        embed::ok("Removed.")
    };
    ctx.send(poise::CreateReply::default().embed(body)).await?;
    Ok(())
}

#[poise::command(slash_command, rename = "toggle", guild_only)]
pub async fn schedule_toggle(
    ctx: Ctx<'_>,
    #[description = "Schedule number"] id: i64,
    #[description = "Enabled?"] enabled: bool,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    sqlx::query("UPDATE scheduled_messages SET enabled = ? WHERE id = ? AND guild_id = ?")
        .bind(i64::from(enabled))
        .bind(id)
        .bind(guild_id)
        .execute(&ctx.data().db)
        .await?;
    ctx.send(poise::CreateReply::default().embed(embed::ok(format!(
        "Schedule #{id} is now {}.",
        if enabled { "enabled" } else { "paused" }
    ))))
    .await?;
    Ok(())
}

/// Post an announcement now.
#[poise::command(
    slash_command,
    guild_only,
    category = "Automation",
    check = "perms::staff_only"
)]
pub async fn announce(
    ctx: Ctx<'_>,
    #[description = "Channel to announce in"] channel: GuildChannel,
    #[description = "The announcement. Use \\n for new lines."] message: String,
    #[description = "Title"] title: Option<String>,
    #[description = "Role to ping"] ping: Option<Role>,
    #[description = "Send as plain text instead of an embed"] plain: Option<bool>,
) -> Result<(), Error> {
    let body = message.replace("\\n", "\n");
    let mut builder = CreateMessage::new();

    if let Some(role) = &ping {
        builder = builder.content(format!("<@&{}>", role.id.get()));
    }
    if plain.unwrap_or(false) {
        builder = builder.content(match &ping {
            Some(role) => format!("<@&{}>\n{body}", role.id.get()),
            None => body.clone(),
        });
    } else {
        builder = builder.embed(
            embed::titled(
                title.unwrap_or_else(|| "📣 Announcement".to_string()),
                embed::BRAND,
            )
            .description(body)
            .footer(embed::footer(format!("Posted by {}", ctx.author().tag())))
            .timestamp(serenity::Timestamp::now()),
        );
    }

    channel.id.send_message(ctx.http(), builder).await?;

    ctx.send(
        poise::CreateReply::default()
            .embed(embed::ok(format!("Posted in <#{}>.", channel.id.get())))
            .ephemeral(true),
    )
    .await?;
    Ok(())
}

// ===========================================================================
// Reminders
// ===========================================================================

/// Have the bot remind you about something.
#[poise::command(
    slash_command,
    prefix_command,
    category = "Utility",
    subcommands("remind_me", "remind_list", "remind_cancel")
)]
pub async fn remind(_ctx: Ctx<'_>) -> Result<(), Error> {
    Ok(())
}

/// Set a reminder.
#[poise::command(slash_command, rename = "me")]
pub async fn remind_me(
    ctx: Ctx<'_>,
    #[description = "When: 10m, 2h, 3d, or 2026-03-01 18:00 UTC"] when: String,
    #[description = "What to remind you about"] about: String,
    #[description = "Send it by DM instead of in this channel"] dm: Option<bool>,
    #[description = "Repeat every, e.g. 24h"] every: Option<String>,
) -> Result<(), Error> {
    let Some(remind_at) = time::parse_when(&when) else {
        ctx.send(
            poise::CreateReply::default()
                .embed(embed::err(
                    "I could not read that time. Try `10m`, `2h`, `3d`, or `2026-03-01 18:00`.",
                ))
                .ephemeral(true),
        )
        .await?;
        return Ok(());
    };
    if remind_at < Utc::now() {
        ctx.send(
            poise::CreateReply::default()
                .embed(embed::err("That time has already passed."))
                .ephemeral(true),
        )
        .await?;
        return Ok(());
    }

    let repeat = every
        .as_deref()
        .and_then(time::parse_duration)
        .map(|d| d.as_secs() as i64)
        .filter(|s| *s >= 300)
        .unwrap_or(0);

    let id = sqlx::query(
        "INSERT INTO reminders (guild_id, channel_id, user_id, content, remind_at, via_dm, repeat_secs)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(ctx.guild_id().map(|g| g.get() as i64).unwrap_or(0))
    .bind(ctx.channel_id().get() as i64)
    .bind(ctx.author().id.get() as i64)
    .bind(text::truncate(&about, 1000))
    .bind(time::to_sql(remind_at))
    .bind(i64::from(dm.unwrap_or(false)))
    .bind(repeat)
    .execute(&ctx.data().db)
    .await?
    .last_insert_rowid();

    ctx.send(
        poise::CreateReply::default().embed(embed::ok(format!(
            "Reminder **#{id}** set for {}.",
            time::full_and_relative(remind_at)
        ))),
    )
    .await?;
    Ok(())
}

/// Your pending reminders.
#[poise::command(slash_command, rename = "list")]
pub async fn remind_list(ctx: Ctx<'_>) -> Result<(), Error> {
    let rows: Vec<(i64, String, String, i64)> = sqlx::query_as(
        "SELECT id, content, remind_at, repeat_secs FROM reminders
         WHERE user_id = ? ORDER BY remind_at ASC LIMIT 20",
    )
    .bind(ctx.author().id.get() as i64)
    .fetch_all(&ctx.data().db)
    .await?;

    let body = if rows.is_empty() {
        "You have no reminders set.".to_string()
    } else {
        rows.iter()
            .map(|(id, content, remind_at, repeat)| {
                let when = time::from_sql(remind_at)
                    .map(|t| time::discord_ts(t, 'R'))
                    .unwrap_or_default();
                format!(
                    "**#{id}** {when}{}\n└ {}",
                    if *repeat > 0 {
                        format!(" · repeats every {}", time::humanize(*repeat))
                    } else {
                        String::new()
                    },
                    text::truncate(&text::escape_md(content), 120)
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    ctx.send(
        poise::CreateReply::default()
            .embed(embed::titled("⏳ Your reminders", embed::INFO).description(body))
            .ephemeral(true),
    )
    .await?;
    Ok(())
}

/// Cancel a reminder.
#[poise::command(slash_command, rename = "cancel")]
pub async fn remind_cancel(
    ctx: Ctx<'_>,
    #[description = "Reminder number"] id: i64,
) -> Result<(), Error> {
    let result = sqlx::query("DELETE FROM reminders WHERE id = ? AND user_id = ?")
        .bind(id)
        .bind(ctx.author().id.get() as i64)
        .execute(&ctx.data().db)
        .await?;
    let body = if result.rows_affected() == 0 {
        embed::err("No reminder of yours has that number.")
    } else {
        embed::ok("Reminder cancelled.")
    };
    ctx.send(poise::CreateReply::default().embed(body).ephemeral(true))
        .await?;
    Ok(())
}

// ===========================================================================
// Custom commands
// ===========================================================================

/// Simple text commands anyone can trigger with the prefix.
#[poise::command(
    slash_command,
    guild_only,
    category = "Automation",
    subcommands("cc_add", "cc_remove", "cc_list"),
    check = "perms::admin_only"
)]
pub async fn customcommand(_ctx: Ctx<'_>) -> Result<(), Error> {
    Ok(())
}

#[poise::command(slash_command, rename = "add", guild_only)]
pub async fn cc_add(
    ctx: Ctx<'_>,
    #[description = "Command name, without the prefix"] name: String,
    #[description = "What it replies with. Use \\n for new lines."] response: String,
    #[description = "Reply as an embed"] as_embed: Option<bool>,
    #[description = "Role required to use it"] required_role: Option<Role>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let name = name.trim().to_lowercase();

    if name.is_empty() || name.contains(char::is_whitespace) {
        ctx.send(
            poise::CreateReply::default()
                .embed(embed::err("The name must be a single word.")),
        )
        .await?;
        return Ok(());
    }

    sqlx::query(
        "INSERT INTO custom_commands (guild_id, name, response, as_embed, required_role, created_by)
         VALUES (?, ?, ?, ?, ?, ?)
         ON CONFLICT(guild_id, name) DO UPDATE SET
            response = excluded.response,
            as_embed = excluded.as_embed,
            required_role = excluded.required_role",
    )
    .bind(guild_id)
    .bind(&name)
    .bind(response.replace("\\n", "\n"))
    .bind(i64::from(as_embed.unwrap_or(false)))
    .bind(required_role.as_ref().map(|r| r.id.get() as i64))
    .bind(ctx.author().id.get() as i64)
    .execute(&ctx.data().db)
    .await?;

    let prefix = db::guild_config(&ctx.data().db, guild_id)
        .await
        .map(|c| c.prefix)
        .unwrap_or_else(|_| "!".to_string());

    ctx.send(poise::CreateReply::default().embed(embed::ok(format!(
        "Saved. Members can now use `{prefix}{name}`."
    ))))
    .await?;
    Ok(())
}

#[poise::command(slash_command, rename = "remove", guild_only)]
pub async fn cc_remove(
    ctx: Ctx<'_>,
    #[description = "Command name"] name: String,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let result = sqlx::query("DELETE FROM custom_commands WHERE guild_id = ? AND name = ?")
        .bind(guild_id)
        .bind(name.trim().to_lowercase())
        .execute(&ctx.data().db)
        .await?;
    let body = if result.rows_affected() == 0 {
        embed::err("No custom command with that name.")
    } else {
        embed::ok("Deleted.")
    };
    ctx.send(poise::CreateReply::default().embed(body)).await?;
    Ok(())
}

#[poise::command(slash_command, rename = "list", guild_only)]
pub async fn cc_list(ctx: Ctx<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let rows: Vec<(String, i64, Option<i64>)> = sqlx::query_as(
        "SELECT name, uses, required_role FROM custom_commands WHERE guild_id = ? ORDER BY name",
    )
    .bind(guild_id)
    .fetch_all(&ctx.data().db)
    .await?;

    let body = if rows.is_empty() {
        "No custom commands yet.".to_string()
    } else {
        rows.iter()
            .map(|(name, uses, role)| {
                format!(
                    "`{name}` — {uses} use(s){}",
                    role.map(|r| format!(" · needs <@&{r}>")).unwrap_or_default()
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    ctx.send(
        poise::CreateReply::default()
            .embed(embed::titled("Custom commands", embed::INFO).description(body)),
    )
    .await?;
    Ok(())
}

/// Try to answer a prefix message with a custom command. Returns true if handled.
pub async fn try_custom_command(
    sctx: &serenity::Context,
    data: &Data,
    message: &serenity::Message,
) -> Result<bool, Error> {
    let Some(guild_id) = message.guild_id else {
        return Ok(false);
    };
    let guild_i64 = guild_id.get() as i64;

    let prefix = db::guild_config(&data.db, guild_i64)
        .await
        .map(|c| c.prefix)
        .unwrap_or_else(|_| data.config.prefix.clone());

    let Some(rest) = message.content.strip_prefix(&prefix) else {
        return Ok(false);
    };
    let name = rest.split_whitespace().next().unwrap_or("").to_lowercase();
    if name.is_empty() {
        return Ok(false);
    }

    let found: Option<(String, i64, Option<i64>)> = sqlx::query_as(
        "SELECT response, as_embed, required_role FROM custom_commands
         WHERE guild_id = ? AND name = ?",
    )
    .bind(guild_i64)
    .bind(&name)
    .fetch_optional(&data.db)
    .await?;

    let Some((response, as_embed, required_role)) = found else {
        return Ok(false);
    };

    if let Some(role) = required_role {
        let allowed = message
            .member
            .as_ref()
            .map(|m| m.roles.iter().any(|r| r.get() as i64 == role))
            .unwrap_or(false);
        if !allowed {
            return Ok(true); // silently ignore rather than nag
        }
    }

    let vars = vec![
        ("mention", format!("<@{}>", message.author.id.get())),
        ("user", message.author.name.clone()),
        (
            "server",
            crate::modules::moderation::guild_name(sctx, guild_id),
        ),
    ];
    let body = text::render(&response, &vars);

    let builder = if as_embed == 1 {
        CreateMessage::new().embed(embed::base().description(body))
    } else {
        CreateMessage::new().content(body)
    };
    let _ = message.channel_id.send_message(&sctx.http, builder).await;

    let _ = sqlx::query(
        "UPDATE custom_commands SET uses = uses + 1 WHERE guild_id = ? AND name = ?",
    )
    .bind(guild_i64)
    .bind(&name)
    .execute(&data.db)
    .await;

    Ok(true)
}

// ===========================================================================
// Workflows
// ===========================================================================

#[derive(Debug, poise::ChoiceParameter)]
pub enum Trigger {
    #[name = "a member joins"]
    MemberJoin,
    #[name = "a member leaves"]
    MemberLeave,
    #[name = "a member levels up"]
    LevelUp,
    #[name = "a member reaches a warning count"]
    WarnCount,
    #[name = "a ticket is opened"]
    TicketOpen,
    #[name = "a ticket is closed"]
    TicketClose,
    #[name = "a member is verified"]
    Verified,
}

impl Trigger {
    fn key(&self) -> &'static str {
        match self {
            Trigger::MemberJoin => "member_join",
            Trigger::MemberLeave => "member_leave",
            Trigger::LevelUp => "level_up",
            Trigger::WarnCount => "warn_count",
            Trigger::TicketOpen => "ticket_open",
            Trigger::TicketClose => "ticket_close",
            Trigger::Verified => "verified",
        }
    }
}

#[derive(Debug, poise::ChoiceParameter)]
pub enum WorkflowAction {
    #[name = "add a role"]
    AddRole,
    #[name = "remove a role"]
    RemoveRole,
    #[name = "send a message to a channel"]
    SendMessage,
    #[name = "DM the member"]
    Dm,
    #[name = "give XP"]
    GiveXp,
    #[name = "write to the mod log"]
    Log,
}

impl WorkflowAction {
    fn key(&self) -> &'static str {
        match self {
            WorkflowAction::AddRole => "add_role",
            WorkflowAction::RemoveRole => "remove_role",
            WorkflowAction::SendMessage => "send_message",
            WorkflowAction::Dm => "dm",
            WorkflowAction::GiveXp => "give_xp",
            WorkflowAction::Log => "log",
        }
    }
}

/// Event-driven automations: when X happens, do Y.
#[poise::command(
    slash_command,
    guild_only,
    category = "Automation",
    subcommands("workflow_add", "workflow_list", "workflow_remove", "workflow_toggle"),
    check = "perms::admin_only"
)]
pub async fn workflow(_ctx: Ctx<'_>) -> Result<(), Error> {
    Ok(())
}

/// Create a workflow. Run it again with the same name to add another action.
#[poise::command(slash_command, rename = "add", guild_only)]
pub async fn workflow_add(
    ctx: Ctx<'_>,
    #[description = "Name for this workflow"] name: String,
    #[description = "What sets it off"] trigger: Trigger,
    #[description = "What to do"] action: WorkflowAction,
    #[description = "Role for add/remove role actions"] role: Option<Role>,
    #[description = "Channel for the send-message action"] channel: Option<GuildChannel>,
    #[description = "Text for message, DM and log actions. {mention} {user} {level} work."]
    message: Option<String>,
    #[description = "XP amount for the give-XP action"] xp: Option<i64>,
    #[description = "Trigger value, e.g. the level or warning count"] trigger_value: Option<String>,
    #[description = "Wait this long before acting, e.g. 10m"] delay: Option<String>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let name = name.trim().to_string();

    // Build the action, rejecting incomplete combinations up front.
    let action_json = match action {
        WorkflowAction::AddRole | WorkflowAction::RemoveRole => {
            let Some(role) = role else {
                ctx.send(
                    poise::CreateReply::default()
                        .embed(embed::err("That action needs a role.")),
                )
                .await?;
                return Ok(());
            };
            serde_json::json!({ "type": action.key(), "role": role.id.get() })
        }
        WorkflowAction::SendMessage => {
            let (Some(channel), Some(message)) = (channel, message.clone()) else {
                ctx.send(
                    poise::CreateReply::default()
                        .embed(embed::err("That action needs a channel and a message.")),
                )
                .await?;
                return Ok(());
            };
            serde_json::json!({
                "type": action.key(),
                "channel": channel.id.get(),
                "text": message.replace("\\n", "\n"),
            })
        }
        WorkflowAction::Dm | WorkflowAction::Log => {
            let Some(message) = message.clone() else {
                ctx.send(
                    poise::CreateReply::default()
                        .embed(embed::err("That action needs a message.")),
                )
                .await?;
                return Ok(());
            };
            serde_json::json!({ "type": action.key(), "text": message.replace("\\n", "\n") })
        }
        WorkflowAction::GiveXp => {
            let Some(xp) = xp else {
                ctx.send(
                    poise::CreateReply::default()
                        .embed(embed::err("That action needs an XP amount.")),
                )
                .await?;
                return Ok(());
            };
            serde_json::json!({ "type": action.key(), "amount": xp })
        }
    };

    let delay_secs = delay
        .as_deref()
        .and_then(time::parse_duration)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    // Append to the existing workflow if the name is already in use.
    let existing: Option<String> =
        sqlx::query_scalar("SELECT actions FROM workflows WHERE guild_id = ? AND name = ?")
            .bind(guild_id)
            .bind(&name)
            .fetch_optional(&ctx.data().db)
            .await?;

    let mut actions: Vec<serde_json::Value> = existing
        .as_deref()
        .and_then(|raw| serde_json::from_str(raw).ok())
        .unwrap_or_default();
    actions.push(action_json);

    sqlx::query(
        "INSERT INTO workflows
            (guild_id, name, trigger_kind, trigger_arg, actions, delay_secs, created_by)
         VALUES (?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(guild_id, name) DO UPDATE SET
            trigger_kind = excluded.trigger_kind,
            trigger_arg = excluded.trigger_arg,
            actions = excluded.actions,
            delay_secs = excluded.delay_secs",
    )
    .bind(guild_id)
    .bind(&name)
    .bind(trigger.key())
    .bind(trigger_value.unwrap_or_default())
    .bind(serde_json::to_string(&actions)?)
    .bind(delay_secs)
    .bind(ctx.author().id.get() as i64)
    .execute(&ctx.data().db)
    .await?;

    ctx.send(
        poise::CreateReply::default().embed(
            embed::ok(format!(
                "Workflow **{name}** saved with **{}** action(s).",
                actions.len()
            ))
            .footer(embed::footer(if delay_secs > 0 {
                format!("Runs {} after the trigger", time::humanize(delay_secs))
            } else {
                "Runs immediately".to_string()
            })),
        ),
    )
    .await?;
    Ok(())
}

#[poise::command(slash_command, rename = "list", guild_only)]
pub async fn workflow_list(ctx: Ctx<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let rows: Vec<(String, String, String, String, i64, i64, i64)> = sqlx::query_as(
        "SELECT name, trigger_kind, trigger_arg, actions, delay_secs, enabled, runs
         FROM workflows WHERE guild_id = ? ORDER BY name",
    )
    .bind(guild_id)
    .fetch_all(&ctx.data().db)
    .await?;

    let body = if rows.is_empty() {
        "No workflows yet. Create one with `/workflow add`.".to_string()
    } else {
        rows.iter()
            .map(|(name, trigger, arg, actions, delay, enabled, runs)| {
                let count = serde_json::from_str::<Vec<serde_json::Value>>(actions)
                    .map(|a| a.len())
                    .unwrap_or(0);
                format!(
                    "{} **{name}** · on `{trigger}`{} · {count} action(s){} · ran {runs}x",
                    if *enabled == 1 { "🟢" } else { "🔴" },
                    if arg.is_empty() {
                        String::new()
                    } else {
                        format!(" = `{arg}`")
                    },
                    if *delay > 0 {
                        format!(" · after {}", time::humanize(*delay))
                    } else {
                        String::new()
                    }
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    ctx.send(
        poise::CreateReply::default()
            .embed(embed::titled("🔧 Workflows", embed::INFO).description(body)),
    )
    .await?;
    Ok(())
}

#[poise::command(slash_command, rename = "remove", guild_only)]
pub async fn workflow_remove(
    ctx: Ctx<'_>,
    #[description = "Workflow name"] name: String,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let result = sqlx::query("DELETE FROM workflows WHERE guild_id = ? AND name = ?")
        .bind(guild_id)
        .bind(name.trim())
        .execute(&ctx.data().db)
        .await?;
    let body = if result.rows_affected() == 0 {
        embed::err("No workflow with that name.")
    } else {
        embed::ok("Workflow deleted.")
    };
    ctx.send(poise::CreateReply::default().embed(body)).await?;
    Ok(())
}

#[poise::command(slash_command, rename = "toggle", guild_only)]
pub async fn workflow_toggle(
    ctx: Ctx<'_>,
    #[description = "Workflow name"] name: String,
    #[description = "Enabled?"] enabled: bool,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    sqlx::query("UPDATE workflows SET enabled = ? WHERE guild_id = ? AND name = ?")
        .bind(i64::from(enabled))
        .bind(guild_id)
        .bind(name.trim())
        .execute(&ctx.data().db)
        .await?;
    ctx.send(poise::CreateReply::default().embed(embed::ok(format!(
        "Workflow **{}** is now {}.",
        name.trim(),
        if enabled { "enabled" } else { "disabled" }
    ))))
    .await?;
    Ok(())
}

/// Context handed to a workflow run.
#[derive(Debug, Clone, Default)]
pub struct WorkflowContext {
    pub user_id: u64,
    pub channel_id: u64,
    /// Numeric value the trigger carries, e.g. the new level or warning count.
    pub value: Option<i64>,
    pub extra: Vec<(String, String)>,
}

/// Fire every workflow listening for `trigger`.
pub async fn fire(
    sctx: &serenity::Context,
    data: &Data,
    guild_id: GuildId,
    trigger: &str,
    context: WorkflowContext,
) {
    let guild_i64 = guild_id.get() as i64;
    let rows: Vec<(i64, String, String, String, i64)> = sqlx::query_as(
        "SELECT id, trigger_arg, actions, name, delay_secs FROM workflows
         WHERE guild_id = ? AND trigger_kind = ? AND enabled = 1",
    )
    .bind(guild_i64)
    .bind(trigger)
    .fetch_all(&data.db)
    .await
    .unwrap_or_default();

    for (workflow_id, trigger_arg, actions, name, delay_secs) in rows {
        // A trigger value of "10" means "only when the value is at least 10".
        if let Ok(required) = trigger_arg.trim().parse::<i64>() {
            match context.value {
                Some(value) if value >= required => {}
                _ => continue,
            }
        }

        if delay_secs > 0 {
            let run_at = time::to_sql(Utc::now() + chrono::Duration::seconds(delay_secs));
            let _ = sqlx::query(
                "INSERT INTO workflow_queue (workflow_id, guild_id, user_id, channel_id, context, run_at)
                 VALUES (?, ?, ?, ?, ?, ?)",
            )
            .bind(workflow_id)
            .bind(guild_i64)
            .bind(context.user_id as i64)
            .bind(context.channel_id as i64)
            .bind(serde_json::to_string(&context.value).unwrap_or_else(|_| "null".into()))
            .bind(run_at)
            .execute(&data.db)
            .await;
            continue;
        }

        run_actions(sctx, data, guild_id, &actions, &context).await;
        let _ = sqlx::query("UPDATE workflows SET runs = runs + 1 WHERE id = ?")
            .bind(workflow_id)
            .execute(&data.db)
            .await;
        tracing::debug!(guild = guild_i64, workflow = %name, "workflow fired");
    }
}

async fn run_actions(
    sctx: &serenity::Context,
    data: &Data,
    guild_id: GuildId,
    actions_json: &str,
    context: &WorkflowContext,
) {
    let Ok(actions) = serde_json::from_str::<Vec<serde_json::Value>>(actions_json) else {
        return;
    };
    let guild_i64 = guild_id.get() as i64;
    let user_id = UserId::new(context.user_id.max(1));

    let mut vars = vec![
        ("mention".to_string(), format!("<@{}>", context.user_id)),
        ("user".to_string(), format!("<@{}>", context.user_id)),
        (
            "server".to_string(),
            crate::modules::moderation::guild_name(sctx, guild_id),
        ),
        (
            "value".to_string(),
            context.value.map(|v| v.to_string()).unwrap_or_default(),
        ),
        (
            "level".to_string(),
            context.value.map(|v| v.to_string()).unwrap_or_default(),
        ),
    ];
    vars.extend(context.extra.iter().cloned());
    let var_refs: Vec<(&str, String)> = vars
        .iter()
        .map(|(key, value)| (key.as_str(), value.clone()))
        .collect();

    for action in actions {
        let kind = action.get("type").and_then(|v| v.as_str()).unwrap_or("");
        match kind {
            "add_role" | "remove_role" => {
                let Some(role) = action.get("role").and_then(|v| v.as_u64()) else {
                    continue;
                };
                let role = RoleId::new(role);
                let _ = if kind == "add_role" {
                    crate::compat::add_role(&sctx.http, guild_id, user_id, role).await
                } else {
                    crate::compat::remove_role(&sctx.http, guild_id, user_id, role).await
                };
            }
            "send_message" => {
                let Some(channel) = action.get("channel").and_then(|v| v.as_u64()) else {
                    continue;
                };
                let body = text::render(
                    action.get("text").and_then(|v| v.as_str()).unwrap_or(""),
                    &var_refs,
                );
                let _ = ChannelId::new(channel)
                    .send_message(&sctx.http, CreateMessage::new().content(body))
                    .await;
            }
            "dm" => {
                let body = text::render(
                    action.get("text").and_then(|v| v.as_str()).unwrap_or(""),
                    &var_refs,
                );
                if let Ok(dm) = user_id.create_dm_channel(sctx).await {
                    let _ = dm
                        .send_message(
                            &sctx.http,
                            CreateMessage::new().embed(embed::base().description(body)),
                        )
                        .await;
                }
            }
            "give_xp" => {
                let amount = action.get("amount").and_then(|v| v.as_i64()).unwrap_or(0);
                if amount != 0 {
                    let _ = crate::modules::leveling::add_xp(
                        &data.db,
                        guild_i64,
                        context.user_id as i64,
                        amount,
                    )
                    .await;
                }
            }
            "log" => {
                let body = text::render(
                    action.get("text").and_then(|v| v.as_str()).unwrap_or(""),
                    &var_refs,
                );
                crate::logs::send(
                    sctx,
                    &data.db,
                    guild_i64,
                    crate::logs::Log::Mod,
                    embed::titled("🔧 Workflow", embed::INFO).description(body),
                )
                .await;
            }
            _ => {}
        }
    }
}

/// Run queued delayed workflow actions whose time has come.
pub async fn drain_queue(sctx: &serenity::Context, data: &Data) {
    let rows: Vec<(i64, i64, i64, i64, i64, String)> = sqlx::query_as(
        "SELECT q.id, q.workflow_id, q.guild_id, q.user_id, q.channel_id, w.actions
         FROM workflow_queue q
         JOIN workflows w ON w.id = q.workflow_id
         WHERE q.run_at <= ?",
    )
    .bind(time::sql_now())
    .fetch_all(&data.db)
    .await
    .unwrap_or_default();

    for (queue_id, workflow_id, guild_i64, user_i64, channel_i64, actions) in rows {
        let context = WorkflowContext {
            user_id: user_i64 as u64,
            channel_id: channel_i64 as u64,
            value: None,
            extra: Vec::new(),
        };
        run_actions(
            sctx,
            data,
            GuildId::new(guild_i64 as u64),
            &actions,
            &context,
        )
        .await;

        let _ = sqlx::query("UPDATE workflows SET runs = runs + 1 WHERE id = ?")
            .bind(workflow_id)
            .execute(&data.db)
            .await;
        let _ = sqlx::query("DELETE FROM workflow_queue WHERE id = ?")
            .bind(queue_id)
            .execute(&data.db)
            .await;
    }
}

// ===========================================================================
// Webhook integrations
// ===========================================================================

/// Forward bot events to an external webhook.
#[poise::command(
    slash_command,
    guild_only,
    category = "Automation",
    subcommands("webhook_add", "webhook_list", "webhook_remove", "webhook_test"),
    check = "perms::admin_only"
)]
pub async fn webhook(_ctx: Ctx<'_>) -> Result<(), Error> {
    Ok(())
}

/// Register a webhook URL and the events it receives.
#[poise::command(slash_command, rename = "add", guild_only, ephemeral)]
pub async fn webhook_add(
    ctx: Ctx<'_>,
    #[description = "A name for this integration"] name: String,
    #[description = "The https webhook URL"] url: String,
    #[description = "Events, comma separated: join,leave,mod,ticket,levelup,automod"]
    events: Option<String>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;

    if !url.starts_with("https://") {
        ctx.send(
            poise::CreateReply::default()
                .embed(embed::err("The URL has to start with `https://`."))
                .ephemeral(true),
        )
        .await?;
        return Ok(());
    }

    sqlx::query("INSERT INTO webhook_targets (guild_id, name, url, events) VALUES (?, ?, ?, ?)")
        .bind(guild_id)
        .bind(name.trim())
        .bind(url.trim())
        .bind(events.unwrap_or_else(|| "mod,join,leave".to_string()))
        .execute(&ctx.data().db)
        .await?;

    ctx.send(
        poise::CreateReply::default()
            .embed(embed::ok(
                "Webhook registered. The URL is stored but never shown again.",
            ))
            .ephemeral(true),
    )
    .await?;
    Ok(())
}

#[poise::command(slash_command, rename = "list", guild_only, ephemeral)]
pub async fn webhook_list(ctx: Ctx<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let rows: Vec<(i64, String, String, i64)> = sqlx::query_as(
        "SELECT id, name, events, enabled FROM webhook_targets WHERE guild_id = ? ORDER BY id",
    )
    .bind(guild_id)
    .fetch_all(&ctx.data().db)
    .await?;

    let body = if rows.is_empty() {
        "No webhooks registered.".to_string()
    } else {
        rows.iter()
            .map(|(id, name, events, enabled)| {
                format!(
                    "{} **#{id}** {name} · events: `{events}`",
                    if *enabled == 1 { "🟢" } else { "🔴" }
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    // URLs are deliberately omitted; they are secrets.
    ctx.send(
        poise::CreateReply::default()
            .embed(
                embed::titled("Webhook integrations", embed::INFO)
                    .description(body)
                    .footer(embed::footer("URLs are hidden on purpose")),
            )
            .ephemeral(true),
    )
    .await?;
    Ok(())
}

#[poise::command(slash_command, rename = "remove", guild_only, ephemeral)]
pub async fn webhook_remove(
    ctx: Ctx<'_>,
    #[description = "Webhook number"] id: i64,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let result = sqlx::query("DELETE FROM webhook_targets WHERE id = ? AND guild_id = ?")
        .bind(id)
        .bind(guild_id)
        .execute(&ctx.data().db)
        .await?;
    let body = if result.rows_affected() == 0 {
        embed::err("No webhook with that number.")
    } else {
        embed::ok("Removed.")
    };
    ctx.send(poise::CreateReply::default().embed(body).ephemeral(true))
        .await?;
    Ok(())
}

#[poise::command(slash_command, rename = "test", guild_only, ephemeral)]
pub async fn webhook_test(
    ctx: Ctx<'_>,
    #[description = "Webhook number"] id: i64,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let url: Option<String> =
        sqlx::query_scalar("SELECT url FROM webhook_targets WHERE id = ? AND guild_id = ?")
            .bind(id)
            .bind(guild_id)
            .fetch_optional(&ctx.data().db)
            .await?;

    let Some(url) = url else {
        ctx.send(
            poise::CreateReply::default()
                .embed(embed::err("No webhook with that number."))
                .ephemeral(true),
        )
        .await?;
        return Ok(());
    };

    ctx.defer_ephemeral().await?;
    let result = ctx
        .data()
        .http
        .post(&url)
        .json(&serde_json::json!({
            "event": "test",
            "guild_id": guild_id.to_string(),
            "content": "Test delivery from the MrTechTrivia bot.",
        }))
        .send()
        .await;

    let body = match result {
        Ok(response) if response.status().is_success() => {
            embed::ok(format!("Delivered. The endpoint replied {}.", response.status()))
        }
        Ok(response) => embed::warn(format!("The endpoint replied {}.", response.status())),
        Err(why) => embed::err(format!("Delivery failed: {why}")),
    };
    ctx.send(poise::CreateReply::default().embed(body).ephemeral(true))
        .await?;
    Ok(())
}

/// Deliver an event to every webhook subscribed to it.
pub async fn notify_webhooks(data: &Data, guild_id: i64, event: &str, payload: serde_json::Value) {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT url, events FROM webhook_targets WHERE guild_id = ? AND enabled = 1",
    )
    .bind(guild_id)
    .fetch_all(&data.db)
    .await
    .unwrap_or_default();

    for (url, events) in rows {
        let subscribed = events
            .split(',')
            .map(|e| e.trim())
            .any(|e| e == event || e == "*");
        if !subscribed {
            continue;
        }
        let body = serde_json::json!({
            "event": event,
            "guild_id": guild_id.to_string(),
            "data": payload,
        });
        let _ = data.http.post(&url).json(&body).send().await;
    }
}

// ===========================================================================
// Weekly report
// ===========================================================================

/// Automatic weekly activity report.
#[poise::command(
    slash_command,
    guild_only,
    category = "Automation",
    subcommands("report_setup", "report_off", "report_now"),
    check = "perms::admin_only"
)]
pub async fn weeklyreport(_ctx: Ctx<'_>) -> Result<(), Error> {
    Ok(())
}

#[poise::command(slash_command, rename = "setup", guild_only)]
pub async fn report_setup(
    ctx: Ctx<'_>,
    #[description = "Channel for the report"] channel: GuildChannel,
    #[description = "Day of the week (1 = Monday)"]
    #[min = 1]
    #[max = 7]
    weekday: Option<i64>,
    #[description = "Hour of the day, UTC"]
    #[min = 0]
    #[max = 23]
    hour: Option<i64>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    sqlx::query(
        "INSERT INTO weekly_reports (guild_id, channel_id, weekday, hour, enabled)
         VALUES (?, ?, ?, ?, 1)
         ON CONFLICT(guild_id) DO UPDATE SET
            channel_id = excluded.channel_id,
            weekday = excluded.weekday,
            hour = excluded.hour,
            enabled = 1",
    )
    .bind(guild_id)
    .bind(channel.id.get() as i64)
    .bind(weekday.unwrap_or(1))
    .bind(hour.unwrap_or(12))
    .execute(&ctx.data().db)
    .await?;

    ctx.send(poise::CreateReply::default().embed(embed::ok(format!(
        "Weekly reports will be posted in <#{}> on day **{}** at **{:02}:00 UTC**.",
        channel.id.get(),
        weekday.unwrap_or(1),
        hour.unwrap_or(12)
    ))))
    .await?;
    Ok(())
}

#[poise::command(slash_command, rename = "off", guild_only)]
pub async fn report_off(ctx: Ctx<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    sqlx::query("UPDATE weekly_reports SET enabled = 0 WHERE guild_id = ?")
        .bind(guild_id)
        .execute(&ctx.data().db)
        .await?;
    ctx.send(poise::CreateReply::default().embed(embed::ok("Weekly reports off.")))
        .await?;
    Ok(())
}

#[poise::command(slash_command, rename = "now", guild_only)]
pub async fn report_now(ctx: Ctx<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap();
    ctx.defer().await?;
    let report = build_weekly_report(ctx.data(), guild_id.get() as i64).await;
    ctx.send(poise::CreateReply::default().embed(report)).await?;
    Ok(())
}

/// Build the weekly summary embed.
pub async fn build_weekly_report(data: &Data, guild_id: i64) -> serenity::CreateEmbed {
    let db = &data.db;
    let since = time::day_offset(7);

    let totals: Option<(i64, i64, i64, i64, i64)> = sqlx::query_as(
        "SELECT COALESCE(SUM(messages), 0), COALESCE(SUM(joins), 0), COALESCE(SUM(leaves), 0),
                COALESCE(SUM(voice_secs), 0), COALESCE(SUM(mod_actions), 0)
         FROM stats_daily WHERE guild_id = ? AND day >= ?",
    )
    .bind(guild_id)
    .bind(&since)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();

    let (messages, joins, leaves, voice, mod_actions) = totals.unwrap_or((0, 0, 0, 0, 0));

    let active: i64 = sqlx::query_scalar(
        "SELECT COUNT(DISTINCT user_id) FROM member_activity WHERE guild_id = ? AND day >= ?",
    )
    .bind(guild_id)
    .bind(&since)
    .fetch_one(db)
    .await
    .unwrap_or(0);

    let top: Vec<(i64, i64)> = sqlx::query_as(
        "SELECT user_id, SUM(messages) FROM member_activity
         WHERE guild_id = ? AND day >= ? GROUP BY user_id ORDER BY SUM(messages) DESC LIMIT 5",
    )
    .bind(guild_id)
    .bind(&since)
    .fetch_all(db)
    .await
    .unwrap_or_default();

    let channels: Vec<(i64, i64)> = sqlx::query_as(
        "SELECT channel_id, SUM(messages) FROM channel_activity
         WHERE guild_id = ? AND day >= ? GROUP BY channel_id ORDER BY SUM(messages) DESC LIMIT 5",
    )
    .bind(guild_id)
    .bind(&since)
    .fetch_all(db)
    .await
    .unwrap_or_default();

    let tickets: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM tickets WHERE guild_id = ? AND created_at >= ?",
    )
    .bind(guild_id)
    .bind(&since)
    .fetch_one(db)
    .await
    .unwrap_or(0);

    let top_body = if top.is_empty() {
        "-".to_string()
    } else {
        top.iter()
            .enumerate()
            .map(|(i, (user_id, count))| format!("**{}.** <@{user_id}> — {count}", i + 1))
            .collect::<Vec<_>>()
            .join("\n")
    };

    let channel_body = if channels.is_empty() {
        "-".to_string()
    } else {
        channels
            .iter()
            .map(|(channel_id, count)| format!("<#{channel_id}> — {count}"))
            .collect::<Vec<_>>()
            .join("\n")
    };

    embed::titled("📰 Weekly report", embed::BRAND)
        .description(format!("The last 7 days, as of {}.", time::today()))
        .field("Messages", messages.to_string(), true)
        .field("Active members", active.to_string(), true)
        .field("Voice time", time::humanize(voice), true)
        .field("Joined", joins.to_string(), true)
        .field("Left", leaves.to_string(), true)
        .field("Net", format!("{:+}", joins - leaves), true)
        .field("Moderation actions", mod_actions.to_string(), true)
        .field("Tickets opened", tickets.to_string(), true)
        .field("Most active members", top_body, false)
        .field("Busiest channels", channel_body, false)
}

/// Due scheduled messages, reminders and weekly reports.
pub async fn run_scheduler(sctx: &serenity::Context, data: &Data) {
    let now = time::sql_now();

    // ---- scheduled messages ---------------------------------------------
    let due: Vec<(i64, i64, String, i64, Option<String>, i64)> = sqlx::query_as(
        "SELECT id, channel_id, content, as_embed, title, interval_secs
         FROM scheduled_messages WHERE enabled = 1 AND next_run <= ?",
    )
    .bind(&now)
    .fetch_all(&data.db)
    .await
    .unwrap_or_default();

    for (id, channel_i64, content, as_embed, title, interval) in due {
        let channel = ChannelId::new(channel_i64 as u64);
        let builder = if as_embed == 1 {
            CreateMessage::new().embed(
                embed::titled(
                    title.unwrap_or_else(|| "📢 Announcement".to_string()),
                    embed::BRAND,
                )
                .description(content),
            )
        } else {
            CreateMessage::new().content(content)
        };
        let _ = channel.send_message(&sctx.http, builder).await;

        if interval > 0 {
            let next = time::to_sql(Utc::now() + chrono::Duration::seconds(interval));
            let _ = sqlx::query(
                "UPDATE scheduled_messages SET next_run = ?, runs = runs + 1 WHERE id = ?",
            )
            .bind(next)
            .bind(id)
            .execute(&data.db)
            .await;
        } else {
            let _ = sqlx::query("DELETE FROM scheduled_messages WHERE id = ?")
                .bind(id)
                .execute(&data.db)
                .await;
        }
    }

    // ---- reminders -------------------------------------------------------
    let reminders: Vec<(i64, i64, i64, String, i64, i64)> = sqlx::query_as(
        "SELECT id, channel_id, user_id, content, via_dm, repeat_secs
         FROM reminders WHERE remind_at <= ?",
    )
    .bind(&now)
    .fetch_all(&data.db)
    .await
    .unwrap_or_default();

    for (id, channel_i64, user_i64, content, via_dm, repeat) in reminders {
        let user_id = UserId::new(user_i64 as u64);
        let card = embed::titled("⏰ Reminder", embed::WARN).description(content.clone());

        let delivered = if via_dm == 1 {
            match user_id.create_dm_channel(sctx).await {
                Ok(dm) => dm
                    .send_message(&sctx.http, CreateMessage::new().embed(card.clone()))
                    .await
                    .is_ok(),
                Err(_) => false,
            }
        } else {
            false
        };

        if !delivered {
            let _ = ChannelId::new(channel_i64 as u64)
                .send_message(
                    &sctx.http,
                    CreateMessage::new()
                        .content(format!("<@{user_i64}>"))
                        .embed(card),
                )
                .await;
        }

        if repeat > 0 {
            let next = time::to_sql(Utc::now() + chrono::Duration::seconds(repeat));
            let _ = sqlx::query("UPDATE reminders SET remind_at = ? WHERE id = ?")
                .bind(next)
                .bind(id)
                .execute(&data.db)
                .await;
        } else {
            let _ = sqlx::query("DELETE FROM reminders WHERE id = ?")
                .bind(id)
                .execute(&data.db)
                .await;
        }
    }

    drain_queue(sctx, data).await;
}

/// Post weekly reports that are due. Called once an hour.
pub async fn run_weekly_reports(sctx: &serenity::Context, data: &Data) {
    use chrono::Datelike;
    let now = Utc::now();
    let weekday = now.weekday().number_from_monday() as i64;
    let hour = now.hour_i64();

    let rows: Vec<(i64, i64, Option<String>)> = sqlx::query_as(
        "SELECT guild_id, channel_id, last_sent FROM weekly_reports
         WHERE enabled = 1 AND weekday = ? AND hour = ?",
    )
    .bind(weekday)
    .bind(hour)
    .fetch_all(&data.db)
    .await
    .unwrap_or_default();

    for (guild_i64, channel_i64, last_sent) in rows {
        // Do not send twice in the same hour after a restart.
        if let Some(last) = last_sent.as_deref().and_then(time::from_sql) {
            if (now - last).num_hours() < 20 {
                continue;
            }
        }

        let report = build_weekly_report(data, guild_i64).await;
        let _ = ChannelId::new(channel_i64 as u64)
            .send_message(&sctx.http, CreateMessage::new().embed(report))
            .await;

        let _ = sqlx::query("UPDATE weekly_reports SET last_sent = ? WHERE guild_id = ?")
            .bind(time::sql_now())
            .bind(guild_i64)
            .execute(&data.db)
            .await;
    }
}

/// Small extension so the hour is an i64 without a cast at every call site.
trait HourExt {
    fn hour_i64(&self) -> i64;
}
impl HourExt for chrono::DateTime<Utc> {
    fn hour_i64(&self) -> i64 {
        use chrono::Timelike;
        self.hour() as i64
    }
}

/// Keep the member-count channel name up to date.
pub async fn update_count_channels(sctx: &serenity::Context, data: &Data, db: &Db) {
    let rows: Vec<(i64, String)> =
        sqlx::query_as("SELECT guild_id, value FROM kv_settings WHERE key = 'count_channel'")
            .fetch_all(db)
            .await
            .unwrap_or_default();

    for (guild_i64, raw) in rows {
        let Ok(channel_id) = raw.parse::<u64>() else {
            continue;
        };
        let guild_id = GuildId::new(guild_i64 as u64);
        let Some(count) = guild_id
            .to_guild_cached(&sctx.cache)
            .map(|g| g.member_count as i64)
        else {
            continue;
        };

        crate::modules::analytics::snapshot_member_count(db, guild_i64, count).await;

        let template = db::kv_get(db, guild_i64, "count_template")
            .await
            .unwrap_or_else(|| "Members: {count}".to_string());
        let name = text::render(&template, &[("count", count.to_string())]);

        let _ = ChannelId::new(channel_id)
            .edit(&sctx.http, serenity::EditChannel::new().name(name))
            .await;
    }
    let _ = data;
}

pub fn commands() -> Vec<poise::Command<Data, Error>> {
    vec![
        schedule(),
        announce(),
        remind(),
        customcommand(),
        workflow(),
        webhook(),
        weeklyreport(),
    ]
}
