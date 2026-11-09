//! Starboard: messages the server reacts to enough get mirrored to a channel.

use poise::serenity_prelude as serenity;
use serenity::{
    ChannelId, CreateEmbed, CreateMessage, EditMessage, GuildChannel, MessageId, Reaction,
    ReactionType,
};

use crate::db;
use crate::state::{Ctx, Data, Error};
use crate::util::{embed, perms, text};

fn emoji_matches(reaction: &ReactionType, configured: &str) -> bool {
    match reaction {
        ReactionType::Unicode(unicode) => unicode == configured,
        ReactionType::Custom { name, id, .. } => {
            configured.contains(&id.get().to_string())
                || name
                    .as_deref()
                    .map(|n| configured.contains(n))
                    .unwrap_or(false)
        }
        _ => false,
    }
}

fn star_emoji_for(count: i64) -> &'static str {
    match count {
        0..=4 => "⭐",
        5..=9 => "🌟",
        10..=24 => "💫",
        _ => "✨",
    }
}

/// Called on every reaction add and remove.
pub async fn handle_reaction(
    sctx: &serenity::Context,
    data: &Data,
    reaction: &Reaction,
) -> Result<(), Error> {
    let Some(guild_id) = reaction.guild_id else {
        return Ok(());
    };
    let guild_i64 = guild_id.get() as i64;

    let cfg = db::starboard_config(&data.db, guild_i64).await?;
    let Some(board_channel) = cfg.channel_id else {
        return Ok(());
    };
    if !emoji_matches(&reaction.emoji, &cfg.emoji) {
        return Ok(());
    }
    // Never mirror the starboard into itself.
    if reaction.channel_id.get() as i64 == board_channel {
        return Ok(());
    }

    let Ok(message) = reaction
        .channel_id
        .message(&sctx.http, reaction.message_id)
        .await
    else {
        return Ok(());
    };

    if cfg.ignore_bots == 1 && message.author.bot {
        return Ok(());
    }

    // Count the stars, optionally ignoring the author's own.
    let mut stars = message
        .reactions
        .iter()
        .find(|r| emoji_matches(&r.reaction_type, &cfg.emoji))
        .map(|r| r.count as i64)
        .unwrap_or(0);

    if cfg.self_star == 0 {
        // Check whether the author is among the reactors and drop one if so.
        if let Ok(users) = message
            .channel_id
            .reaction_users(
                &sctx.http,
                message.id,
                reaction.emoji.clone(),
                Some(100),
                None::<serenity::UserId>,
            )
            .await
        {
            if users.iter().any(|u| u.id == message.author.id) {
                stars -= 1;
            }
        }
    }

    let existing: Option<(i64, i64)> = sqlx::query_as(
        "SELECT star_message, stars FROM starboard_entries WHERE guild_id = ? AND message_id = ?",
    )
    .bind(guild_i64)
    .bind(message.id.get() as i64)
    .fetch_optional(&data.db)
    .await?;

    let board = ChannelId::new(board_channel as u64);

    // Below the threshold: remove the entry if there was one.
    if stars < cfg.threshold {
        if let Some((star_message, _)) = existing {
            let _ = board
                .delete_message(
                    &sctx.http,
                    MessageId::new(star_message as u64),
                    Some("Fell below the starboard threshold"),
                )
                .await;
            sqlx::query("DELETE FROM starboard_entries WHERE guild_id = ? AND message_id = ?")
                .bind(guild_i64)
                .bind(message.id.get() as i64)
                .execute(&data.db)
                .await?;
        }
        return Ok(());
    }

    let link = format!(
        "https://discord.com/channels/{}/{}/{}",
        guild_id.get(),
        message.channel_id.get(),
        message.id.get()
    );

    let mut card = CreateEmbed::new()
        .colour(embed::colour(embed::STAR))
        .author(
            serenity::CreateEmbedAuthor::new(message.author.tag())
                .icon_url(message.author.face()),
        )
        .description(if message.content.trim().is_empty() {
            format!("*(no text)*\n\n[Jump to message]({link})")
        } else {
            format!(
                "{}\n\n[Jump to message]({link})",
                text::truncate(&message.content, 3000)
            )
        })
        .field("Channel", format!("<#{}>", message.channel_id.get()), true)
        .timestamp(message.timestamp);

    // Mirror the first image if there is one.
    if let Some(attachment) = message
        .attachments
        .iter()
        .find(|a| a.content_type.as_deref().map(|c| c.starts_with("image/")).unwrap_or(false))
    {
        card = card.image(attachment.url.clone());
    } else if let Some(image) = message
        .embeds
        .iter()
        .find_map(|e| e.image.as_ref().map(|i| i.url.clone()))
    {
        card = card.image(image);
    }

    let header = format!(
        "{} **{stars}** · <#{}>",
        star_emoji_for(stars),
        message.channel_id.get()
    );

    match existing {
        Some((star_message, _)) => {
            let _ = board
                .edit_message(
                    &sctx.http,
                    MessageId::new(star_message as u64),
                    EditMessage::new().content(header).embed(card),
                )
                .await;
            sqlx::query(
                "UPDATE starboard_entries SET stars = ? WHERE guild_id = ? AND message_id = ?",
            )
            .bind(stars)
            .bind(guild_i64)
            .bind(message.id.get() as i64)
            .execute(&data.db)
            .await?;
        }
        None => {
            if let Ok(posted) = board
                .send_message(
                    &sctx.http,
                    CreateMessage::new().content(header).embed(card),
                )
                .await
            {
                sqlx::query(
                    "INSERT INTO starboard_entries (guild_id, message_id, star_message, stars)
                     VALUES (?, ?, ?, ?)
                     ON CONFLICT(guild_id, message_id) DO UPDATE SET
                        star_message = excluded.star_message, stars = excluded.stars",
                )
                .bind(guild_i64)
                .bind(message.id.get() as i64)
                .bind(posted.id.get() as i64)
                .bind(stars)
                .execute(&data.db)
                .await?;
            }
        }
    }

    Ok(())
}

/// Configure the starboard.
#[poise::command(
    slash_command,
    guild_only,
    category = "Community",
    subcommands("starboard_setup", "starboard_off", "starboard_top", "starboard_status")
)]
pub async fn starboard(_ctx: Ctx<'_>) -> Result<(), Error> {
    Ok(())
}

#[poise::command(slash_command, rename = "setup", guild_only, check = "perms::admin_only")]
pub async fn starboard_setup(
    ctx: Ctx<'_>,
    #[description = "Channel the starred messages are posted in"] channel: GuildChannel,
    #[description = "How many reactions are needed"]
    #[min = 1]
    #[max = 50]
    threshold: Option<i64>,
    #[description = "Emoji to count (default ⭐)"] emoji: Option<String>,
    #[description = "Let authors star their own messages"] allow_self_star: Option<bool>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let dbh = &ctx.data().db;

    db::set_opt_i64(
        dbh,
        "starboard_config",
        "channel_id",
        guild_id,
        Some(channel.id.get() as i64),
    )
    .await?;
    if let Some(threshold) = threshold {
        db::set_i64(dbh, "starboard_config", "threshold", guild_id, threshold).await?;
    }
    if let Some(emoji) = emoji {
        db::set_text(dbh, "starboard_config", "emoji", guild_id, emoji.trim().to_string()).await?;
    }
    if let Some(allow) = allow_self_star {
        db::set_bool(dbh, "starboard_config", "self_star", guild_id, allow).await?;
    }

    let cfg = db::starboard_config(dbh, guild_id).await?;
    ctx.send(poise::CreateReply::default().embed(embed::ok(format!(
        "Starboard set up in <#{}> at **{}** {} reaction(s).",
        channel.id.get(),
        cfg.threshold,
        cfg.emoji
    ))))
    .await?;
    Ok(())
}

#[poise::command(slash_command, rename = "off", guild_only, check = "perms::admin_only")]
pub async fn starboard_off(ctx: Ctx<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    db::set_opt_i64(
        &ctx.data().db,
        "starboard_config",
        "channel_id",
        guild_id,
        None,
    )
    .await?;
    ctx.send(poise::CreateReply::default().embed(embed::ok("Starboard switched off.")))
        .await?;
    Ok(())
}

#[poise::command(slash_command, rename = "status", guild_only)]
pub async fn starboard_status(ctx: Ctx<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let cfg = db::starboard_config(&ctx.data().db, guild_id).await?;
    let entries: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM starboard_entries WHERE guild_id = ?")
            .bind(guild_id)
            .fetch_one(&ctx.data().db)
            .await?;

    ctx.send(
        poise::CreateReply::default().embed(
            embed::titled("⭐ Starboard", embed::STAR)
                .field(
                    "Channel",
                    cfg.channel_id
                        .map(|c| format!("<#{c}>"))
                        .unwrap_or_else(|| "off".into()),
                    true,
                )
                .field("Threshold", cfg.threshold.to_string(), true)
                .field("Emoji", cfg.emoji.clone(), true)
                .field(
                    "Self-stars",
                    if cfg.self_star == 1 { "counted" } else { "ignored" },
                    true,
                )
                .field("Messages on the board", entries.to_string(), true),
        ),
    )
    .await?;
    Ok(())
}

/// The most starred messages here.
#[poise::command(slash_command, rename = "top", guild_only)]
pub async fn starboard_top(ctx: Ctx<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap();
    let rows: Vec<(i64, i64, i64)> = sqlx::query_as(
        "SELECT message_id, star_message, stars FROM starboard_entries
         WHERE guild_id = ? ORDER BY stars DESC LIMIT 10",
    )
    .bind(guild_id.get() as i64)
    .fetch_all(&ctx.data().db)
    .await?;

    let cfg = db::starboard_config(&ctx.data().db, guild_id.get() as i64).await?;
    let board = cfg.channel_id.unwrap_or(0);

    let body = if rows.is_empty() {
        "Nothing on the starboard yet.".to_string()
    } else {
        rows.iter()
            .enumerate()
            .map(|(i, (_, star_message, stars))| {
                format!(
                    "**{}.** {} **{stars}** — [view](https://discord.com/channels/{}/{}/{})",
                    i + 1,
                    star_emoji_for(*stars),
                    guild_id.get(),
                    board,
                    star_message
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    ctx.send(
        poise::CreateReply::default()
            .embed(embed::titled("⭐ Top starred messages", embed::STAR).description(body)),
    )
    .await?;
    Ok(())
}

pub fn commands() -> Vec<poise::Command<Data, Error>> {
    vec![starboard()]
}
