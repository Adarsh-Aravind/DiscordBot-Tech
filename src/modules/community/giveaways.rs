//! Giveaways with entry requirements, bonus entries and rerolls.

use chrono::Utc;
use poise::serenity_prelude as serenity;
use serenity::{
    ChannelId, ComponentInteraction, CreateActionRow, CreateButton, CreateInteractionResponse,
    CreateInteractionResponseMessage, CreateMessage, EditMessage, GuildChannel, MessageId, Role,
};

use crate::state::{Ctx, Data, Error};
use crate::util::{embed, perms, time};

fn components(giveaway_id: i64, ended: bool, entries: i64) -> Vec<CreateActionRow> {
    if ended {
        return Vec::new();
    }
    vec![CreateActionRow::Buttons(vec![
        CreateButton::new(format!("giveaway:{giveaway_id}:enter"))
            .label("Enter")
            .style(serenity::ButtonStyle::Success)
            .emoji('🎉'),
        CreateButton::new(format!("giveaway:{giveaway_id}:count"))
            .label(format!("{entries} entered"))
            .style(serenity::ButtonStyle::Secondary)
            .disabled(true),
    ])]
}

fn render(
    prize: &str,
    host_id: i64,
    winners: i64,
    ends_at: Option<chrono::DateTime<Utc>>,
    required_role: Option<i64>,
    bonus_role: Option<i64>,
    bonus_entries: i64,
    min_level: i64,
    entries: i64,
    result: Option<&str>,
) -> serenity::CreateEmbed {
    let mut requirements = Vec::new();
    if let Some(role) = required_role {
        requirements.push(format!("Must have <@&{role}>"));
    }
    if min_level > 0 {
        requirements.push(format!("Must be level **{min_level}** or above"));
    }
    if let Some(role) = bonus_role {
        if bonus_entries > 0 {
            requirements.push(format!(
                "<@&{role}> gets **{}** extra entr{}",
                bonus_entries,
                if bonus_entries == 1 { "y" } else { "ies" }
            ));
        }
    }

    let mut e = embed::titled(
        format!("🎉 {prize}"),
        if result.is_some() {
            embed::MUTED
        } else {
            embed::BRAND
        },
    )
    .field("Winners", winners.to_string(), true)
    .field("Entries", entries.to_string(), true)
    .field("Hosted by", format!("<@{host_id}>"), true);

    if let Some(ends) = ends_at {
        e = e.field(
            if result.is_some() { "Ended" } else { "Ends" },
            time::full_and_relative(ends),
            false,
        );
    }
    if !requirements.is_empty() {
        e = e.field("Requirements", requirements.join("\n"), false);
    }
    if let Some(result) = result {
        e = e.field("Result", result, false);
    } else {
        e = e.footer(embed::footer("Press Enter to join"));
    }
    e
}

/// Run a giveaway.
#[poise::command(
    slash_command,
    guild_only,
    category = "Community",
    subcommands(
        "giveaway_start",
        "giveaway_end",
        "giveaway_reroll",
        "giveaway_list",
        "giveaway_cancel"
    ),
    check = "perms::staff_only"
)]
pub async fn giveaway(_ctx: Ctx<'_>) -> Result<(), Error> {
    Ok(())
}

/// Start a giveaway.
#[poise::command(slash_command, rename = "start", guild_only)]
pub async fn giveaway_start(
    ctx: Ctx<'_>,
    #[description = "What is being given away"] prize: String,
    #[description = "How long it runs, e.g. 2h, 3d"] duration: String,
    #[description = "Number of winners"]
    #[min = 1]
    #[max = 25]
    winners: Option<i64>,
    #[description = "Channel, defaults to this one"] channel: Option<GuildChannel>,
    #[description = "Role required to enter"] required_role: Option<Role>,
    #[description = "Role that gets bonus entries"] bonus_role: Option<Role>,
    #[description = "How many extra entries the bonus role gets"]
    #[min = 1]
    #[max = 10]
    bonus_entries: Option<i64>,
    #[description = "Minimum server level to enter"]
    #[min = 0]
    min_level: Option<i64>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let target = channel.map(|c| c.id).unwrap_or_else(|| ctx.channel_id());

    let Some(parsed) = time::parse_duration(&duration) else {
        ctx.send(poise::CreateReply::default().embed(embed::err(
            "I could not read that duration. Try `2h`, `3d` or `1h30m`.",
        )))
        .await?;
        return Ok(());
    };
    let ends_at = Utc::now() + chrono::Duration::seconds(parsed.as_secs() as i64);
    let winners = winners.unwrap_or(1);

    let giveaway_id = sqlx::query(
        "INSERT INTO giveaways
            (guild_id, channel_id, host_id, prize, winners, ends_at,
             required_role, bonus_role, bonus_entries, min_level)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(guild_id)
    .bind(target.get() as i64)
    .bind(ctx.author().id.get() as i64)
    .bind(&prize)
    .bind(winners)
    .bind(time::to_sql(ends_at))
    .bind(required_role.as_ref().map(|r| r.id.get() as i64))
    .bind(bonus_role.as_ref().map(|r| r.id.get() as i64))
    .bind(bonus_entries.unwrap_or(0))
    .bind(min_level.unwrap_or(0))
    .execute(&ctx.data().db)
    .await?
    .last_insert_rowid();

    let message = target
        .send_message(
            ctx.http(),
            CreateMessage::new()
                .embed(render(
                    &prize,
                    ctx.author().id.get() as i64,
                    winners,
                    Some(ends_at),
                    required_role.as_ref().map(|r| r.id.get() as i64),
                    bonus_role.as_ref().map(|r| r.id.get() as i64),
                    bonus_entries.unwrap_or(0),
                    min_level.unwrap_or(0),
                    0,
                    None,
                ))
                .components(components(giveaway_id, false, 0)),
        )
        .await?;

    sqlx::query("UPDATE giveaways SET message_id = ? WHERE id = ?")
        .bind(message.id.get() as i64)
        .bind(giveaway_id)
        .execute(&ctx.data().db)
        .await?;

    ctx.send(
        poise::CreateReply::default()
            .embed(embed::ok(format!(
                "Giveaway **#{giveaway_id}** started in <#{}>, ending {}.",
                target.get(),
                time::discord_ts(ends_at, 'R')
            )))
            .ephemeral(true),
    )
    .await?;
    Ok(())
}

/// End a giveaway now and draw the winners.
#[poise::command(slash_command, rename = "end", guild_only)]
pub async fn giveaway_end(
    ctx: Ctx<'_>,
    #[description = "Giveaway number"] giveaway_id: i64,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let exists: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM giveaways WHERE id = ? AND guild_id = ?")
            .bind(giveaway_id)
            .bind(guild_id)
            .fetch_one(&ctx.data().db)
            .await?;
    if exists == 0 {
        ctx.send(poise::CreateReply::default().embed(embed::err("No giveaway with that number.")))
            .await?;
        return Ok(());
    }

    ctx.defer_ephemeral().await?;
    let drawn = draw(ctx.serenity_context(), ctx.data(), giveaway_id, false).await;

    ctx.send(
        poise::CreateReply::default()
            .embed(embed::ok(match drawn {
                Some(winners) if !winners.is_empty() => format!(
                    "Drew {} winner(s).",
                    winners.len()
                ),
                _ => "Nobody entered, so there is no winner.".to_string(),
            }))
            .ephemeral(true),
    )
    .await?;
    Ok(())
}

/// Draw replacement winners for a finished giveaway.
#[poise::command(slash_command, rename = "reroll", guild_only)]
pub async fn giveaway_reroll(
    ctx: Ctx<'_>,
    #[description = "Giveaway number"] giveaway_id: i64,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let ended: Option<i64> =
        sqlx::query_scalar("SELECT ended FROM giveaways WHERE id = ? AND guild_id = ?")
            .bind(giveaway_id)
            .bind(guild_id)
            .fetch_optional(&ctx.data().db)
            .await?;

    match ended {
        None => {
            ctx.send(
                poise::CreateReply::default()
                    .embed(embed::err("No giveaway with that number.")),
            )
            .await?;
        }
        Some(0) => {
            ctx.send(poise::CreateReply::default().embed(embed::err(
                "That giveaway is still running. End it first with `/giveaway end`.",
            )))
            .await?;
        }
        Some(_) => {
            ctx.defer_ephemeral().await?;
            let drawn = draw(ctx.serenity_context(), ctx.data(), giveaway_id, true).await;
            ctx.send(
                poise::CreateReply::default()
                    .embed(embed::ok(match drawn {
                        Some(winners) if !winners.is_empty() => {
                            format!("Rerolled: {} new winner(s).", winners.len())
                        }
                        _ => "There are not enough entrants left to reroll.".to_string(),
                    }))
                    .ephemeral(true),
            )
            .await?;
        }
    }
    Ok(())
}

/// Giveaways that are still running.
#[poise::command(slash_command, rename = "list", guild_only)]
pub async fn giveaway_list(ctx: Ctx<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let rows: Vec<(i64, String, i64, String, i64)> = sqlx::query_as(
        "SELECT g.id, g.prize, g.winners, g.ends_at,
                (SELECT COALESCE(SUM(entries), 0) FROM giveaway_entries e WHERE e.giveaway_id = g.id)
         FROM giveaways g WHERE g.guild_id = ? AND g.ended = 0 ORDER BY g.ends_at ASC",
    )
    .bind(guild_id)
    .fetch_all(&ctx.data().db)
    .await?;

    let body = if rows.is_empty() {
        "No giveaways running.".to_string()
    } else {
        rows.iter()
            .map(|(id, prize, winners, ends_at, entries)| {
                let ends = time::from_sql(ends_at)
                    .map(|t| time::discord_ts(t, 'R'))
                    .unwrap_or_default();
                format!("**#{id}** {prize} · {winners} winner(s) · {entries} entries · ends {ends}")
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    ctx.send(
        poise::CreateReply::default()
            .embed(embed::titled("🎉 Active giveaways", embed::BRAND).description(body)),
    )
    .await?;
    Ok(())
}

/// Cancel a giveaway without picking a winner.
#[poise::command(slash_command, rename = "cancel", guild_only)]
pub async fn giveaway_cancel(
    ctx: Ctx<'_>,
    #[description = "Giveaway number"] giveaway_id: i64,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let found: Option<(i64, Option<i64>, String)> = sqlx::query_as(
        "SELECT channel_id, message_id, prize FROM giveaways WHERE id = ? AND guild_id = ?",
    )
    .bind(giveaway_id)
    .bind(guild_id)
    .fetch_optional(&ctx.data().db)
    .await?;

    let Some((channel_id, message_id, prize)) = found else {
        ctx.send(poise::CreateReply::default().embed(embed::err("No giveaway with that number.")))
            .await?;
        return Ok(());
    };

    sqlx::query("UPDATE giveaways SET ended = 1 WHERE id = ?")
        .bind(giveaway_id)
        .execute(&ctx.data().db)
        .await?;

    if let Some(message_id) = message_id {
        let _ = ChannelId::new(channel_id as u64)
            .edit_message(
                ctx.http(),
                MessageId::new(message_id as u64),
                EditMessage::new()
                    .embed(
                        embed::titled(format!("🚫 {prize}"), embed::DANGER)
                            .description("This giveaway was cancelled."),
                    )
                    .components(Vec::new()),
            )
            .await;
    }

    ctx.send(
        poise::CreateReply::default()
            .embed(embed::ok("Giveaway cancelled."))
            .ephemeral(true),
    )
    .await?;
    Ok(())
}

/// Pick winners. `reroll` excludes anyone who already won.
pub async fn draw(
    sctx: &serenity::Context,
    data: &Data,
    giveaway_id: i64,
    reroll: bool,
) -> Option<Vec<i64>> {
    let row: (i64, i64, Option<i64>, String, i64, i64, String) = sqlx::query_as(
        "SELECT guild_id, channel_id, message_id, prize, winners, host_id, last_winners
         FROM giveaways WHERE id = ?",
    )
    .bind(giveaway_id)
    .fetch_optional(&data.db)
    .await
    .ok()
    .flatten()?;
    let (guild_id, channel_id, message_id, prize, winner_count, host_id, previous) = row;

    let entries: Vec<(i64, i64)> =
        sqlx::query_as("SELECT user_id, entries FROM giveaway_entries WHERE giveaway_id = ?")
            .bind(giveaway_id)
            .fetch_all(&data.db)
            .await
            .unwrap_or_default();

    let excluded = crate::db::csv_ids(&previous);

    // Weighted pool: a member with bonus entries appears several times.
    let mut pool: Vec<i64> = Vec::new();
    for (user_id, count) in &entries {
        if reroll && excluded.contains(user_id) {
            continue;
        }
        for _ in 0..(*count).max(1) {
            pool.push(*user_id);
        }
    }

    let mut winners: Vec<i64> = Vec::new();
    {
        use rand::seq::SliceRandom;
        let mut rng = rand::thread_rng();
        pool.shuffle(&mut rng);
        for candidate in pool {
            if winners.len() as i64 >= winner_count {
                break;
            }
            if !winners.contains(&candidate) {
                winners.push(candidate);
            }
        }
    }

    let total_entries: i64 = entries.iter().map(|(_, count)| count.max(&1)).sum();
    let result = if winners.is_empty() {
        "No valid entries.".to_string()
    } else {
        format!(
            "{} {}",
            winners
                .iter()
                .map(|id| format!("<@{id}>"))
                .collect::<Vec<_>>()
                .join(", "),
            if winners.len() == 1 { "won!" } else { "won!" }
        )
    };

    let mut stored = excluded.clone();
    stored.extend(winners.iter().copied());
    let _ = sqlx::query("UPDATE giveaways SET ended = 1, last_winners = ? WHERE id = ?")
        .bind(crate::db::ids_csv(&stored))
        .bind(giveaway_id)
        .execute(&data.db)
        .await;

    let channel = ChannelId::new(channel_id as u64);
    let ends_at: Option<String> = sqlx::query_scalar("SELECT ends_at FROM giveaways WHERE id = ?")
        .bind(giveaway_id)
        .fetch_optional(&data.db)
        .await
        .ok()
        .flatten();

    if let Some(message_id) = message_id {
        let _ = channel
            .edit_message(
                &sctx.http,
                MessageId::new(message_id as u64),
                EditMessage::new()
                    .embed(render(
                        &prize,
                        host_id,
                        winner_count,
                        ends_at.as_deref().and_then(time::from_sql),
                        None,
                        None,
                        0,
                        0,
                        total_entries,
                        Some(&result),
                    ))
                    .components(Vec::new()),
            )
            .await;
    }

    let announcement = if winners.is_empty() {
        format!("🎉 The giveaway for **{prize}** ended with no valid entries.")
    } else {
        format!(
            "🎉 Congratulations {} — you won **{prize}**!",
            winners
                .iter()
                .map(|id| format!("<@{id}>"))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let _ = channel
        .send_message(&sctx.http, CreateMessage::new().content(announcement))
        .await;

    // DM each winner so they actually notice.
    for winner in &winners {
        if let Ok(dm) = serenity::UserId::new(*winner as u64)
            .create_dm_channel(sctx)
            .await
        {
            let _ = dm
                .send_message(
                    &sctx.http,
                    CreateMessage::new().embed(
                        embed::titled("🎉 You won a giveaway!", embed::SUCCESS).description(format!(
                            "You won **{prize}** in **{}**.\nThe host is <@{host_id}>.",
                            crate::modules::moderation::guild_name(
                                sctx,
                                serenity::GuildId::new(guild_id as u64)
                            )
                        )),
                    ),
                )
                .await;
        }
    }

    Some(winners)
}

/// Handle the Enter button.
pub async fn handle_component(
    sctx: &serenity::Context,
    data: &Data,
    interaction: &ComponentInteraction,
) -> Result<bool, Error> {
    let Some(rest) = interaction.data.custom_id.strip_prefix("giveaway:") else {
        return Ok(false);
    };
    let mut parts = rest.split(':');
    let giveaway_id: i64 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let action = parts.next().unwrap_or("");
    if action != "enter" {
        return Ok(true);
    }

    let row: Option<(i64, i64, Option<i64>, Option<i64>, i64, i64, String, i64, i64)> =
        sqlx::query_as(
            "SELECT ended, min_level, required_role, bonus_role, bonus_entries, winners,
                    prize, host_id, guild_id
             FROM giveaways WHERE id = ?",
        )
        .bind(giveaway_id)
        .fetch_optional(&data.db)
        .await?;

    let Some((
        ended,
        min_level,
        required_role,
        bonus_role,
        bonus_entries,
        winner_count,
        prize,
        host_id,
        guild_id,
    )) = row
    else {
        return Ok(true);
    };

    if ended == 1 {
        respond(sctx, interaction, "This giveaway has already ended.").await?;
        return Ok(true);
    }

    let member_roles: Vec<i64> = interaction
        .member
        .as_ref()
        .map(|m| m.roles.iter().map(|r| r.get() as i64).collect())
        .unwrap_or_default();

    if let Some(role) = required_role {
        if !member_roles.contains(&role) {
            respond(
                sctx,
                interaction,
                &format!("You need <@&{role}> to enter this giveaway."),
            )
            .await?;
            return Ok(true);
        }
    }

    if min_level > 0 {
        let level: i64 = sqlx::query_scalar(
            "SELECT level FROM levels WHERE guild_id = ? AND user_id = ?",
        )
        .bind(guild_id)
        .bind(interaction.user.id.get() as i64)
        .fetch_optional(&data.db)
        .await?
        .unwrap_or(0);
        if level < min_level {
            respond(
                sctx,
                interaction,
                &format!("You need to be level **{min_level}** to enter. You are level **{level}**."),
            )
            .await?;
            return Ok(true);
        }
    }

    let mut entries = 1i64;
    if let Some(role) = bonus_role {
        if member_roles.contains(&role) {
            entries += bonus_entries.max(0);
        }
    }

    let already: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM giveaway_entries WHERE giveaway_id = ? AND user_id = ?",
    )
    .bind(giveaway_id)
    .bind(interaction.user.id.get() as i64)
    .fetch_one(&data.db)
    .await?;

    let note = if already > 0 {
        sqlx::query("DELETE FROM giveaway_entries WHERE giveaway_id = ? AND user_id = ?")
            .bind(giveaway_id)
            .bind(interaction.user.id.get() as i64)
            .execute(&data.db)
            .await?;
        "You left the giveaway.".to_string()
    } else {
        sqlx::query(
            "INSERT INTO giveaway_entries (giveaway_id, user_id, entries) VALUES (?, ?, ?)",
        )
        .bind(giveaway_id)
        .bind(interaction.user.id.get() as i64)
        .bind(entries)
        .execute(&data.db)
        .await?;
        if entries > 1 {
            format!("You are in, with **{entries}** entries. Good luck!")
        } else {
            "You are in. Good luck!".to_string()
        }
    };

    respond(sctx, interaction, &note).await?;

    // Keep the entry counter on the button fresh.
    let total: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(entries), 0) FROM giveaway_entries WHERE giveaway_id = ?",
    )
    .bind(giveaway_id)
    .fetch_one(&data.db)
    .await
    .unwrap_or(0);
    let unique: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM giveaway_entries WHERE giveaway_id = ?")
            .bind(giveaway_id)
            .fetch_one(&data.db)
            .await
            .unwrap_or(0);

    let ends_at: Option<String> = sqlx::query_scalar("SELECT ends_at FROM giveaways WHERE id = ?")
        .bind(giveaway_id)
        .fetch_optional(&data.db)
        .await?;

    let mut message = interaction.message.clone();
    let _ = message
        .edit(
            sctx,
            EditMessage::new()
                .embed(render(
                    &prize,
                    host_id,
                    winner_count,
                    ends_at.as_deref().and_then(time::from_sql),
                    required_role,
                    bonus_role,
                    bonus_entries,
                    min_level,
                    unique,
                    None,
                ))
                .components(components(giveaway_id, false, total)),
        )
        .await;

    Ok(true)
}

async fn respond(
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

/// End giveaways whose time is up.
pub async fn sweep(sctx: &serenity::Context, data: &Data) {
    let due: Vec<i64> =
        sqlx::query_scalar("SELECT id FROM giveaways WHERE ended = 0 AND ends_at <= ?")
            .bind(time::sql_now())
            .fetch_all(&data.db)
            .await
            .unwrap_or_default();

    for giveaway_id in due {
        draw(sctx, data, giveaway_id, false).await;
    }
}
