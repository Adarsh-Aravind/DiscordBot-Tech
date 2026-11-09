//! Community events: scheduling, sign-ups and reminders.

use chrono::Utc;
use poise::serenity_prelude as serenity;
use serenity::{
    ChannelId, ComponentInteraction, CreateActionRow, CreateButton, CreateInteractionResponse,
    CreateInteractionResponseMessage, CreateMessage, EditMessage, GuildChannel, MessageId, UserId,
};

use crate::state::{Ctx, Data, Error};
use crate::util::{embed, perms, text, time};

fn components(event_id: i64, started: bool) -> Vec<CreateActionRow> {
    if started {
        return Vec::new();
    }
    vec![CreateActionRow::Buttons(vec![
        CreateButton::new(format!("event:{event_id}:going"))
            .label("Going")
            .style(serenity::ButtonStyle::Success)
            .emoji('✅'),
        CreateButton::new(format!("event:{event_id}:maybe"))
            .label("Maybe")
            .style(serenity::ButtonStyle::Secondary)
            .emoji('❔'),
        CreateButton::new(format!("event:{event_id}:no"))
            .label("Can't make it")
            .style(serenity::ButtonStyle::Danger)
            .emoji('❌'),
    ])]
}

async fn signups(data: &Data, event_id: i64) -> (Vec<i64>, Vec<i64>, Vec<i64>) {
    let rows: Vec<(i64, String)> =
        sqlx::query_as("SELECT user_id, status FROM event_signups WHERE event_id = ?")
            .bind(event_id)
            .fetch_all(&data.db)
            .await
            .unwrap_or_default();

    let mut going = Vec::new();
    let mut maybe = Vec::new();
    let mut no = Vec::new();
    for (user_id, status) in rows {
        match status.as_str() {
            "going" => going.push(user_id),
            "maybe" => maybe.push(user_id),
            _ => no.push(user_id),
        }
    }
    (going, maybe, no)
}

fn mention_list(ids: &[i64], limit: usize) -> String {
    if ids.is_empty() {
        return "-".to_string();
    }
    let shown = ids
        .iter()
        .take(limit)
        .map(|id| format!("<@{id}>"))
        .collect::<Vec<_>>()
        .join(", ");
    if ids.len() > limit {
        format!("{shown} … and {} more", ids.len() - limit)
    } else {
        shown
    }
}

#[allow(clippy::too_many_arguments)]
fn render(
    name: &str,
    description: &str,
    location: &str,
    host_id: i64,
    starts_at: chrono::DateTime<Utc>,
    max_attendees: i64,
    going: &[i64],
    maybe: &[i64],
    started: bool,
) -> serenity::CreateEmbed {
    let capacity = if max_attendees > 0 {
        format!("{} / {max_attendees}", going.len())
    } else {
        going.len().to_string()
    };

    embed::titled(
        format!("📅 {}", text::truncate(name, 240)),
        if started { embed::MUTED } else { embed::BRAND },
    )
    .description(if description.trim().is_empty() {
        "No description given.".to_string()
    } else {
        text::truncate(description, 1500)
    })
    .field("When", time::full_and_relative(starts_at), false)
    .field("Where", location.to_string(), true)
    .field("Host", format!("<@{host_id}>"), true)
    .field("Going", capacity, true)
    .field("Attending", mention_list(going, 20), false)
    .field("Maybe", mention_list(maybe, 10), false)
    .footer(embed::footer(if started {
        "This event has started"
    } else {
        "Press a button to RSVP"
    }))
}

/// Schedule and manage community events.
#[poise::command(
    slash_command,
    guild_only,
    category = "Community",
    subcommands("event_create", "event_list", "event_info", "event_cancel", "event_ping")
)]
pub async fn event(_ctx: Ctx<'_>) -> Result<(), Error> {
    Ok(())
}

/// Create an event with an RSVP panel.
#[poise::command(slash_command, rename = "create", guild_only, check = "perms::staff_only")]
pub async fn event_create(
    ctx: Ctx<'_>,
    #[description = "Event name"] name: String,
    #[description = "When: a delay like 2d, or a UTC time like 2026-03-01 19:00"] when: String,
    #[description = "What is it about?"] description: Option<String>,
    #[description = "Where: a voice channel name, a link, or anything"] location: Option<String>,
    #[description = "Maximum attendees (0 for unlimited)"]
    #[min = 0]
    max_attendees: Option<i64>,
    #[description = "Channel to post in, defaults to this one"] channel: Option<GuildChannel>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let target = channel.map(|c| c.id).unwrap_or_else(|| ctx.channel_id());

    let Some(starts_at) = time::parse_when(&when) else {
        ctx.send(poise::CreateReply::default().embed(embed::err(
            "I could not read that time. Use a delay like `2d`, or a UTC time like `2026-03-01 19:00`.",
        )))
        .await?;
        return Ok(());
    };
    if starts_at < Utc::now() {
        ctx.send(
            poise::CreateReply::default()
                .embed(embed::err("That time is in the past.")),
        )
        .await?;
        return Ok(());
    }

    let description = description.unwrap_or_default();
    let location = location.unwrap_or_else(|| "Discord".to_string());
    let max_attendees = max_attendees.unwrap_or(0);

    let event_id = sqlx::query(
        "INSERT INTO events
            (guild_id, name, description, location, host_id, channel_id, starts_at, max_attendees)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(guild_id)
    .bind(&name)
    .bind(&description)
    .bind(&location)
    .bind(ctx.author().id.get() as i64)
    .bind(target.get() as i64)
    .bind(time::to_sql(starts_at))
    .bind(max_attendees)
    .execute(&ctx.data().db)
    .await?
    .last_insert_rowid();

    let message = target
        .send_message(
            ctx.http(),
            CreateMessage::new()
                .embed(render(
                    &name,
                    &description,
                    &location,
                    ctx.author().id.get() as i64,
                    starts_at,
                    max_attendees,
                    &[],
                    &[],
                    false,
                ))
                .components(components(event_id, false)),
        )
        .await?;

    sqlx::query("UPDATE events SET message_id = ? WHERE id = ?")
        .bind(message.id.get() as i64)
        .bind(event_id)
        .execute(&ctx.data().db)
        .await?;

    ctx.send(
        poise::CreateReply::default()
            .embed(embed::ok(format!(
                "Event **#{event_id}** created for {}.",
                time::discord_ts(starts_at, 'F')
            )))
            .ephemeral(true),
    )
    .await?;
    Ok(())
}

/// Upcoming events.
#[poise::command(slash_command, rename = "list", guild_only)]
pub async fn event_list(ctx: Ctx<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let rows: Vec<(i64, String, String, i64)> = sqlx::query_as(
        "SELECT e.id, e.name, e.starts_at,
                (SELECT COUNT(*) FROM event_signups s WHERE s.event_id = e.id AND s.status = 'going')
         FROM events e WHERE e.guild_id = ? AND e.started = 0 ORDER BY e.starts_at ASC LIMIT 15",
    )
    .bind(guild_id)
    .fetch_all(&ctx.data().db)
    .await?;

    let body = if rows.is_empty() {
        "No upcoming events.".to_string()
    } else {
        rows.iter()
            .map(|(id, name, starts_at, going)| {
                let when = time::from_sql(starts_at)
                    .map(|t| time::full_and_relative(t))
                    .unwrap_or_default();
                format!("**#{id}** {name}\n└ {when} · {going} going")
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    ctx.send(
        poise::CreateReply::default()
            .embed(embed::titled("📅 Upcoming events", embed::BRAND).description(body)),
    )
    .await?;
    Ok(())
}

/// Details and the full attendee list.
#[poise::command(slash_command, rename = "info", guild_only)]
pub async fn event_info(
    ctx: Ctx<'_>,
    #[description = "Event number"] event_id: i64,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let found: Option<(String, String, String, i64, String, i64, i64)> = sqlx::query_as(
        "SELECT name, description, location, host_id, starts_at, max_attendees, started
         FROM events WHERE id = ? AND guild_id = ?",
    )
    .bind(event_id)
    .bind(guild_id)
    .fetch_optional(&ctx.data().db)
    .await?;

    let Some((name, description, location, host_id, starts_at, max_attendees, started)) = found
    else {
        ctx.send(poise::CreateReply::default().embed(embed::err("No event with that number.")))
            .await?;
        return Ok(());
    };

    let (going, maybe, _) = signups(ctx.data(), event_id).await;
    ctx.send(poise::CreateReply::default().embed(render(
        &name,
        &description,
        &location,
        host_id,
        time::from_sql(&starts_at).unwrap_or_else(Utc::now),
        max_attendees,
        &going,
        &maybe,
        started == 1,
    )))
    .await?;
    Ok(())
}

/// Cancel an event and tell everyone who signed up.
#[poise::command(slash_command, rename = "cancel", guild_only, check = "perms::staff_only")]
pub async fn event_cancel(
    ctx: Ctx<'_>,
    #[description = "Event number"] event_id: i64,
    #[description = "Why is it cancelled?"] reason: Option<String>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let found: Option<(String, i64, Option<i64>)> = sqlx::query_as(
        "SELECT name, channel_id, message_id FROM events WHERE id = ? AND guild_id = ?",
    )
    .bind(event_id)
    .bind(guild_id)
    .fetch_optional(&ctx.data().db)
    .await?;

    let Some((name, channel_id, message_id)) = found else {
        ctx.send(poise::CreateReply::default().embed(embed::err("No event with that number.")))
            .await?;
        return Ok(());
    };

    let (going, maybe, _) = signups(ctx.data(), event_id).await;

    sqlx::query("UPDATE events SET started = 1 WHERE id = ?")
        .bind(event_id)
        .execute(&ctx.data().db)
        .await?;

    let reason = reason.unwrap_or_else(|| "No reason given".to_string());

    if let Some(message_id) = message_id {
        let _ = ChannelId::new(channel_id as u64)
            .edit_message(
                ctx.http(),
                MessageId::new(message_id as u64),
                EditMessage::new()
                    .embed(
                        embed::titled(format!("🚫 {name} (cancelled)"), embed::DANGER)
                            .description(format!("**Reason:** {reason}")),
                    )
                    .components(Vec::new()),
            )
            .await;
    }

    // Tell everyone who said they were coming.
    let mut notified = 0;
    for user_id in going.iter().chain(maybe.iter()) {
        if let Ok(dm) = UserId::new(*user_id as u64)
            .create_dm_channel(ctx.serenity_context())
            .await
        {
            if dm
                .send_message(
                    ctx.http(),
                    CreateMessage::new().embed(
                        embed::titled("Event cancelled", embed::DANGER).description(format!(
                            "**{name}** has been cancelled.\n**Reason:** {reason}"
                        )),
                    ),
                )
                .await
                .is_ok()
            {
                notified += 1;
            }
        }
    }

    ctx.send(poise::CreateReply::default().embed(embed::ok(format!(
        "Event cancelled. Notified **{notified}** member(s)."
    ))))
    .await?;
    Ok(())
}

/// DM everyone attending an event.
#[poise::command(slash_command, rename = "ping", guild_only, check = "perms::staff_only")]
pub async fn event_ping(
    ctx: Ctx<'_>,
    #[description = "Event number"] event_id: i64,
    #[description = "Message to send"] message: String,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let name: Option<String> =
        sqlx::query_scalar("SELECT name FROM events WHERE id = ? AND guild_id = ?")
            .bind(event_id)
            .bind(guild_id)
            .fetch_optional(&ctx.data().db)
            .await?;

    let Some(name) = name else {
        ctx.send(poise::CreateReply::default().embed(embed::err("No event with that number.")))
            .await?;
        return Ok(());
    };

    ctx.defer_ephemeral().await?;
    let (going, _, _) = signups(ctx.data(), event_id).await;

    let mut sent = 0;
    for user_id in &going {
        if let Ok(dm) = UserId::new(*user_id as u64)
            .create_dm_channel(ctx.serenity_context())
            .await
        {
            if dm
                .send_message(
                    ctx.http(),
                    CreateMessage::new().embed(
                        embed::titled(format!("📅 {name}"), embed::INFO)
                            .description(text::truncate(&message, 2000)),
                    ),
                )
                .await
                .is_ok()
            {
                sent += 1;
            }
        }
    }

    ctx.send(
        poise::CreateReply::default()
            .embed(embed::ok(format!("Messaged **{sent}** attendee(s).")))
            .ephemeral(true),
    )
    .await?;
    Ok(())
}

/// Handle an RSVP button.
pub async fn handle_component(
    sctx: &serenity::Context,
    data: &Data,
    interaction: &ComponentInteraction,
) -> Result<bool, Error> {
    let Some(rest) = interaction.data.custom_id.strip_prefix("event:") else {
        return Ok(false);
    };
    let mut parts = rest.split(':');
    let event_id: i64 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let status = parts.next().unwrap_or("going");

    let found: Option<(String, String, String, i64, String, i64, i64)> = sqlx::query_as(
        "SELECT name, description, location, host_id, starts_at, max_attendees, started
         FROM events WHERE id = ?",
    )
    .bind(event_id)
    .fetch_optional(&data.db)
    .await?;

    let Some((name, description, location, host_id, starts_at, max_attendees, started)) = found
    else {
        return Ok(true);
    };

    if started == 1 {
        answer(sctx, interaction, "This event is no longer taking RSVPs.").await?;
        return Ok(true);
    }

    // Respect the attendee cap.
    if status == "going" && max_attendees > 0 {
        let current: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM event_signups WHERE event_id = ? AND status = 'going'",
        )
        .bind(event_id)
        .fetch_one(&data.db)
        .await?;
        let already_going: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM event_signups
             WHERE event_id = ? AND user_id = ? AND status = 'going'",
        )
        .bind(event_id)
        .bind(interaction.user.id.get() as i64)
        .fetch_one(&data.db)
        .await?;

        if current >= max_attendees && already_going == 0 {
            answer(
                sctx,
                interaction,
                "This event is full. You can still pick **Maybe** in case a spot opens up.",
            )
            .await?;
            return Ok(true);
        }
    }

    sqlx::query(
        "INSERT INTO event_signups (event_id, user_id, status) VALUES (?, ?, ?)
         ON CONFLICT(event_id, user_id) DO UPDATE SET status = excluded.status",
    )
    .bind(event_id)
    .bind(interaction.user.id.get() as i64)
    .bind(status)
    .execute(&data.db)
    .await?;

    let note = match status {
        "going" => "You are down as attending. You will get a reminder before it starts.",
        "maybe" => "Marked as a maybe.",
        _ => "Marked as not attending.",
    };
    answer(sctx, interaction, note).await?;

    let (going, maybe, _) = signups(data, event_id).await;
    let mut message = interaction.message.clone();
    let _ = message
        .edit(
            sctx,
            EditMessage::new().embed(render(
                &name,
                &description,
                &location,
                host_id,
                time::from_sql(&starts_at).unwrap_or_else(Utc::now),
                max_attendees,
                &going,
                &maybe,
                false,
            )),
        )
        .await;

    Ok(true)
}

async fn answer(
    sctx: &serenity::Context,
    interaction: &ComponentInteraction,
    body: &str,
) -> Result<(), Error> {
    interaction
        .create_response(
            &sctx.http,
            CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content(body)
                    .ephemeral(true),
            ),
        )
        .await?;
    Ok(())
}

/// Send reminders 15 minutes before, and mark events started.
pub async fn sweep(sctx: &serenity::Context, data: &Data) {
    let soon = time::to_sql(Utc::now() + chrono::Duration::minutes(15));
    let now = time::sql_now();

    // ---- reminders -------------------------------------------------------
    let upcoming: Vec<(i64, String, String, i64)> = sqlx::query_as(
        "SELECT id, name, starts_at, channel_id FROM events
         WHERE started = 0 AND reminder_sent = 0 AND starts_at <= ?",
    )
    .bind(&soon)
    .fetch_all(&data.db)
    .await
    .unwrap_or_default();

    for (event_id, name, starts_at, channel_id) in upcoming {
        let (going, _, _) = signups(data, event_id).await;
        let when = time::from_sql(&starts_at)
            .map(|t| time::discord_ts(t, 'R'))
            .unwrap_or_default();

        for user_id in &going {
            if let Ok(dm) = UserId::new(*user_id as u64).create_dm_channel(sctx).await {
                let _ = dm
                    .send_message(
                        &sctx.http,
                        CreateMessage::new().embed(
                            embed::titled("⏰ Event starting soon", embed::WARN)
                                .description(format!("**{name}** starts {when}.")),
                        ),
                    )
                    .await;
            }
        }

        let _ = ChannelId::new(channel_id as u64)
            .send_message(
                &sctx.http,
                CreateMessage::new().content(format!(
                    "⏰ **{name}** starts {when}. {}",
                    going
                        .iter()
                        .take(30)
                        .map(|id| format!("<@{id}>"))
                        .collect::<Vec<_>>()
                        .join(" ")
                )),
            )
            .await;

        let _ = sqlx::query("UPDATE events SET reminder_sent = 1 WHERE id = ?")
            .bind(event_id)
            .execute(&data.db)
            .await;
    }

    // ---- mark started ----------------------------------------------------
    let starting: Vec<(i64, String, i64, Option<i64>)> = sqlx::query_as(
        "SELECT id, name, channel_id, message_id FROM events
         WHERE started = 0 AND starts_at <= ?",
    )
    .bind(&now)
    .fetch_all(&data.db)
    .await
    .unwrap_or_default();

    for (event_id, name, channel_id, message_id) in starting {
        let _ = sqlx::query("UPDATE events SET started = 1 WHERE id = ?")
            .bind(event_id)
            .execute(&data.db)
            .await;

        let _ = ChannelId::new(channel_id as u64)
            .send_message(
                &sctx.http,
                CreateMessage::new().content(format!("🎬 **{name}** is starting now!")),
            )
            .await;

        if let Some(message_id) = message_id {
            let _ = ChannelId::new(channel_id as u64)
                .edit_message(
                    &sctx.http,
                    MessageId::new(message_id as u64),
                    EditMessage::new().components(Vec::new()),
                )
                .await;
        }
    }
}
