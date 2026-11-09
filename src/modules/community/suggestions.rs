//! Suggestions with voting and a status workflow.

use poise::serenity_prelude as serenity;
use serenity::{
    ComponentInteraction, CreateActionRow, CreateButton, CreateInteractionResponse,
    CreateInteractionResponseMessage, CreateMessage, EditMessage, GuildChannel, UserId,
};

use crate::db;
use crate::state::{Ctx, Data, Error};
use crate::util::{embed, perms, text};

#[derive(Debug, poise::ChoiceParameter)]
pub enum Status {
    #[name = "approved"]
    Approved,
    #[name = "denied"]
    Denied,
    #[name = "implemented"]
    Implemented,
    #[name = "considering"]
    Considering,
    #[name = "duplicate"]
    Duplicate,
}

impl Status {
    fn key(&self) -> &'static str {
        match self {
            Status::Approved => "approved",
            Status::Denied => "denied",
            Status::Implemented => "implemented",
            Status::Considering => "considering",
            Status::Duplicate => "duplicate",
        }
    }
}

pub fn status_style(status: &str) -> (&'static str, u32) {
    match status {
        "approved" => ("✅ Approved", embed::SUCCESS),
        "denied" => ("❌ Denied", embed::DANGER),
        "implemented" => ("🚀 Implemented", 0x1ABC9C),
        "considering" => ("🤔 Under consideration", embed::WARN),
        "duplicate" => ("♻️ Duplicate", embed::MUTED),
        _ => ("🗳️ Open for votes", embed::BRAND),
    }
}

fn components(suggestion_id: i64, closed: bool) -> Vec<CreateActionRow> {
    if closed {
        return Vec::new();
    }
    vec![CreateActionRow::Buttons(vec![
        CreateButton::new(format!("suggest:{suggestion_id}:up"))
            .label("Upvote")
            .style(serenity::ButtonStyle::Success)
            .emoji('👍'),
        CreateButton::new(format!("suggest:{suggestion_id}:down"))
            .label("Downvote")
            .style(serenity::ButtonStyle::Danger)
            .emoji('👎'),
    ])]
}

async fn votes(data: &Data, suggestion_id: i64) -> (i64, i64) {
    let up: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM suggestion_votes WHERE suggestion_id = ? AND vote = 1",
    )
    .bind(suggestion_id)
    .fetch_one(&data.db)
    .await
    .unwrap_or(0);
    let down: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM suggestion_votes WHERE suggestion_id = ? AND vote = -1",
    )
    .bind(suggestion_id)
    .fetch_one(&data.db)
    .await
    .unwrap_or(0);
    (up, down)
}

fn render(
    number: i64,
    author_id: i64,
    content: &str,
    status: &str,
    up: i64,
    down: i64,
    note: Option<&str>,
    reviewer: Option<i64>,
) -> serenity::CreateEmbed {
    let (label, colour) = status_style(status);
    let total = up + down;
    let ratio = if total > 0 {
        format!(
            "`{}` {up} 👍 · {down} 👎",
            text::progress_bar(up, total, 16)
        )
    } else {
        "No votes yet".to_string()
    };

    let mut e = embed::titled(format!("Suggestion #{number}"), colour)
        .description(text::truncate(content, 2000))
        .field("Status", label, true)
        .field("Votes", ratio, false)
        .footer(embed::footer(format!("Suggested by user {author_id}")));

    if let Some(note) = note {
        if !note.trim().is_empty() {
            e = e.field(
                "Staff response",
                format!(
                    "{}{}",
                    reviewer
                        .map(|r| format!("<@{r}>: "))
                        .unwrap_or_default(),
                    text::truncate(note, 900)
                ),
                false,
            );
        }
    }
    e
}

/// Suggest something, and let staff triage it.
#[poise::command(
    slash_command,
    guild_only,
    category = "Community",
    subcommands(
        "suggest_new",
        "suggest_channel",
        "suggest_status",
        "suggest_list",
        "suggest_view"
    )
)]
pub async fn suggest(_ctx: Ctx<'_>) -> Result<(), Error> {
    Ok(())
}

/// Post a suggestion.
#[poise::command(slash_command, rename = "new", guild_only)]
pub async fn suggest_new(
    ctx: Ctx<'_>,
    #[description = "Your suggestion"] suggestion: String,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let configured = db::kv_get(&ctx.data().db, guild_id, "suggestion_channel").await;

    let channel = configured
        .and_then(|c| c.parse::<u64>().ok())
        .map(serenity::ChannelId::new)
        .unwrap_or_else(|| ctx.channel_id());

    let number = db::next_number(&ctx.data().db, "suggestions", "number", guild_id).await?;

    let id = sqlx::query(
        "INSERT INTO suggestions (guild_id, number, user_id, content, channel_id)
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(guild_id)
    .bind(number)
    .bind(ctx.author().id.get() as i64)
    .bind(&suggestion)
    .bind(channel.get() as i64)
    .execute(&ctx.data().db)
    .await?
    .last_insert_rowid();

    let message = channel
        .send_message(
            ctx.http(),
            CreateMessage::new()
                .embed(render(
                    number,
                    ctx.author().id.get() as i64,
                    &suggestion,
                    "pending",
                    0,
                    0,
                    None,
                    None,
                ))
                .components(components(id, false)),
        )
        .await?;

    sqlx::query("UPDATE suggestions SET message_id = ? WHERE id = ?")
        .bind(message.id.get() as i64)
        .bind(id)
        .execute(&ctx.data().db)
        .await?;

    ctx.send(
        poise::CreateReply::default()
            .embed(embed::ok(format!(
                "Suggestion **#{number}** posted in <#{}>.",
                channel.get()
            )))
            .ephemeral(true),
    )
    .await?;
    Ok(())
}

/// Where suggestions are posted.
#[poise::command(slash_command, rename = "channel", guild_only, check = "perms::admin_only")]
pub async fn suggest_channel(
    ctx: Ctx<'_>,
    #[description = "Suggestion channel"] channel: GuildChannel,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    db::kv_set(
        &ctx.data().db,
        guild_id,
        "suggestion_channel",
        &channel.id.get().to_string(),
    )
    .await?;
    ctx.send(poise::CreateReply::default().embed(embed::ok(format!(
        "Suggestions will be posted in <#{}>.",
        channel.id.get()
    ))))
    .await?;
    Ok(())
}

/// Set the status of a suggestion and tell the author.
#[poise::command(slash_command, rename = "status", guild_only, check = "perms::staff_only")]
pub async fn suggest_status(
    ctx: Ctx<'_>,
    #[description = "Suggestion number"] number: i64,
    #[description = "New status"] status: Status,
    #[description = "Note shown publicly and DMed to the author"] note: Option<String>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;

    let found: Option<(i64, i64, String, i64, Option<i64>)> = sqlx::query_as(
        "SELECT id, user_id, content, channel_id, message_id FROM suggestions
         WHERE guild_id = ? AND number = ?",
    )
    .bind(guild_id)
    .bind(number)
    .fetch_optional(&ctx.data().db)
    .await?;

    let Some((id, author_id, content, channel_id, message_id)) = found else {
        ctx.send(
            poise::CreateReply::default()
                .embed(embed::err("No suggestion with that number.")),
        )
        .await?;
        return Ok(());
    };

    sqlx::query(
        "UPDATE suggestions SET status = ?, staff_note = ?, reviewer = ? WHERE id = ?",
    )
    .bind(status.key())
    .bind(&note)
    .bind(ctx.author().id.get() as i64)
    .bind(id)
    .execute(&ctx.data().db)
    .await?;

    let (up, down) = votes(ctx.data(), id).await;
    let updated = render(
        number,
        author_id,
        &content,
        status.key(),
        up,
        down,
        note.as_deref(),
        Some(ctx.author().id.get() as i64),
    );

    // Update the original post and lock voting.
    if let Some(message_id) = message_id {
        let _ = serenity::ChannelId::new(channel_id as u64)
            .edit_message(
                ctx.http(),
                serenity::MessageId::new(message_id as u64),
                EditMessage::new()
                    .embed(updated.clone())
                    .components(Vec::new()),
            )
            .await;
    }

    // Let the author know.
    if let Ok(dm) = UserId::new(author_id as u64)
        .create_dm_channel(ctx.serenity_context())
        .await
    {
        let _ = dm
            .send_message(
                ctx.http(),
                CreateMessage::new().embed(
                    embed::titled(
                        format!("Your suggestion #{number} was updated"),
                        status_style(status.key()).1,
                    )
                    .description(format!(
                        "**{}**\n\n{}",
                        status_style(status.key()).0,
                        note.clone().unwrap_or_else(|| "No note was left.".into())
                    )),
                ),
            )
            .await;
    }

    ctx.send(
        poise::CreateReply::default()
            .embed(embed::ok(format!(
                "Suggestion #{number} marked **{}**.",
                status.key()
            )))
            .ephemeral(true),
    )
    .await?;
    Ok(())
}

/// Browse suggestions.
#[poise::command(slash_command, rename = "list", guild_only)]
pub async fn suggest_list(
    ctx: Ctx<'_>,
    #[description = "Only show this status"] status: Option<Status>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;

    let rows: Vec<(i64, i64, String, String)> = match &status {
        Some(status) => {
            sqlx::query_as(
                "SELECT number, user_id, content, status FROM suggestions
                 WHERE guild_id = ? AND status = ? ORDER BY number DESC LIMIT 15",
            )
            .bind(guild_id)
            .bind(status.key())
            .fetch_all(&ctx.data().db)
            .await?
        }
        None => {
            sqlx::query_as(
                "SELECT number, user_id, content, status FROM suggestions
                 WHERE guild_id = ? ORDER BY number DESC LIMIT 15",
            )
            .bind(guild_id)
            .fetch_all(&ctx.data().db)
            .await?
        }
    };

    let body = if rows.is_empty() {
        "Nothing here yet.".to_string()
    } else {
        rows.iter()
            .map(|(number, user_id, content, status)| {
                format!(
                    "**#{number}** {} · <@{user_id}>\n└ {}",
                    status_style(status).0,
                    text::truncate(&text::escape_md(content), 120)
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    ctx.send(
        poise::CreateReply::default()
            .embed(embed::titled("💡 Suggestions", embed::BRAND).description(body)),
    )
    .await?;
    Ok(())
}

/// Show one suggestion in full.
#[poise::command(slash_command, rename = "view", guild_only)]
pub async fn suggest_view(
    ctx: Ctx<'_>,
    #[description = "Suggestion number"] number: i64,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let found: Option<(i64, i64, String, String, Option<String>, Option<i64>)> = sqlx::query_as(
        "SELECT id, user_id, content, status, staff_note, reviewer FROM suggestions
         WHERE guild_id = ? AND number = ?",
    )
    .bind(guild_id)
    .bind(number)
    .fetch_optional(&ctx.data().db)
    .await?;

    let Some((id, author_id, content, status, note, reviewer)) = found else {
        ctx.send(
            poise::CreateReply::default()
                .embed(embed::err("No suggestion with that number.")),
        )
        .await?;
        return Ok(());
    };

    let (up, down) = votes(ctx.data(), id).await;
    ctx.send(poise::CreateReply::default().embed(render(
        number,
        author_id,
        &content,
        &status,
        up,
        down,
        note.as_deref(),
        reviewer,
    )))
    .await?;
    Ok(())
}

/// Handle an upvote/downvote button.
pub async fn handle_component(
    sctx: &serenity::Context,
    data: &Data,
    interaction: &ComponentInteraction,
) -> Result<bool, Error> {
    let Some(rest) = interaction.data.custom_id.strip_prefix("suggest:") else {
        return Ok(false);
    };
    let mut parts = rest.split(':');
    let id: i64 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let direction = parts.next().unwrap_or("");
    let vote = if direction == "up" { 1i64 } else { -1i64 };

    let found: Option<(i64, i64, String, String)> =
        sqlx::query_as("SELECT number, user_id, content, status FROM suggestions WHERE id = ?")
            .bind(id)
            .fetch_optional(&data.db)
            .await?;

    let Some((number, author_id, content, status)) = found else {
        return Ok(true);
    };

    let user_id = interaction.user.id.get() as i64;
    let existing: Option<i64> = sqlx::query_scalar(
        "SELECT vote FROM suggestion_votes WHERE suggestion_id = ? AND user_id = ?",
    )
    .bind(id)
    .bind(user_id)
    .fetch_optional(&data.db)
    .await?;

    let note = match existing {
        Some(previous) if previous == vote => {
            sqlx::query("DELETE FROM suggestion_votes WHERE suggestion_id = ? AND user_id = ?")
                .bind(id)
                .bind(user_id)
                .execute(&data.db)
                .await?;
            "Vote removed."
        }
        _ => {
            sqlx::query(
                "INSERT INTO suggestion_votes (suggestion_id, user_id, vote) VALUES (?, ?, ?)
                 ON CONFLICT(suggestion_id, user_id) DO UPDATE SET vote = excluded.vote",
            )
            .bind(id)
            .bind(user_id)
            .bind(vote)
            .execute(&data.db)
            .await?;
            if vote == 1 {
                "Upvoted."
            } else {
                "Downvoted."
            }
        }
    };

    interaction
        .create_response(
            &sctx.http,
            CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content(note)
                    .ephemeral(true),
            ),
        )
        .await?;

    let (up, down) = votes(data, id).await;
    let mut message = interaction.message.clone();
    let _ = message
        .edit(
            sctx,
            EditMessage::new().embed(render(
                number, author_id, &content, &status, up, down, None, None,
            )),
        )
        .await;

    Ok(true)
}

/// Anonymous feedback straight to the staff team.
#[poise::command(slash_command, guild_only, category = "Community", ephemeral)]
pub async fn feedback(
    ctx: Ctx<'_>,
    #[description = "What would you like staff to know?"] message: String,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;

    sqlx::query("INSERT INTO anon_feedback (guild_id, content) VALUES (?, ?)")
        .bind(guild_id)
        .bind(&message)
        .execute(&ctx.data().db)
        .await?;

    // Deliberately no author field: this is anonymous by design. The row in
    // anon_feedback carries no user id either.
    let target = db::kv_get(&ctx.data().db, guild_id, "feedback_channel")
        .await
        .and_then(|c| c.parse::<u64>().ok())
        .map(serenity::ChannelId::new);

    let notice = embed::titled("📨 Anonymous feedback", embed::INFO)
        .description(text::truncate(&message, 3000))
        .footer(embed::footer("Sent anonymously - no author is recorded."));

    if let Some(channel) = target {
        channel
            .send_message(ctx.http(), CreateMessage::new().embed(notice))
            .await?;
    } else {
        crate::logs::send(
            ctx.serenity_context(),
            &ctx.data().db,
            guild_id,
            crate::logs::Log::Mod,
            notice,
        )
        .await;
    }

    ctx.send(
        poise::CreateReply::default()
            .embed(embed::ok(
                "Sent. Staff will see the message but not who wrote it.",
            ))
            .ephemeral(true),
    )
    .await?;
    Ok(())
}

/// Choose where anonymous feedback is delivered.
#[poise::command(
    slash_command,
    guild_only,
    category = "Community",
    check = "perms::admin_only"
)]
pub async fn feedbackchannel(
    ctx: Ctx<'_>,
    #[description = "Channel for anonymous feedback"] channel: GuildChannel,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    db::kv_set(
        &ctx.data().db,
        guild_id,
        "feedback_channel",
        &channel.id.get().to_string(),
    )
    .await?;
    ctx.send(poise::CreateReply::default().embed(embed::ok(format!(
        "Anonymous feedback will go to <#{}>.",
        channel.id.get()
    ))))
    .await?;
    Ok(())
}
