//! Confessions, the quote collection, community challenges, member milestones
//! and the timed quiz game.

use chrono::Utc;
use poise::serenity_prelude as serenity;
use serenity::{ChannelId, CreateMessage, GuildChannel, GuildId, Role, User};

use crate::db;
use crate::state::{Ctx, Data, Error};
use crate::util::{embed, perms, text, time};

// ===========================================================================
// Confessions
// ===========================================================================

/// Post something anonymously.
#[poise::command(
    slash_command,
    guild_only,
    category = "Community",
    subcommands("confess_send", "confess_channel", "confess_reveal", "confess_ban")
)]
pub async fn confess(_ctx: Ctx<'_>) -> Result<(), Error> {
    Ok(())
}

/// Send an anonymous confession.
#[poise::command(slash_command, rename = "send", guild_only, ephemeral)]
pub async fn confess_send(
    ctx: Ctx<'_>,
    #[description = "What do you want to say?"] message: String,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let author = ctx.author().id.get() as i64;

    // The author is stored so staff can act on abuse, and is never shown.
    let banned: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM confessions WHERE guild_id = ? AND user_id = ? AND banned = 1",
    )
    .bind(guild_id)
    .bind(author)
    .fetch_one(&ctx.data().db)
    .await?;

    if banned > 0 {
        ctx.send(
            poise::CreateReply::default()
                .embed(embed::err("You are blocked from using confessions here."))
                .ephemeral(true),
        )
        .await?;
        return Ok(());
    }

    let Some(raw) = db::kv_get(&ctx.data().db, guild_id, "confession_channel").await else {
        ctx.send(
            poise::CreateReply::default()
                .embed(embed::err("Confessions are not set up in this server."))
                .ephemeral(true),
        )
        .await?;
        return Ok(());
    };
    let Ok(channel_id) = raw.parse::<u64>() else {
        return Ok(());
    };

    let number = db::next_number(&ctx.data().db, "confessions", "number", guild_id).await?;

    let posted = ChannelId::new(channel_id)
        .send_message(
            ctx.http(),
            CreateMessage::new().embed(
                embed::titled(format!("Confession #{number}"), 0x9B59B6)
                    .description(text::truncate(&message, 2000))
                    .footer(embed::footer(
                        "Sent anonymously · /confess send to post your own",
                    )),
            ),
        )
        .await?;

    sqlx::query(
        "INSERT INTO confessions (guild_id, number, user_id, content, message_id)
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(guild_id)
    .bind(number)
    .bind(author)
    .bind(&message)
    .bind(posted.id.get() as i64)
    .execute(&ctx.data().db)
    .await?;

    ctx.send(
        poise::CreateReply::default()
            .embed(embed::ok(format!(
                "Posted as confession **#{number}**. Your name is not shown."
            )))
            .ephemeral(true),
    )
    .await?;
    Ok(())
}

/// Where confessions are posted.
#[poise::command(slash_command, rename = "channel", guild_only, check = "perms::admin_only")]
pub async fn confess_channel(
    ctx: Ctx<'_>,
    #[description = "Confession channel"] channel: GuildChannel,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    db::kv_set(
        &ctx.data().db,
        guild_id,
        "confession_channel",
        &channel.id.get().to_string(),
    )
    .await?;
    ctx.send(poise::CreateReply::default().embed(embed::ok(format!(
        "Confessions will be posted in <#{}>.",
        channel.id.get()
    ))))
    .await?;
    Ok(())
}

/// Reveal who sent a confession. Admin only, logged, for abuse handling.
#[poise::command(slash_command, rename = "reveal", guild_only, check = "perms::admin_only")]
pub async fn confess_reveal(
    ctx: Ctx<'_>,
    #[description = "Confession number"] number: i64,
    #[description = "Why do you need to know?"] reason: String,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let found: Option<(i64, String)> = sqlx::query_as(
        "SELECT user_id, content FROM confessions WHERE guild_id = ? AND number = ?",
    )
    .bind(guild_id)
    .bind(number)
    .fetch_optional(&ctx.data().db)
    .await?;

    let Some((user_id, content)) = found else {
        ctx.send(
            poise::CreateReply::default()
                .embed(embed::err("No confession with that number."))
                .ephemeral(true),
        )
        .await?;
        return Ok(());
    };

    // Every reveal goes in the mod log, so this cannot be done quietly.
    crate::logs::send(
        ctx.serenity_context(),
        &ctx.data().db,
        guild_id,
        crate::logs::Log::Mod,
        embed::titled("🔎 Confession author revealed", embed::WARN)
            .field("Confession", format!("#{number}"), true)
            .field("Revealed by", format!("<@{}>", ctx.author().id.get()), true)
            .field("Reason", text::truncate(&reason, 500), false),
    )
    .await;

    ctx.send(
        poise::CreateReply::default()
            .embed(
                embed::titled(format!("Confession #{number}"), embed::WARN)
                    .field("Author", format!("<@{user_id}>"), true)
                    .field("Content", text::truncate(&content, 1000), false)
                    .footer(embed::footer("This reveal has been written to the mod log.")),
            )
            .ephemeral(true),
    )
    .await?;
    Ok(())
}

/// Block someone from using confessions.
#[poise::command(slash_command, rename = "ban", guild_only, check = "perms::admin_only")]
pub async fn confess_ban(
    ctx: Ctx<'_>,
    #[description = "Confession number whose author should be blocked"] number: i64,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let author: Option<i64> =
        sqlx::query_scalar("SELECT user_id FROM confessions WHERE guild_id = ? AND number = ?")
            .bind(guild_id)
            .bind(number)
            .fetch_optional(&ctx.data().db)
            .await?;

    let Some(author) = author else {
        ctx.send(
            poise::CreateReply::default()
                .embed(embed::err("No confession with that number."))
                .ephemeral(true),
        )
        .await?;
        return Ok(());
    };

    sqlx::query("UPDATE confessions SET banned = 1 WHERE guild_id = ? AND user_id = ?")
        .bind(guild_id)
        .bind(author)
        .execute(&ctx.data().db)
        .await?;

    ctx.send(
        poise::CreateReply::default()
            .embed(embed::ok(
                "That author is blocked from confessions. Their identity was not shown here.",
            ))
            .ephemeral(true),
    )
    .await?;
    Ok(())
}

// ===========================================================================
// Quotes
// ===========================================================================

/// Save and recall memorable messages.
#[poise::command(
    slash_command,
    guild_only,
    category = "Community",
    subcommands("quote_add", "quote_get", "quote_random", "quote_list", "quote_remove")
)]
pub async fn quote(_ctx: Ctx<'_>) -> Result<(), Error> {
    Ok(())
}

/// Save a quote.
#[poise::command(slash_command, rename = "add", guild_only)]
pub async fn quote_add(
    ctx: Ctx<'_>,
    #[description = "Who said it"] author: User,
    #[description = "What they said"] content: String,
    #[description = "Link to the original message"] link: Option<String>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let number = db::next_number(&ctx.data().db, "quotes", "number", guild_id).await?;

    sqlx::query(
        "INSERT INTO quotes (guild_id, number, author_id, content, added_by, link)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(guild_id)
    .bind(number)
    .bind(author.id.get() as i64)
    .bind(text::truncate(&content, 1500))
    .bind(ctx.author().id.get() as i64)
    .bind(&link)
    .execute(&ctx.data().db)
    .await?;

    ctx.send(poise::CreateReply::default().embed(embed::ok(format!(
        "Saved as quote **#{number}**."
    ))))
    .await?;
    Ok(())
}

fn quote_embed(number: i64, author_id: i64, content: &str, link: Option<&str>) -> serenity::CreateEmbed {
    let mut e = embed::titled(format!("Quote #{number}"), embed::BRAND).description(format!(
        "> {}\n\n— <@{author_id}>",
        content.replace('\n', "\n> ")
    ));
    if let Some(link) = link {
        if !link.trim().is_empty() {
            e = e.field("Source", format!("[original message]({link})"), false);
        }
    }
    e
}

/// Show a specific quote.
#[poise::command(slash_command, rename = "get", guild_only)]
pub async fn quote_get(
    ctx: Ctx<'_>,
    #[description = "Quote number"] number: i64,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let found: Option<(i64, String, Option<String>)> = sqlx::query_as(
        "SELECT author_id, content, link FROM quotes WHERE guild_id = ? AND number = ?",
    )
    .bind(guild_id)
    .bind(number)
    .fetch_optional(&ctx.data().db)
    .await?;

    match found {
        Some((author_id, content, link)) => {
            ctx.send(poise::CreateReply::default().embed(quote_embed(
                number,
                author_id,
                &content,
                link.as_deref(),
            )))
            .await?;
        }
        None => {
            ctx.send(poise::CreateReply::default().embed(embed::err("No quote with that number.")))
                .await?;
        }
    }
    Ok(())
}

/// A random quote.
#[poise::command(slash_command, rename = "random", guild_only)]
pub async fn quote_random(ctx: Ctx<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let found: Option<(i64, i64, String, Option<String>)> = sqlx::query_as(
        "SELECT number, author_id, content, link FROM quotes WHERE guild_id = ?
         ORDER BY RANDOM() LIMIT 1",
    )
    .bind(guild_id)
    .fetch_optional(&ctx.data().db)
    .await?;

    match found {
        Some((number, author_id, content, link)) => {
            ctx.send(poise::CreateReply::default().embed(quote_embed(
                number,
                author_id,
                &content,
                link.as_deref(),
            )))
            .await?;
        }
        None => {
            ctx.send(poise::CreateReply::default().embed(embed::warn(
                "No quotes saved yet. Add one with `/quote add`.",
            )))
            .await?;
        }
    }
    Ok(())
}

/// Browse the quote collection.
#[poise::command(slash_command, rename = "list", guild_only)]
pub async fn quote_list(
    ctx: Ctx<'_>,
    #[description = "Only quotes from this member"] author: Option<User>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;

    let rows: Vec<(i64, i64, String)> = match &author {
        Some(author) => {
            sqlx::query_as(
                "SELECT number, author_id, content FROM quotes
                 WHERE guild_id = ? AND author_id = ? ORDER BY number DESC LIMIT 15",
            )
            .bind(guild_id)
            .bind(author.id.get() as i64)
            .fetch_all(&ctx.data().db)
            .await?
        }
        None => {
            sqlx::query_as(
                "SELECT number, author_id, content FROM quotes
                 WHERE guild_id = ? ORDER BY number DESC LIMIT 15",
            )
            .bind(guild_id)
            .fetch_all(&ctx.data().db)
            .await?
        }
    };

    let body = if rows.is_empty() {
        "No quotes yet.".to_string()
    } else {
        rows.iter()
            .map(|(number, author_id, content)| {
                format!(
                    "**#{number}** <@{author_id}>: {}",
                    text::truncate(&text::escape_md(content), 120)
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    ctx.send(
        poise::CreateReply::default()
            .embed(embed::titled("💬 Quote collection", embed::BRAND).description(body)),
    )
    .await?;
    Ok(())
}

/// Delete a quote.
#[poise::command(slash_command, rename = "remove", guild_only, check = "perms::staff_only")]
pub async fn quote_remove(
    ctx: Ctx<'_>,
    #[description = "Quote number"] number: i64,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let result = sqlx::query("DELETE FROM quotes WHERE guild_id = ? AND number = ?")
        .bind(guild_id)
        .bind(number)
        .execute(&ctx.data().db)
        .await?;
    let body = if result.rows_affected() == 0 {
        embed::err("No quote with that number.")
    } else {
        embed::ok("Quote deleted.")
    };
    ctx.send(poise::CreateReply::default().embed(body)).await?;
    Ok(())
}

// ===========================================================================
// Challenges
// ===========================================================================

#[derive(Debug, poise::ChoiceParameter)]
pub enum GoalKind {
    #[name = "messages sent"]
    Messages,
    #[name = "minutes in voice"]
    Voice,
    #[name = "XP earned"]
    Xp,
    #[name = "trivia answers"]
    Trivia,
}

impl GoalKind {
    fn key(&self) -> &'static str {
        match self {
            GoalKind::Messages => "messages",
            GoalKind::Voice => "voice",
            GoalKind::Xp => "xp",
            GoalKind::Trivia => "trivia",
        }
    }
}

/// Community challenges with a shared goal.
#[poise::command(
    slash_command,
    guild_only,
    category = "Community",
    subcommands("challenge_create", "challenge_list", "challenge_progress", "challenge_end")
)]
pub async fn challenge(_ctx: Ctx<'_>) -> Result<(), Error> {
    Ok(())
}

#[poise::command(slash_command, rename = "create", guild_only, check = "perms::staff_only")]
pub async fn challenge_create(
    ctx: Ctx<'_>,
    #[description = "Challenge name"] name: String,
    #[description = "What counts towards it"] goal: GoalKind,
    #[description = "Target amount"]
    #[min = 1]
    target: i64,
    #[description = "How long it runs, e.g. 7d"] duration: String,
    #[description = "Description"] description: Option<String>,
    #[description = "Role awarded for completing it"] reward_role: Option<Role>,
    #[description = "XP awarded for completing it"]
    #[min = 0]
    reward_xp: Option<i64>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let Some(parsed) = time::parse_duration(&duration) else {
        ctx.send(
            poise::CreateReply::default()
                .embed(embed::err("I could not read that duration. Try `7d`.")),
        )
        .await?;
        return Ok(());
    };
    let ends_at = Utc::now() + chrono::Duration::seconds(parsed.as_secs() as i64);

    let id = sqlx::query(
        "INSERT INTO challenges
            (guild_id, name, description, goal_kind, goal_value, reward_role, reward_xp, ends_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(guild_id)
    .bind(&name)
    .bind(description.unwrap_or_default())
    .bind(goal.key())
    .bind(target)
    .bind(reward_role.as_ref().map(|r| r.id.get() as i64))
    .bind(reward_xp.unwrap_or(0))
    .bind(time::to_sql(ends_at))
    .execute(&ctx.data().db)
    .await?
    .last_insert_rowid();

    ctx.send(
        poise::CreateReply::default().embed(
            embed::titled("🏁 Challenge started", embed::BRAND)
                .description(format!("**{name}** (#{id})"))
                .field("Goal", format!("{target} {}", goal.key()), true)
                .field("Ends", time::full_and_relative(ends_at), false)
                .footer(embed::footer("Track it with /challenge progress")),
        ),
    )
    .await?;
    Ok(())
}

#[poise::command(slash_command, rename = "list", guild_only)]
pub async fn challenge_list(ctx: Ctx<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let rows: Vec<(i64, String, String, i64, String)> = sqlx::query_as(
        "SELECT id, name, goal_kind, goal_value, ends_at FROM challenges
         WHERE guild_id = ? AND ends_at > ? ORDER BY ends_at ASC",
    )
    .bind(guild_id)
    .bind(time::sql_now())
    .fetch_all(&ctx.data().db)
    .await?;

    let body = if rows.is_empty() {
        "No challenges running.".to_string()
    } else {
        rows.iter()
            .map(|(id, name, kind, target, ends_at)| {
                let ends = time::from_sql(ends_at)
                    .map(|t| time::discord_ts(t, 'R'))
                    .unwrap_or_default();
                format!("**#{id}** {name} — {target} {kind} · ends {ends}")
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    ctx.send(
        poise::CreateReply::default()
            .embed(embed::titled("🏁 Active challenges", embed::BRAND).description(body)),
    )
    .await?;
    Ok(())
}

#[poise::command(slash_command, rename = "progress", guild_only)]
pub async fn challenge_progress(
    ctx: Ctx<'_>,
    #[description = "Challenge number"] challenge_id: i64,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let found: Option<(String, String, i64, String)> = sqlx::query_as(
        "SELECT name, goal_kind, goal_value, ends_at FROM challenges
         WHERE id = ? AND guild_id = ?",
    )
    .bind(challenge_id)
    .bind(guild_id)
    .fetch_optional(&ctx.data().db)
    .await?;

    let Some((name, kind, target, ends_at)) = found else {
        ctx.send(poise::CreateReply::default().embed(embed::err("No challenge with that number.")))
            .await?;
        return Ok(());
    };

    let top: Vec<(i64, i64)> = sqlx::query_as(
        "SELECT user_id, value FROM challenge_progress
         WHERE challenge_id = ? ORDER BY value DESC LIMIT 10",
    )
    .bind(challenge_id)
    .fetch_all(&ctx.data().db)
    .await?;

    let mine: i64 = sqlx::query_scalar(
        "SELECT value FROM challenge_progress WHERE challenge_id = ? AND user_id = ?",
    )
    .bind(challenge_id)
    .bind(ctx.author().id.get() as i64)
    .fetch_optional(&ctx.data().db)
    .await?
    .unwrap_or(0);

    let board = if top.is_empty() {
        "Nobody has made progress yet.".to_string()
    } else {
        top.iter()
            .enumerate()
            .map(|(i, (user_id, value))| {
                format!("**{}.** <@{user_id}> — {value} / {target}", i + 1)
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    ctx.send(
        poise::CreateReply::default().embed(
            embed::titled(format!("🏁 {name}"), embed::BRAND)
                .field("Goal", format!("{target} {kind}"), true)
                .field(
                    "Ends",
                    time::from_sql(&ends_at)
                        .map(|t| time::discord_ts(t, 'R'))
                        .unwrap_or_default(),
                    true,
                )
                .field(
                    "Your progress",
                    format!(
                        "`{}` {mine} / {target}",
                        text::progress_bar(mine, target, 18)
                    ),
                    false,
                )
                .field("Leaderboard", board, false),
        ),
    )
    .await?;
    Ok(())
}

#[poise::command(slash_command, rename = "end", guild_only, check = "perms::staff_only")]
pub async fn challenge_end(
    ctx: Ctx<'_>,
    #[description = "Challenge number"] challenge_id: i64,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    sqlx::query("UPDATE challenges SET ends_at = ? WHERE id = ? AND guild_id = ?")
        .bind(time::sql_now())
        .bind(challenge_id)
        .bind(guild_id)
        .execute(&ctx.data().db)
        .await?;
    ctx.send(
        poise::CreateReply::default()
            .embed(embed::ok("Challenge ended. Rewards go out on the next sweep.")),
    )
    .await?;
    Ok(())
}

/// Add progress towards every running challenge of a given kind.
pub async fn bump_challenges(data: &Data, guild_id: i64, user_id: i64, kind: &str, amount: i64) {
    let running: Vec<(i64, i64)> = sqlx::query_as(
        "SELECT id, goal_value FROM challenges
         WHERE guild_id = ? AND goal_kind = ? AND starts_at <= ? AND ends_at > ?",
    )
    .bind(guild_id)
    .bind(kind)
    .bind(time::sql_now())
    .bind(time::sql_now())
    .fetch_all(&data.db)
    .await
    .unwrap_or_default();

    for (challenge_id, goal) in running {
        let _ = sqlx::query(
            "INSERT INTO challenge_progress (challenge_id, user_id, value) VALUES (?, ?, ?)
             ON CONFLICT(challenge_id, user_id) DO UPDATE SET value = challenge_progress.value + ?",
        )
        .bind(challenge_id)
        .bind(user_id)
        .bind(amount)
        .bind(amount)
        .execute(&data.db)
        .await;

        let _ = sqlx::query(
            "UPDATE challenge_progress SET completed = 1
             WHERE challenge_id = ? AND user_id = ? AND value >= ?",
        )
        .bind(challenge_id)
        .bind(user_id)
        .bind(goal)
        .execute(&data.db)
        .await;
    }
}

/// Hand out rewards for finished challenges.
pub async fn sweep_challenges(sctx: &serenity::Context, data: &Data) {
    let finished: Vec<(i64, i64, String, Option<i64>, i64)> = sqlx::query_as(
        "SELECT id, guild_id, name, reward_role, reward_xp FROM challenges
         WHERE announced = 0 AND ends_at <= ?",
    )
    .bind(time::sql_now())
    .fetch_all(&data.db)
    .await
    .unwrap_or_default();

    for (challenge_id, guild_id, name, reward_role, reward_xp) in finished {
        let winners: Vec<i64> = sqlx::query_scalar(
            "SELECT user_id FROM challenge_progress WHERE challenge_id = ? AND completed = 1",
        )
        .bind(challenge_id)
        .fetch_all(&data.db)
        .await
        .unwrap_or_default();

        for user_id in &winners {
            if let Some(role) = reward_role {
                let _ = crate::compat::add_role(
                    &sctx.http,
                    GuildId::new(guild_id as u64),
                    serenity::UserId::new(*user_id as u64),
                    serenity::RoleId::new(role as u64),
                )
                .await;
            }
            if reward_xp > 0 {
                let _ =
                    crate::modules::leveling::add_xp(&data.db, guild_id, *user_id, reward_xp).await;
            }
        }

        let body = if winners.is_empty() {
            format!("**{name}** finished with nobody reaching the goal.")
        } else {
            format!(
                "**{name}** is over. Completed by {}:\n{}",
                winners.len(),
                winners
                    .iter()
                    .take(30)
                    .map(|id| format!("<@{id}>"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };

        crate::logs::send(
            sctx,
            &data.db,
            guild_id,
            crate::logs::Log::Mod,
            embed::titled("🏁 Challenge finished", embed::SUCCESS).description(body),
        )
        .await;

        let _ = sqlx::query("UPDATE challenges SET announced = 1 WHERE id = ?")
            .bind(challenge_id)
            .execute(&data.db)
            .await;
    }
}

// ===========================================================================
// Milestones
// ===========================================================================

/// Announce when the server passes a round member count.
pub async fn check_member_milestone(sctx: &serenity::Context, data: &Data, guild_id: GuildId) {
    let guild_i64 = guild_id.get() as i64;
    let Some(count) = guild_id
        .to_guild_cached(&sctx.cache)
        .map(|g| g.member_count as i64)
    else {
        return;
    };

    // Round numbers worth shouting about.
    let step = match count {
        0..=999 => 100,
        1_000..=9_999 => 500,
        _ => 1_000,
    };
    let milestone = (count / step) * step;
    if milestone < step {
        return;
    }

    let already: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM milestones WHERE guild_id = ? AND kind = 'members' AND value = ?",
    )
    .bind(guild_i64)
    .bind(milestone)
    .fetch_one(&data.db)
    .await
    .unwrap_or(1);

    if already > 0 {
        return;
    }

    let _ = sqlx::query(
        "INSERT OR IGNORE INTO milestones (guild_id, kind, value) VALUES (?, 'members', ?)",
    )
    .bind(guild_i64)
    .bind(milestone)
    .execute(&data.db)
    .await;

    let channel = db::kv_get(&data.db, guild_i64, "milestone_channel")
        .await
        .and_then(|c| c.parse::<u64>().ok())
        .map(ChannelId::new);

    let announcement = embed::titled("🎊 Member milestone!", embed::SUCCESS).description(format!(
        "**{}** just passed **{milestone}** members. Thank you all for being here!",
        crate::modules::moderation::guild_name(sctx, guild_id)
    ));

    match channel {
        Some(channel) => {
            let _ = channel
                .send_message(&sctx.http, CreateMessage::new().embed(announcement))
                .await;
        }
        None => {
            crate::logs::send(sctx, &data.db, guild_i64, crate::logs::Log::Join, announcement)
                .await;
        }
    }
}

/// Where milestone and anniversary announcements go.
#[poise::command(
    slash_command,
    guild_only,
    category = "Community",
    check = "perms::admin_only"
)]
pub async fn milestonechannel(
    ctx: Ctx<'_>,
    #[description = "Announcement channel"] channel: GuildChannel,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    db::kv_set(
        &ctx.data().db,
        guild_id,
        "milestone_channel",
        &channel.id.get().to_string(),
    )
    .await?;
    ctx.send(poise::CreateReply::default().embed(embed::ok(format!(
        "Milestones and server anniversaries will be announced in <#{}>.",
        channel.id.get()
    ))))
    .await?;
    Ok(())
}

/// Announce the server's anniversary once a year.
pub async fn check_anniversary(sctx: &serenity::Context, data: &Data, guild_id: GuildId) {
    let guild_i64 = guild_id.get() as i64;
    let created = guild_id.created_at();
    let created_dt = chrono::DateTime::from_timestamp(created.unix_timestamp(), 0);
    let Some(created_dt) = created_dt else {
        return;
    };

    let now = Utc::now();
    use chrono::Datelike;
    if created_dt.month() != now.month() || created_dt.day() != now.day() {
        return;
    }
    let years = now.year() - created_dt.year();
    if years < 1 {
        return;
    }

    let already: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM milestones WHERE guild_id = ? AND kind = 'anniversary' AND value = ?",
    )
    .bind(guild_i64)
    .bind(years as i64)
    .fetch_one(&data.db)
    .await
    .unwrap_or(1);
    if already > 0 {
        return;
    }

    let _ = sqlx::query(
        "INSERT OR IGNORE INTO milestones (guild_id, kind, value) VALUES (?, 'anniversary', ?)",
    )
    .bind(guild_i64)
    .bind(years as i64)
    .execute(&data.db)
    .await;

    let announcement = embed::titled("🎂 Server anniversary!", embed::BRAND).description(format!(
        "**{}** is **{years}** year{} old today. Here is to many more.",
        crate::modules::moderation::guild_name(sctx, guild_id),
        if years == 1 { "" } else { "s" }
    ));

    let channel = db::kv_get(&data.db, guild_i64, "milestone_channel")
        .await
        .and_then(|c| c.parse::<u64>().ok())
        .map(ChannelId::new);

    match channel {
        Some(channel) => {
            let _ = channel
                .send_message(&sctx.http, CreateMessage::new().embed(announcement))
                .await;
        }
        None => {
            crate::logs::send(sctx, &data.db, guild_i64, crate::logs::Log::Join, announcement)
                .await;
        }
    }
}

// ===========================================================================
// Quiz: a timed run of several trivia questions
// ===========================================================================

/// Run a timed quiz of several questions.
#[poise::command(
    slash_command,
    guild_only,
    category = "Games",
    check = "perms::staff_only"
)]
pub async fn quiz(
    ctx: Ctx<'_>,
    #[description = "How many questions"]
    #[min = 2]
    #[max = 10]
    questions: Option<i64>,
    #[description = "Seconds per question"]
    #[min = 10]
    #[max = 120]
    seconds: Option<i64>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let channel = ctx.channel_id();
    let rounds = questions.unwrap_or(5);
    let per_question = seconds.unwrap_or(25);

    // Snapshot the scores so we can report what this round changed.
    let before: Vec<(i64, i64)> =
        sqlx::query_as("SELECT user_id, points FROM trivia_scores WHERE guild_id = ?")
            .bind(guild_id)
            .fetch_all(&ctx.data().db)
            .await?;

    ctx.send(
        poise::CreateReply::default().embed(
            embed::titled("🧠 Quiz starting", embed::BRAND).description(format!(
                "**{rounds}** questions, **{per_question}s** each. Answer with the buttons - first answer counts."
            )),
        ),
    )
    .await?;

    // The quiz runs in the background so the command returns straight away.
    let sctx = ctx.serenity_context().clone();
    let pool = ctx.data().db.clone();
    tokio::spawn(async move {
        for round in 1..=rounds {
            let row: Option<(i64, String, String, String)> = sqlx::query_as(
                "SELECT id, question, answer, choices FROM trivia_questions
                 WHERE guild_id = 0 OR guild_id = ? ORDER BY RANDOM() LIMIT 1",
            )
            .bind(guild_id)
            .fetch_optional(&pool)
            .await
            .ok()
            .flatten();

            let Some((question_id, question, answer, raw_choices)) = row else {
                break;
            };

            let mut choices: Vec<String> =
                serde_json::from_str(&raw_choices).unwrap_or_else(|_| vec![answer.clone()]);
            {
                use rand::seq::SliceRandom;
                let mut rng = rand::thread_rng();
                choices.shuffle(&mut rng);
            }
            let correct_index = choices.iter().position(|c| *c == answer).unwrap_or(0);
            let nonce: u64 = {
                use rand::Rng;
                rand::thread_rng().gen()
            };

            let _ = db::kv_set(
                &pool,
                guild_id,
                &format!("trivia.{nonce}"),
                &format!("{question_id}|{correct_index}|{}", choices.join("\u{1f}")),
            )
            .await;

            let labels = ["🇦", "🇧", "🇨", "🇩"];
            let body = choices
                .iter()
                .enumerate()
                .map(|(i, choice)| format!("{} {choice}", labels.get(i).copied().unwrap_or("▫️")))
                .collect::<Vec<_>>()
                .join("\n");

            let buttons: Vec<serenity::CreateButton> = choices
                .iter()
                .enumerate()
                .take(4)
                .map(|(i, _)| {
                    serenity::CreateButton::new(format!("trivia:{nonce}:{i}"))
                        .label(labels.get(i).copied().unwrap_or("?"))
                        .style(serenity::ButtonStyle::Secondary)
                })
                .collect();

            let _ = channel
                .send_message(
                    &sctx.http,
                    CreateMessage::new()
                        .embed(
                            embed::titled(
                                format!("Question {round} of {rounds}"),
                                embed::BRAND,
                            )
                            .description(format!("**{question}**\n\n{body}")),
                        )
                        .components(vec![serenity::CreateActionRow::Buttons(buttons)]),
                )
                .await;

            tokio::time::sleep(std::time::Duration::from_secs(per_question as u64)).await;

            let _ = channel
                .send_message(
                    &sctx.http,
                    CreateMessage::new()
                        .content(format!("⏱️ Time! The answer was **{answer}**.")),
                )
                .await;
        }

        // ---- round results -----------------------------------------------
        let after: Vec<(i64, i64)> =
            sqlx::query_as("SELECT user_id, points FROM trivia_scores WHERE guild_id = ?")
                .bind(guild_id)
                .fetch_all(&pool)
                .await
                .unwrap_or_default();

        let mut gains: Vec<(i64, i64)> = after
            .iter()
            .map(|(user_id, points)| {
                let previous = before
                    .iter()
                    .find(|(id, _)| id == user_id)
                    .map(|(_, p)| *p)
                    .unwrap_or(0);
                (*user_id, points - previous)
            })
            .filter(|(_, gain)| *gain > 0)
            .collect();
        gains.sort_by(|a, b| b.1.cmp(&a.1));

        let body = if gains.is_empty() {
            "Nobody scored this round.".to_string()
        } else {
            gains
                .iter()
                .take(10)
                .enumerate()
                .map(|(i, (user_id, gain))| {
                    let medal = match i {
                        0 => "🥇",
                        1 => "🥈",
                        2 => "🥉",
                        _ => "▫️",
                    };
                    format!("{medal} <@{user_id}> — **{}** correct", gain / 10)
                })
                .collect::<Vec<_>>()
                .join("\n")
        };

        let _ = channel
            .send_message(
                &sctx.http,
                CreateMessage::new().embed(
                    embed::titled("🧠 Quiz over", embed::SUCCESS).description(body),
                ),
            )
            .await;
    });

    Ok(())
}

pub fn commands() -> Vec<poise::Command<Data, Error>> {
    vec![
        confess(),
        quote(),
        challenge(),
        milestonechannel(),
        quiz(),
    ]
}
