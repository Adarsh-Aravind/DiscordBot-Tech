//! Analytics: counters written as events happen, and the commands that read
//! them back.

use poise::serenity_prelude as serenity;
use serenity::{GuildChannel, Message, User};

use crate::db::Db;
use crate::state::{Ctx, Data, Error};
use crate::util::rankcard::thousands;
use crate::util::{embed, perms, text, time};

// ===========================================================================
// Recording
// ===========================================================================

/// Add to one of the per-day counters. `column` is always a literal.
pub async fn bump_daily(db: &Db, guild_id: i64, column: &str, amount: i64) {
    let day = time::today();
    let sql = format!(
        "INSERT INTO stats_daily (guild_id, day, {column}) VALUES (?, ?, ?)
         ON CONFLICT(guild_id, day) DO UPDATE SET {column} = {column} + ?"
    );
    let _ = sqlx::query(&sql)
        .bind(guild_id)
        .bind(&day)
        .bind(amount)
        .bind(amount)
        .execute(db)
        .await;
}

/// Everything a message contributes to the statistics tables.
pub async fn record_message(data: &Data, message: &Message) {
    let Some(guild_id) = message.guild_id else {
        return;
    };
    let guild_i64 = guild_id.get() as i64;
    let user_i64 = message.author.id.get() as i64;
    let day = time::today();
    let hour = chrono::Utc::now().format("%H").to_string().parse::<i64>().unwrap_or(0);

    bump_daily(&data.db, guild_i64, "messages", 1).await;

    let _ = sqlx::query(
        "INSERT INTO member_activity (guild_id, user_id, day, messages) VALUES (?, ?, ?, 1)
         ON CONFLICT(guild_id, user_id, day) DO UPDATE SET messages = messages + 1",
    )
    .bind(guild_i64)
    .bind(user_i64)
    .bind(&day)
    .execute(&data.db)
    .await;

    let _ = sqlx::query(
        "INSERT INTO channel_activity (guild_id, channel_id, day, messages) VALUES (?, ?, ?, 1)
         ON CONFLICT(guild_id, channel_id, day) DO UPDATE SET messages = messages + 1",
    )
    .bind(guild_i64)
    .bind(message.channel_id.get() as i64)
    .bind(&day)
    .execute(&data.db)
    .await;

    let _ = sqlx::query(
        "INSERT INTO hourly_activity (guild_id, hour, messages) VALUES (?, ?, 1)
         ON CONFLICT(guild_id, hour) DO UPDATE SET messages = messages + 1",
    )
    .bind(guild_i64)
    .bind(hour)
    .execute(&data.db)
    .await;

    let _ = sqlx::query("UPDATE members SET last_seen = ? WHERE guild_id = ? AND user_id = ?")
        .bind(time::sql_now())
        .bind(guild_i64)
        .bind(user_i64)
        .execute(&data.db)
        .await;

    crate::modules::community::misc::bump_challenges(data, guild_i64, user_i64, "messages", 1)
        .await;
}

pub async fn record_voice(db: &Db, guild_id: i64, user_id: i64, seconds: i64) {
    let day = time::today();
    bump_daily(db, guild_id, "voice_secs", seconds).await;

    let _ = sqlx::query(
        "INSERT INTO member_activity (guild_id, user_id, day, voice_secs) VALUES (?, ?, ?, ?)
         ON CONFLICT(guild_id, user_id, day) DO UPDATE SET voice_secs = voice_secs + ?",
    )
    .bind(guild_id)
    .bind(user_id)
    .bind(&day)
    .bind(seconds)
    .bind(seconds)
    .execute(db)
    .await;

    let hour = chrono::Utc::now()
        .format("%H")
        .to_string()
        .parse::<i64>()
        .unwrap_or(0);
    let _ = sqlx::query(
        "INSERT INTO hourly_activity (guild_id, hour, voice) VALUES (?, ?, ?)
         ON CONFLICT(guild_id, hour) DO UPDATE SET voice = voice + ?",
    )
    .bind(guild_id)
    .bind(hour)
    .bind(seconds)
    .bind(seconds)
    .execute(db)
    .await;
}

/// Count a command invocation.
pub async fn record_command(db: &Db, guild_id: i64, command: &str, failed: bool) {
    let _ = sqlx::query(
        "INSERT INTO command_usage (guild_id, command, uses, errors, last_use)
         VALUES (?, ?, 1, ?, datetime('now'))
         ON CONFLICT(guild_id, command) DO UPDATE SET
            uses = uses + 1,
            errors = errors + ?,
            last_use = datetime('now')",
    )
    .bind(guild_id)
    .bind(command)
    .bind(i64::from(failed))
    .bind(i64::from(failed))
    .execute(db)
    .await;

    bump_daily(db, guild_id, "commands", 1).await;
}

/// Store the member count once a day so growth can be charted.
pub async fn snapshot_member_count(db: &Db, guild_id: i64, count: i64) {
    let _ = sqlx::query(
        "INSERT INTO stats_daily (guild_id, day, member_count) VALUES (?, ?, ?)
         ON CONFLICT(guild_id, day) DO UPDATE SET member_count = excluded.member_count",
    )
    .bind(guild_id)
    .bind(time::today())
    .bind(count)
    .execute(db)
    .await;
}

// ===========================================================================
// Helpers
// ===========================================================================

async fn active_members(db: &Db, guild_id: i64, days: i64) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(DISTINCT user_id) FROM member_activity
         WHERE guild_id = ? AND day >= ?",
    )
    .bind(guild_id)
    .bind(time::day_offset(days))
    .fetch_one(db)
    .await
    .unwrap_or(0)
}

async fn sum_since(db: &Db, guild_id: i64, column: &str, days: i64) -> i64 {
    let sql = format!(
        "SELECT COALESCE(SUM({column}), 0) FROM stats_daily WHERE guild_id = ? AND day >= ?"
    );
    sqlx::query_scalar(&sql)
        .bind(guild_id)
        .bind(time::day_offset(days))
        .fetch_one(db)
        .await
        .unwrap_or(0)
}

fn sparkline(values: &[i64]) -> String {
    const BLOCKS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let max = values.iter().copied().max().unwrap_or(0);
    if max == 0 {
        return "▁".repeat(values.len().max(1));
    }
    values
        .iter()
        .map(|v| {
            let index = ((*v as f64 / max as f64) * (BLOCKS.len() - 1) as f64).round() as usize;
            BLOCKS[index.min(BLOCKS.len() - 1)]
        })
        .collect()
}

// ===========================================================================
// Commands
// ===========================================================================

/// Server analytics.
#[poise::command(
    slash_command,
    guild_only,
    category = "Analytics",
    subcommands(
        "stats_server",
        "stats_members",
        "stats_activity",
        "stats_channels",
        "stats_peak",
        "stats_retention",
        "stats_staff",
        "stats_commands",
        "stats_moderation",
        "stats_me"
    )
)]
pub async fn stats(_ctx: Ctx<'_>) -> Result<(), Error> {
    Ok(())
}

/// A single overview of the server.
#[poise::command(slash_command, rename = "server", guild_only)]
pub async fn stats_server(ctx: Ctx<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap();
    let guild_i64 = guild_id.get() as i64;
    let dbh = &ctx.data().db;

    let (member_count, channels, roles, name) = {
        match guild_id.to_guild_cached(&ctx.serenity_context().cache) {
            Some(guild) => (
                guild.member_count as i64,
                guild.channels.len() as i64,
                guild.roles.len() as i64,
                guild.name.clone(),
            ),
            None => (0, 0, 0, "this server".to_string()),
        }
    };

    let dau = active_members(dbh, guild_i64, 1).await;
    let wau = active_members(dbh, guild_i64, 7).await;
    let mau = active_members(dbh, guild_i64, 30).await;

    let messages_7 = sum_since(dbh, guild_i64, "messages", 7).await;
    let messages_30 = sum_since(dbh, guild_i64, "messages", 30).await;
    let voice_7 = sum_since(dbh, guild_i64, "voice_secs", 7).await;
    let joins_7 = sum_since(dbh, guild_i64, "joins", 7).await;
    let leaves_7 = sum_since(dbh, guild_i64, "leaves", 7).await;

    let daily: Vec<i64> = sqlx::query_scalar(
        "SELECT messages FROM stats_daily WHERE guild_id = ? AND day >= ? ORDER BY day ASC",
    )
    .bind(guild_i64)
    .bind(time::day_offset(14))
    .fetch_all(dbh)
    .await
    .unwrap_or_default();

    let total_messages: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(messages), 0) FROM stats_daily WHERE guild_id = ?",
    )
    .bind(guild_i64)
    .fetch_one(dbh)
    .await?;

    ctx.send(
        poise::CreateReply::default().embed(
            embed::titled(format!("📊 {name}"), embed::INFO)
                .field("Members", thousands(member_count), true)
                .field("Channels", channels.to_string(), true)
                .field("Roles", roles.to_string(), true)
                .field("Active today", thousands(dau), true)
                .field("Active this week", thousands(wau), true)
                .field("Active this month", thousands(mau), true)
                .field("Messages (7d)", thousands(messages_7), true)
                .field("Messages (30d)", thousands(messages_30), true)
                .field("Messages (all tracked)", thousands(total_messages), true)
                .field("Voice time (7d)", time::humanize(voice_7), true)
                .field(
                    "Net growth (7d)",
                    format!("{joins_7} joined · {leaves_7} left · **{:+}**", joins_7 - leaves_7),
                    true,
                )
                .field(
                    "Messages, last 14 days",
                    format!("`{}`", sparkline(&daily)),
                    false,
                )
                .footer(embed::footer(
                    "Counters start when the bot joins, so earlier history is not included.",
                )),
        ),
    )
    .await?;
    Ok(())
}

/// Member growth over time.
#[poise::command(slash_command, rename = "members", guild_only)]
pub async fn stats_members(
    ctx: Ctx<'_>,
    #[description = "How many days to look back"]
    #[min = 1]
    #[max = 365]
    days: Option<i64>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap();
    let guild_i64 = guild_id.get() as i64;
    let days = days.unwrap_or(30);
    let dbh = &ctx.data().db;

    let rows: Vec<(String, i64, i64, i64)> = sqlx::query_as(
        "SELECT day, joins, leaves, member_count FROM stats_daily
         WHERE guild_id = ? AND day >= ? ORDER BY day ASC",
    )
    .bind(guild_i64)
    .bind(time::day_offset(days))
    .fetch_all(dbh)
    .await?;

    let joins: i64 = rows.iter().map(|r| r.1).sum();
    let leaves: i64 = rows.iter().map(|r| r.2).sum();
    let counts: Vec<i64> = rows.iter().map(|r| r.3).filter(|c| *c > 0).collect();

    let current = guild_id
        .to_guild_cached(&ctx.serenity_context().cache)
        .map(|g| g.member_count as i64)
        .unwrap_or(0);

    let best_day = rows.iter().max_by_key(|r| r.1);
    let worst_day = rows.iter().max_by_key(|r| r.2);

    let rejoin_rate: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM members WHERE guild_id = ? AND join_count > 1",
    )
    .bind(guild_i64)
    .fetch_one(dbh)
    .await?;

    ctx.send(
        poise::CreateReply::default().embed(
            embed::titled(format!("📈 Member growth · last {days} days"), embed::INFO)
                .field("Current members", thousands(current), true)
                .field("Joined", thousands(joins), true)
                .field("Left", thousands(leaves), true)
                .field("Net change", format!("{:+}", joins - leaves), true)
                .field(
                    "Retention of joiners",
                    if joins > 0 {
                        format!(
                            "{}%",
                            (((joins - leaves).max(0) as f64 / joins as f64) * 100.0).round()
                        )
                    } else {
                        "n/a".to_string()
                    },
                    true,
                )
                .field("Members who rejoined", rejoin_rate.to_string(), true)
                .field(
                    "Best day",
                    best_day
                        .map(|r| format!("{} · +{}", r.0, r.1))
                        .unwrap_or_else(|| "-".into()),
                    true,
                )
                .field(
                    "Worst day",
                    worst_day
                        .map(|r| format!("{} · -{}", r.0, r.2))
                        .unwrap_or_else(|| "-".into()),
                    true,
                )
                .field(
                    "Member count trend",
                    format!("`{}`", sparkline(&counts)),
                    false,
                ),
        ),
    )
    .await?;
    Ok(())
}

/// Daily, weekly and monthly active members, with message and voice volume.
#[poise::command(slash_command, rename = "activity", guild_only)]
pub async fn stats_activity(ctx: Ctx<'_>) -> Result<(), Error> {
    let guild_i64 = ctx.guild_id().unwrap().get() as i64;
    let dbh = &ctx.data().db;

    let dau = active_members(dbh, guild_i64, 1).await;
    let wau = active_members(dbh, guild_i64, 7).await;
    let mau = active_members(dbh, guild_i64, 30).await;

    let top_members: Vec<(i64, i64)> = sqlx::query_as(
        "SELECT user_id, SUM(messages) FROM member_activity
         WHERE guild_id = ? AND day >= ? GROUP BY user_id ORDER BY SUM(messages) DESC LIMIT 10",
    )
    .bind(guild_i64)
    .bind(time::day_offset(30))
    .fetch_all(dbh)
    .await?;

    let top_voice: Vec<(i64, i64)> = sqlx::query_as(
        "SELECT user_id, SUM(voice_secs) FROM member_activity
         WHERE guild_id = ? AND day >= ? GROUP BY user_id
         HAVING SUM(voice_secs) > 0 ORDER BY SUM(voice_secs) DESC LIMIT 5",
    )
    .bind(guild_i64)
    .bind(time::day_offset(30))
    .fetch_all(dbh)
    .await?;

    let chatters = if top_members.is_empty() {
        "-".to_string()
    } else {
        top_members
            .iter()
            .enumerate()
            .map(|(i, (user_id, count))| {
                format!("**{}.** <@{user_id}> — {}", i + 1, thousands(*count))
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    let talkers = if top_voice.is_empty() {
        "-".to_string()
    } else {
        top_voice
            .iter()
            .map(|(user_id, secs)| format!("<@{user_id}> — {}", time::humanize(*secs)))
            .collect::<Vec<_>>()
            .join("\n")
    };

    // Stickiness is a standard engagement measure: DAU as a share of MAU.
    let stickiness = if mau > 0 {
        format!("{}%", ((dau as f64 / mau as f64) * 100.0).round())
    } else {
        "n/a".to_string()
    };

    ctx.send(
        poise::CreateReply::default().embed(
            embed::titled("👥 Member activity", embed::INFO)
                .field("Daily active", thousands(dau), true)
                .field("Weekly active", thousands(wau), true)
                .field("Monthly active", thousands(mau), true)
                .field("Stickiness (DAU/MAU)", stickiness, true)
                .field("Top chatters (30d)", chatters, false)
                .field("Top in voice (30d)", talkers, false),
        ),
    )
    .await?;
    Ok(())
}

/// Which channels carry the conversation.
#[poise::command(slash_command, rename = "channels", guild_only)]
pub async fn stats_channels(
    ctx: Ctx<'_>,
    #[description = "Days to look back"]
    #[min = 1]
    #[max = 365]
    days: Option<i64>,
) -> Result<(), Error> {
    let guild_i64 = ctx.guild_id().unwrap().get() as i64;
    let days = days.unwrap_or(30);

    let rows: Vec<(i64, i64)> = sqlx::query_as(
        "SELECT channel_id, SUM(messages) FROM channel_activity
         WHERE guild_id = ? AND day >= ? GROUP BY channel_id ORDER BY SUM(messages) DESC LIMIT 15",
    )
    .bind(guild_i64)
    .bind(time::day_offset(days))
    .fetch_all(&ctx.data().db)
    .await?;

    let total: i64 = rows.iter().map(|r| r.1).sum();

    let body = if rows.is_empty() {
        "No channel activity recorded yet.".to_string()
    } else {
        rows.iter()
            .map(|(channel_id, count)| {
                let share = if total > 0 {
                    (*count as f64 / total as f64 * 100.0).round() as i64
                } else {
                    0
                };
                format!(
                    "<#{channel_id}> — **{}** ({share}%)\n`{}`",
                    thousands(*count),
                    text::progress_bar(*count, rows[0].1.max(1), 14)
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    ctx.send(
        poise::CreateReply::default().embed(
            embed::titled(format!("💬 Channel activity · last {days} days"), embed::INFO)
                .description(body),
        ),
    )
    .await?;
    Ok(())
}

/// When the server is busiest.
#[poise::command(slash_command, rename = "peak", guild_only)]
pub async fn stats_peak(ctx: Ctx<'_>) -> Result<(), Error> {
    let guild_i64 = ctx.guild_id().unwrap().get() as i64;
    let rows: Vec<(i64, i64, i64)> = sqlx::query_as(
        "SELECT hour, messages, voice FROM hourly_activity WHERE guild_id = ? ORDER BY hour ASC",
    )
    .bind(guild_i64)
    .fetch_all(&ctx.data().db)
    .await?;

    if rows.is_empty() {
        ctx.send(
            poise::CreateReply::default()
                .embed(embed::warn("No hourly data yet. Give it a day or two.")),
        )
        .await?;
        return Ok(());
    }

    let mut hours = vec![0i64; 24];
    for (hour, messages, _) in &rows {
        if let Some(slot) = hours.get_mut(*hour as usize) {
            *slot = *messages;
        }
    }

    let peak = hours
        .iter()
        .enumerate()
        .max_by_key(|(_, count)| **count)
        .map(|(hour, count)| (hour, *count))
        .unwrap_or((0, 0));
    let quietest = hours
        .iter()
        .enumerate()
        .min_by_key(|(_, count)| **count)
        .map(|(hour, count)| (hour, *count))
        .unwrap_or((0, 0));

    let chart = hours
        .chunks(6)
        .enumerate()
        .map(|(block, chunk)| {
            format!(
                "`{:02}:00` {}",
                block * 6,
                sparkline(chunk)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    ctx.send(
        poise::CreateReply::default().embed(
            embed::titled("🕒 Peak activity hours", embed::INFO)
                .description(format!("All times are UTC.\n\n{chart}"))
                .field(
                    "Busiest hour",
                    format!("{:02}:00 UTC — {} messages", peak.0, thousands(peak.1)),
                    true,
                )
                .field(
                    "Quietest hour",
                    format!("{:02}:00 UTC — {} messages", quietest.0, thousands(quietest.1)),
                    true,
                ),
        ),
    )
    .await?;
    Ok(())
}

/// How many joiners stick around.
#[poise::command(slash_command, rename = "retention", guild_only)]
pub async fn stats_retention(ctx: Ctx<'_>) -> Result<(), Error> {
    let guild_i64 = ctx.guild_id().unwrap().get() as i64;
    let dbh = &ctx.data().db;

    let mut lines = Vec::new();
    for window in [1i64, 7, 30, 90] {
        let joined: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM members WHERE guild_id = ? AND first_join >= ?",
        )
        .bind(guild_i64)
        .bind(time::day_offset(window))
        .fetch_one(dbh)
        .await?;

        let still_here: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM members
             WHERE guild_id = ? AND first_join >= ?
               AND (last_leave IS NULL OR last_leave < last_join)",
        )
        .bind(guild_i64)
        .bind(time::day_offset(window))
        .fetch_one(dbh)
        .await?;

        let percent = if joined > 0 {
            format!(
                "{}%",
                ((still_here as f64 / joined as f64) * 100.0).round()
            )
        } else {
            "n/a".to_string()
        };
        lines.push(format!(
            "**Last {window} day(s):** {still_here} of {joined} still here · {percent}"
        ));
    }

    // How many members have been seen talking in the last 30 days.
    let engaged: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM members WHERE guild_id = ? AND last_seen >= ?",
    )
    .bind(guild_i64)
    .bind(time::day_offset(30))
    .fetch_one(dbh)
    .await?;
    let known: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM members WHERE guild_id = ?")
        .bind(guild_i64)
        .fetch_one(dbh)
        .await?;

    ctx.send(
        poise::CreateReply::default().embed(
            embed::titled("🔁 Member retention", embed::INFO)
                .description(lines.join("\n"))
                .field(
                    "Spoke in the last 30 days",
                    format!(
                        "{engaged} of {known} tracked members ({}%)",
                        if known > 0 {
                            ((engaged as f64 / known as f64) * 100.0).round()
                        } else {
                            0.0
                        }
                    ),
                    false,
                )
                .footer(embed::footer(
                    "Only members the bot has seen join or speak are counted.",
                )),
        ),
    )
    .await?;
    Ok(())
}

/// Staff workload: moderation actions and tickets.
#[poise::command(slash_command, rename = "staff", guild_only, check = "perms::staff_only")]
pub async fn stats_staff(
    ctx: Ctx<'_>,
    #[description = "Days to look back"]
    #[min = 1]
    #[max = 365]
    days: Option<i64>,
) -> Result<(), Error> {
    let guild_i64 = ctx.guild_id().unwrap().get() as i64;
    let days = days.unwrap_or(30);
    let since = time::day_offset(days);
    let dbh = &ctx.data().db;

    let mod_actions: Vec<(i64, i64)> = sqlx::query_as(
        "SELECT mod_id, COUNT(*) FROM mod_cases
         WHERE guild_id = ? AND created_at >= ? AND automated = 0
         GROUP BY mod_id ORDER BY COUNT(*) DESC LIMIT 10",
    )
    .bind(guild_i64)
    .bind(&since)
    .fetch_all(dbh)
    .await?;

    let tickets: Vec<(i64, i64, Option<f64>)> = sqlx::query_as(
        "SELECT claimed_by, COUNT(*), AVG(rating) FROM tickets
         WHERE guild_id = ? AND created_at >= ? AND claimed_by IS NOT NULL
         GROUP BY claimed_by ORDER BY COUNT(*) DESC LIMIT 10",
    )
    .bind(guild_i64)
    .bind(&since)
    .fetch_all(dbh)
    .await?;

    let automated: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM mod_cases WHERE guild_id = ? AND created_at >= ? AND automated = 1",
    )
    .bind(guild_i64)
    .bind(&since)
    .fetch_one(dbh)
    .await?;

    let mod_body = if mod_actions.is_empty() {
        "No manual moderation in this window.".to_string()
    } else {
        mod_actions
            .iter()
            .map(|(mod_id, count)| format!("<@{mod_id}> — **{count}** action(s)"))
            .collect::<Vec<_>>()
            .join("\n")
    };

    let ticket_body = if tickets.is_empty() {
        "No tickets handled in this window.".to_string()
    } else {
        tickets
            .iter()
            .map(|(staff_id, count, rating)| {
                format!(
                    "<@{staff_id}> — **{count}** ticket(s) · rating {}",
                    rating
                        .map(|r| format!("{r:.1}/5"))
                        .unwrap_or_else(|| "n/a".into())
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    ctx.send(
        poise::CreateReply::default().embed(
            embed::titled(format!("🛠️ Staff performance · last {days} days"), embed::INFO)
                .field("Moderation", mod_body, false)
                .field("Tickets", ticket_body, false)
                .field("Automod actions", automated.to_string(), true),
        ),
    )
    .await?;
    Ok(())
}

/// Which commands get used.
#[poise::command(slash_command, rename = "commands", guild_only)]
pub async fn stats_commands(ctx: Ctx<'_>) -> Result<(), Error> {
    let guild_i64 = ctx.guild_id().unwrap().get() as i64;
    let rows: Vec<(String, i64, i64)> = sqlx::query_as(
        "SELECT command, uses, errors FROM command_usage
         WHERE guild_id = ? ORDER BY uses DESC LIMIT 20",
    )
    .bind(guild_i64)
    .fetch_all(&ctx.data().db)
    .await?;

    let total: i64 = rows.iter().map(|r| r.1).sum();
    let body = if rows.is_empty() {
        "No commands recorded yet.".to_string()
    } else {
        rows.iter()
            .map(|(command, uses, errors)| {
                if *errors > 0 {
                    format!("`/{command}` — **{uses}** ({errors} failed)")
                } else {
                    format!("`/{command}` — **{uses}**")
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    ctx.send(
        poise::CreateReply::default().embed(
            embed::titled("⌨️ Command usage", embed::INFO)
                .description(body)
                .footer(embed::footer(format!("{} total invocations", thousands(total)))),
        ),
    )
    .await?;
    Ok(())
}

/// Moderation volume over time.
#[poise::command(slash_command, rename = "moderation", guild_only, check = "perms::staff_only")]
pub async fn stats_moderation(ctx: Ctx<'_>) -> Result<(), Error> {
    let guild_i64 = ctx.guild_id().unwrap().get() as i64;
    let dbh = &ctx.data().db;

    let by_action: Vec<(String, i64)> = sqlx::query_as(
        "SELECT action, COUNT(*) FROM mod_cases WHERE guild_id = ?
         GROUP BY action ORDER BY COUNT(*) DESC",
    )
    .bind(guild_i64)
    .fetch_all(dbh)
    .await?;

    let last_7 = sum_since(dbh, guild_i64, "mod_actions", 7).await;
    let last_30 = sum_since(dbh, guild_i64, "mod_actions", 30).await;

    let repeat_offenders: Vec<(i64, i64)> = sqlx::query_as(
        "SELECT target_id, COUNT(*) FROM mod_cases WHERE guild_id = ?
         GROUP BY target_id HAVING COUNT(*) > 1 ORDER BY COUNT(*) DESC LIMIT 10",
    )
    .bind(guild_i64)
    .fetch_all(dbh)
    .await?;

    let appeals: (i64, i64, i64) = sqlx::query_as(
        "SELECT
            SUM(CASE WHEN status = 'pending' THEN 1 ELSE 0 END),
            SUM(CASE WHEN status = 'accepted' THEN 1 ELSE 0 END),
            SUM(CASE WHEN status = 'denied' THEN 1 ELSE 0 END)
         FROM appeals WHERE guild_id = ?",
    )
    .bind(guild_i64)
    .fetch_optional(dbh)
    .await?
    .unwrap_or((0, 0, 0));

    let actions = if by_action.is_empty() {
        "Nothing recorded.".to_string()
    } else {
        by_action
            .iter()
            .map(|(action, count)| format!("**{action}** · {count}"))
            .collect::<Vec<_>>()
            .join("\n")
    };

    let offenders = if repeat_offenders.is_empty() {
        "-".to_string()
    } else {
        repeat_offenders
            .iter()
            .map(|(user_id, count)| format!("<@{user_id}> — {count} cases"))
            .collect::<Vec<_>>()
            .join("\n")
    };

    ctx.send(
        poise::CreateReply::default().embed(
            embed::titled("⚖️ Moderation statistics", embed::INFO)
                .field("Actions (7d)", last_7.to_string(), true)
                .field("Actions (30d)", last_30.to_string(), true)
                .field("By type", actions, false)
                .field("Repeat offenders", offenders, false)
                .field(
                    "Appeals",
                    format!(
                        "{} pending · {} accepted · {} denied",
                        appeals.0, appeals.1, appeals.2
                    ),
                    false,
                ),
        ),
    )
    .await?;
    Ok(())
}

/// Your own numbers in this server.
#[poise::command(slash_command, rename = "me", guild_only)]
pub async fn stats_me(
    ctx: Ctx<'_>,
    #[description = "Someone else's numbers"] user: Option<User>,
) -> Result<(), Error> {
    let guild_i64 = ctx.guild_id().unwrap().get() as i64;
    let target = user.unwrap_or_else(|| ctx.author().clone());
    let user_i64 = target.id.get() as i64;
    let dbh = &ctx.data().db;

    let (messages_7, voice_7): (i64, i64) = sqlx::query_as(
        "SELECT COALESCE(SUM(messages), 0), COALESCE(SUM(voice_secs), 0)
         FROM member_activity WHERE guild_id = ? AND user_id = ? AND day >= ?",
    )
    .bind(guild_i64)
    .bind(user_i64)
    .bind(time::day_offset(7))
    .fetch_optional(dbh)
    .await?
    .unwrap_or((0, 0));

    let (messages_30, voice_30): (i64, i64) = sqlx::query_as(
        "SELECT COALESCE(SUM(messages), 0), COALESCE(SUM(voice_secs), 0)
         FROM member_activity WHERE guild_id = ? AND user_id = ? AND day >= ?",
    )
    .bind(guild_i64)
    .bind(user_i64)
    .bind(time::day_offset(30))
    .fetch_optional(dbh)
    .await?
    .unwrap_or((0, 0));

    let daily: Vec<i64> = sqlx::query_scalar(
        "SELECT messages FROM member_activity
         WHERE guild_id = ? AND user_id = ? AND day >= ? ORDER BY day ASC",
    )
    .bind(guild_i64)
    .bind(user_i64)
    .bind(time::day_offset(14))
    .fetch_all(dbh)
    .await?;

    let favourite: Option<(i64, i64)> = sqlx::query_as(
        "SELECT channel_id, SUM(messages) FROM channel_activity
         WHERE guild_id = ? AND day >= ? GROUP BY channel_id ORDER BY SUM(messages) DESC LIMIT 1",
    )
    .bind(guild_i64)
    .bind(time::day_offset(30))
    .fetch_optional(dbh)
    .await?;

    let cases: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM mod_cases WHERE guild_id = ? AND target_id = ?")
            .bind(guild_i64)
            .bind(user_i64)
            .fetch_one(dbh)
            .await?;

    ctx.send(
        poise::CreateReply::default().embed(
            embed::titled(format!("📊 {}", target.tag()), embed::INFO)
                .field("Messages (7d)", thousands(messages_7), true)
                .field("Messages (30d)", thousands(messages_30), true)
                .field("Voice (7d)", time::humanize(voice_7), true)
                .field("Voice (30d)", time::humanize(voice_30), true)
                .field("Moderation cases", cases.to_string(), true)
                .field(
                    "Busiest channel here",
                    favourite
                        .map(|(channel_id, _)| format!("<#{channel_id}>"))
                        .unwrap_or_else(|| "-".into()),
                    true,
                )
                .field(
                    "Your last 14 days",
                    format!("`{}`", sparkline(&daily)),
                    false,
                )
                .thumbnail(target.face()),
        ),
    )
    .await?;
    Ok(())
}

/// Live member count, optionally kept in a channel name.
#[poise::command(slash_command, prefix_command, guild_only, category = "Analytics")]
pub async fn membercount(ctx: Ctx<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap();

    // Copied out of the cache in one block so no guard is held across an await.
    let (total, bots, cached) = match guild_id.to_guild_cached(&ctx.serenity_context().cache) {
        Some(guild) => (
            guild.member_count as i64,
            guild.members.values().filter(|m| m.user.bot).count() as i64,
            guild.members.len() as i64,
        ),
        None => (0, 0, 0),
    };
    let humans = (total - bots).max(0);

    ctx.send(
        poise::CreateReply::default().embed(
            embed::titled("👥 Member count", embed::INFO)
                .field("Total", thousands(total), true)
                .field("People", thousands(humans), true)
                .field("Bots", thousands(bots), true)
                .footer(embed::footer(format!(
                    "{} members are in my cache",
                    thousands(cached)
                ))),
        ),
    )
    .await?;
    Ok(())
}

/// Which channel should show the live member count.
#[poise::command(
    slash_command,
    guild_only,
    category = "Analytics",
    check = "perms::admin_only"
)]
pub async fn countchannel(
    ctx: Ctx<'_>,
    #[description = "Voice or text channel to rename, or leave empty to switch off"]
    channel: Option<GuildChannel>,
    #[description = "Name template, {count} is replaced"] template: Option<String>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let dbh = &ctx.data().db;

    match channel {
        Some(channel) => {
            crate::db::kv_set(
                dbh,
                guild_id,
                "count_channel",
                &channel.id.get().to_string(),
            )
            .await?;
            crate::db::kv_set(
                dbh,
                guild_id,
                "count_template",
                template.as_deref().unwrap_or("Members: {count}"),
            )
            .await?;
            ctx.send(poise::CreateReply::default().embed(embed::ok(format!(
                "<#{}> will be renamed with the member count every 10 minutes.",
                channel.id.get()
            ))))
            .await?;
        }
        None => {
            crate::db::kv_set(dbh, guild_id, "count_channel", "").await?;
            ctx.send(poise::CreateReply::default().embed(embed::ok("Member count channel off.")))
                .await?;
        }
    }
    Ok(())
}

pub fn commands() -> Vec<poise::Command<Data, Error>> {
    vec![stats(), membercount(), countchannel()]
}
