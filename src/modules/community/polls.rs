//! Polls, including anonymous and multiple-choice ones.

use chrono::Utc;
use poise::serenity_prelude as serenity;
use serenity::{
    ComponentInteraction, CreateActionRow, CreateButton, CreateInteractionResponse,
    CreateInteractionResponseMessage, CreateMessage, EditMessage,
};

use crate::db::Db;
use crate::state::{Ctx, Data, Error};
use crate::util::{embed, text, time};

const LETTERS: [&str; 10] = ["🇦", "🇧", "🇨", "🇩", "🇪", "🇫", "🇬", "🇭", "🇮", "🇯"];

fn components(poll_id: i64, options: &[String]) -> Vec<CreateActionRow> {
    let mut rows = Vec::new();
    for (chunk_index, chunk) in options.chunks(5).enumerate() {
        let buttons: Vec<CreateButton> = chunk
            .iter()
            .enumerate()
            .map(|(i, option)| {
                let index = chunk_index * 5 + i;
                CreateButton::new(format!("poll:{poll_id}:{index}"))
                    .label(text::truncate(option, 70))
                    .style(serenity::ButtonStyle::Secondary)
            })
            .collect();
        rows.push(CreateActionRow::Buttons(buttons));
    }
    rows.push(CreateActionRow::Buttons(vec![CreateButton::new(format!(
        "poll:{poll_id}:results"
    ))
    .label("Show results")
    .style(serenity::ButtonStyle::Primary)
    .emoji('📊')]));
    rows
}

async fn tally(db: &Db, poll_id: i64, option_count: usize) -> Vec<i64> {
    let rows: Vec<(i64, i64)> = sqlx::query_as(
        "SELECT choice, COUNT(*) FROM poll_votes WHERE poll_id = ? GROUP BY choice",
    )
    .bind(poll_id)
    .fetch_all(db)
    .await
    .unwrap_or_default();

    let mut counts = vec![0i64; option_count];
    for (choice, count) in rows {
        if let Some(slot) = counts.get_mut(choice as usize) {
            *slot = count;
        }
    }
    counts
}

/// Render the poll body, hiding counts while an anonymous poll is still open.
pub fn render(
    question: &str,
    options: &[String],
    counts: &[i64],
    anonymous: bool,
    closed: bool,
    ends_at: Option<&str>,
) -> serenity::CreateEmbed {
    let total: i64 = counts.iter().sum();
    let show_counts = closed || !anonymous;

    let body = options
        .iter()
        .enumerate()
        .map(|(i, option)| {
            let letter = LETTERS.get(i).copied().unwrap_or("▫️");
            if !show_counts {
                return format!("{letter} **{option}**");
            }
            let count = counts.get(i).copied().unwrap_or(0);
            let percent = if total > 0 {
                (count as f64 / total as f64 * 100.0).round() as i64
            } else {
                0
            };
            format!(
                "{letter} **{option}**\n`{}` {count} vote(s) · {percent}%",
                text::progress_bar(count, total.max(1), 16)
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");

    let mut e = embed::titled(
        format!("📊 {}", text::truncate(question, 240)),
        if closed { embed::MUTED } else { embed::BRAND },
    )
    .description(body);

    let mut footer = format!("{total} vote(s)");
    if anonymous {
        footer.push_str(" · anonymous");
    }
    if closed {
        footer.push_str(" · closed");
    }
    e = e.footer(embed::footer(footer));

    if !closed {
        if let Some(ends) = ends_at.and_then(time::from_sql) {
            e = e.field("Closes", time::discord_ts(ends, 'R'), true);
        }
    }
    e
}

/// Create and manage polls.
#[poise::command(
    slash_command,
    guild_only,
    category = "Community",
    subcommands("poll_create", "poll_close", "poll_results")
)]
pub async fn poll(_ctx: Ctx<'_>) -> Result<(), Error> {
    Ok(())
}

/// Start a poll.
#[poise::command(slash_command, rename = "create", guild_only)]
pub async fn poll_create(
    ctx: Ctx<'_>,
    #[description = "The question"] question: String,
    #[description = "Options, separated by |  (2-10)"] options: String,
    #[description = "How long it runs, e.g. 1h, 2d"] duration: Option<String>,
    #[description = "Hide who voted and the running totals"] anonymous: Option<bool>,
    #[description = "Allow picking more than one option"] multiple_choice: Option<bool>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;

    let parsed: Vec<String> = options
        .split('|')
        .map(|o| o.trim().to_string())
        .filter(|o| !o.is_empty())
        .take(10)
        .collect();

    if parsed.len() < 2 {
        ctx.send(poise::CreateReply::default().embed(embed::err(
            "Give me at least two options, separated by `|`.\nExample: `Yes | No | Maybe`",
        )))
        .await?;
        return Ok(());
    }

    let ends_at = duration
        .as_deref()
        .and_then(time::parse_duration)
        .map(|d| time::to_sql(Utc::now() + chrono::Duration::seconds(d.as_secs() as i64)));

    let anonymous = anonymous.unwrap_or(false);
    let multi = multiple_choice.unwrap_or(false);

    let poll_id = sqlx::query(
        "INSERT INTO polls (guild_id, channel_id, author_id, question, options, anonymous, multi, ends_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(guild_id)
    .bind(ctx.channel_id().get() as i64)
    .bind(ctx.author().id.get() as i64)
    .bind(&question)
    .bind(serde_json::to_string(&parsed)?)
    .bind(i64::from(anonymous))
    .bind(i64::from(multi))
    .bind(&ends_at)
    .execute(&ctx.data().db)
    .await?
    .last_insert_rowid();

    let counts = vec![0i64; parsed.len()];
    let message = ctx
        .channel_id()
        .send_message(
            ctx.http(),
            CreateMessage::new()
                .embed(render(
                    &question,
                    &parsed,
                    &counts,
                    anonymous,
                    false,
                    ends_at.as_deref(),
                ))
                .components(components(poll_id, &parsed)),
        )
        .await?;

    sqlx::query("UPDATE polls SET message_id = ? WHERE id = ?")
        .bind(message.id.get() as i64)
        .bind(poll_id)
        .execute(&ctx.data().db)
        .await?;

    ctx.send(
        poise::CreateReply::default()
            .embed(embed::ok(format!("Poll **#{poll_id}** is live.")))
            .ephemeral(true),
    )
    .await?;
    Ok(())
}

/// Close a poll early and publish the result.
#[poise::command(slash_command, rename = "close", guild_only)]
pub async fn poll_close(
    ctx: Ctx<'_>,
    #[description = "Poll number"] poll_id: i64,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;

    let found: Option<(i64, String, String, i64, i64)> = sqlx::query_as(
        "SELECT author_id, question, options, anonymous, closed FROM polls
         WHERE id = ? AND guild_id = ?",
    )
    .bind(poll_id)
    .bind(guild_id)
    .fetch_optional(&ctx.data().db)
    .await?;

    let Some((author_id, question, raw_options, anonymous, closed)) = found else {
        ctx.send(poise::CreateReply::default().embed(embed::err("No poll with that number.")))
            .await?;
        return Ok(());
    };
    if closed == 1 {
        ctx.send(poise::CreateReply::default().embed(embed::warn("That poll is already closed.")))
            .await?;
        return Ok(());
    }
    if author_id != ctx.author().id.get() as i64 && !crate::util::perms::is_staff(&ctx).await {
        ctx.send(
            poise::CreateReply::default()
                .embed(embed::err("Only the poll author or staff can close it.")),
        )
        .await?;
        return Ok(());
    }

    close_poll(ctx.serenity_context(), ctx.data(), poll_id).await;

    let options: Vec<String> = serde_json::from_str(&raw_options).unwrap_or_default();
    let counts = tally(&ctx.data().db, poll_id, options.len()).await;
    ctx.send(
        poise::CreateReply::default()
            .embed(render(&question, &options, &counts, anonymous == 1, true, None)),
    )
    .await?;
    Ok(())
}

/// Show the current standings of a poll.
#[poise::command(slash_command, rename = "results", guild_only)]
pub async fn poll_results(
    ctx: Ctx<'_>,
    #[description = "Poll number"] poll_id: i64,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let found: Option<(String, String, i64, i64)> = sqlx::query_as(
        "SELECT question, options, anonymous, closed FROM polls WHERE id = ? AND guild_id = ?",
    )
    .bind(poll_id)
    .bind(guild_id)
    .fetch_optional(&ctx.data().db)
    .await?;

    let Some((question, raw_options, anonymous, closed)) = found else {
        ctx.send(poise::CreateReply::default().embed(embed::err("No poll with that number.")))
            .await?;
        return Ok(());
    };

    let options: Vec<String> = serde_json::from_str(&raw_options).unwrap_or_default();
    let counts = tally(&ctx.data().db, poll_id, options.len()).await;

    // An anonymous poll keeps its counts private until it closes, even here.
    ctx.send(
        poise::CreateReply::default()
            .embed(render(
                &question,
                &options,
                &counts,
                anonymous == 1,
                closed == 1,
                None,
            ))
            .ephemeral(anonymous == 1 && closed == 0),
    )
    .await?;
    Ok(())
}

/// Handle a poll button.
pub async fn handle_component(
    sctx: &serenity::Context,
    data: &Data,
    interaction: &ComponentInteraction,
) -> Result<bool, Error> {
    let Some(rest) = interaction.data.custom_id.strip_prefix("poll:") else {
        return Ok(false);
    };
    let mut parts = rest.split(':');
    let poll_id: i64 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let action = parts.next().unwrap_or("");

    let found: Option<(String, String, i64, i64, i64)> = sqlx::query_as(
        "SELECT question, options, anonymous, multi, closed FROM polls WHERE id = ?",
    )
    .bind(poll_id)
    .fetch_optional(&data.db)
    .await?;

    let Some((question, raw_options, anonymous, multi, closed)) = found else {
        reply(sctx, interaction, "That poll no longer exists.").await?;
        return Ok(true);
    };
    let options: Vec<String> = serde_json::from_str(&raw_options).unwrap_or_default();

    if action == "results" {
        let counts = tally(&data.db, poll_id, options.len()).await;
        interaction
            .create_response(
                &sctx.http,
                CreateInteractionResponse::Message(
                    CreateInteractionResponseMessage::new()
                        .embed(render(
                            &question,
                            &options,
                            &counts,
                            anonymous == 1,
                            closed == 1,
                            None,
                        ))
                        .ephemeral(true),
                ),
            )
            .await?;
        return Ok(true);
    }

    if closed == 1 {
        reply(sctx, interaction, "This poll is closed.").await?;
        return Ok(true);
    }

    let Ok(choice) = action.parse::<i64>() else {
        return Ok(true);
    };
    if choice < 0 || choice as usize >= options.len() {
        return Ok(true);
    }

    let user_id = interaction.user.id.get() as i64;

    // A single-choice poll replaces the previous vote; a multi one toggles.
    let already: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM poll_votes WHERE poll_id = ? AND user_id = ? AND choice = ?",
    )
    .bind(poll_id)
    .bind(user_id)
    .bind(choice)
    .fetch_one(&data.db)
    .await?;

    let message = if already > 0 {
        sqlx::query("DELETE FROM poll_votes WHERE poll_id = ? AND user_id = ? AND choice = ?")
            .bind(poll_id)
            .bind(user_id)
            .bind(choice)
            .execute(&data.db)
            .await?;
        format!("Removed your vote for **{}**.", options[choice as usize])
    } else {
        if multi == 0 {
            sqlx::query("DELETE FROM poll_votes WHERE poll_id = ? AND user_id = ?")
                .bind(poll_id)
                .bind(user_id)
                .execute(&data.db)
                .await?;
        }
        sqlx::query("INSERT OR IGNORE INTO poll_votes (poll_id, user_id, choice) VALUES (?, ?, ?)")
            .bind(poll_id)
            .bind(user_id)
            .bind(choice)
            .execute(&data.db)
            .await?;
        format!("Voted for **{}**.", options[choice as usize])
    };

    reply(sctx, interaction, &message).await?;

    // Refresh the public embed for non-anonymous polls.
    if anonymous == 0 {
        let counts = tally(&data.db, poll_id, options.len()).await;
        let mut message = interaction.message.clone();
        let _ = message
            .edit(
                sctx,
                EditMessage::new().embed(render(
                    &question,
                    &options,
                    &counts,
                    false,
                    false,
                    None,
                )),
            )
            .await;
    }

    Ok(true)
}

async fn reply(
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

/// Mark a poll closed and publish the final tally. Used by the scheduler too.
pub async fn close_poll(sctx: &serenity::Context, data: &Data, poll_id: i64) {
    let found: Option<(i64, i64, Option<i64>, String, String, i64)> = sqlx::query_as(
        "SELECT guild_id, channel_id, message_id, question, options, anonymous
         FROM polls WHERE id = ?",
    )
    .bind(poll_id)
    .fetch_optional(&data.db)
    .await
    .ok()
    .flatten();

    let Some((_guild_id, channel_id, message_id, question, raw_options, anonymous)) = found else {
        return;
    };

    let _ = sqlx::query("UPDATE polls SET closed = 1 WHERE id = ?")
        .bind(poll_id)
        .execute(&data.db)
        .await;

    let options: Vec<String> = serde_json::from_str(&raw_options).unwrap_or_default();
    let counts = tally(&data.db, poll_id, options.len()).await;
    let final_embed = render(&question, &options, &counts, anonymous == 1, true, None);

    let channel = serenity::ChannelId::new(channel_id as u64);

    // Edit the original message so the result lives where the poll was.
    if let Some(message_id) = message_id {
        let _ = channel
            .edit_message(
                &sctx.http,
                serenity::MessageId::new(message_id as u64),
                EditMessage::new()
                    .embed(final_embed.clone())
                    .components(Vec::new()),
            )
            .await;
    }

    let winner = counts
        .iter()
        .enumerate()
        .max_by_key(|(_, count)| **count)
        .filter(|(_, count)| **count > 0)
        .and_then(|(i, _)| options.get(i).cloned());

    let note = match winner {
        Some(winner) => format!("Poll closed. Winner: **{winner}**."),
        None => "Poll closed with no votes.".to_string(),
    };
    let _ = channel
        .send_message(&sctx.http, CreateMessage::new().content(note))
        .await;
}

/// Close every poll whose deadline has passed.
pub async fn sweep(sctx: &serenity::Context, data: &Data) {
    let due: Vec<i64> = sqlx::query_scalar(
        "SELECT id FROM polls WHERE closed = 0 AND ends_at IS NOT NULL AND ends_at <= ?",
    )
    .bind(time::sql_now())
    .fetch_all(&data.db)
    .await
    .unwrap_or_default();

    for poll_id in due {
        close_poll(sctx, data, poll_id).await;
    }
}
