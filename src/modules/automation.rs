//! Automation: announcements and reminders.

use chrono::Utc;
use poise::serenity_prelude as serenity;
use serenity::{ChannelId, CreateMessage, GuildChannel, Role, UserId};

use crate::state::{Ctx, Data, Error};
use crate::util::{embed, perms, text, time};

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


/// Deliver reminders that are due. Called every 30 seconds.
pub async fn run_reminders(sctx: &serenity::Context, data: &Data) {
    let now = time::sql_now();

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
}

pub fn commands() -> Vec<poise::Command<Data, Error>> {
    vec![announce(), remind()]
}
