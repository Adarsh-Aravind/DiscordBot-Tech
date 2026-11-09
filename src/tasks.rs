//! Background jobs. Each loop is independent so one slow job cannot stall the
//! others, and every iteration catches its own errors.

use std::time::Duration;

use chrono::Utc;
use poise::serenity_prelude as serenity;
use serenity::{ChannelId, CreateMessage, GuildId, RoleId, UserId};

use crate::db::Db;
use crate::modules::{automation, community, leveling, moderation, tickets, voice};
use crate::state::Data;
use crate::util::{embed, time};

/// Start every background loop. Called once the bot is ready.
pub fn spawn_all(sctx: serenity::Context, data: Data) {
    spawn_loop("fast", 30, sctx.clone(), data.clone(), |sctx, data| async move {
        moderation::sweep_expired(&sctx, &data.db).await;
        automation::run_scheduler(&sctx, &data).await;
    });

    spawn_loop("minute", 60, sctx.clone(), data.clone(), |sctx, data| async move {
        community::polls::sweep(&sctx, &data).await;
        community::giveaways::sweep(&sctx, &data).await;
        community::events::sweep(&sctx, &data).await;
    });

    spawn_loop("five-minute", 300, sctx.clone(), data.clone(), |sctx, data| async move {
        voice::tick_voice_xp(&sctx, &data).await;
        voice::sweep_afk(&sctx, &data).await;
        tickets::auto_close_stale(&sctx, &data).await;
    });

    spawn_loop("hourly", 3600, sctx.clone(), data.clone(), |sctx, data| async move {
        automation::run_weekly_reports(&sctx, &data).await;
        automation::update_count_channels(&sctx, &data, &data.db).await;
        community::misc::sweep_challenges(&sctx, &data).await;
        prune_old_data(&data.db).await;
        snapshot_guilds(&sctx, &data).await;
    });

    // The daily loop wakes every 15 minutes and acts once per UTC day.
    spawn_loop("daily", 900, sctx, data, |sctx, data| async move {
        run_daily(&sctx, &data).await;
    });
}

fn spawn_loop<F, Fut>(
    name: &'static str,
    seconds: u64,
    sctx: serenity::Context,
    data: Data,
    job: F,
) where
    F: Fn(serenity::Context, Data) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    tokio::spawn(async move {
        // A short stagger so the loops do not all fire on the same tick.
        tokio::time::sleep(Duration::from_secs(5)).await;
        let mut ticker = tokio::time::interval(Duration::from_secs(seconds));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            ticker.tick().await;
            let started = std::time::Instant::now();
            job(sctx.clone(), data.clone()).await;
            let elapsed = started.elapsed();
            if elapsed > Duration::from_secs(seconds) {
                tracing::warn!(
                    task = name,
                    secs = elapsed.as_secs(),
                    "background job took longer than its interval"
                );
            } else {
                tracing::trace!(task = name, ms = elapsed.as_millis(), "background job done");
            }
        }
    });
}

/// Drop data we no longer need, so the database does not grow without bound.
async fn prune_old_data(db: &Db) {
    // Message log is only needed for edit/delete logs and snipes.
    let cutoff = time::day_offset(14);
    let _ = sqlx::query("DELETE FROM message_log WHERE created_at < ?")
        .bind(&cutoff)
        .execute(db)
        .await;

    // Per-day member activity beyond a year is not used by any command.
    let _ = sqlx::query("DELETE FROM member_activity WHERE day < ?")
        .bind(time::day_offset(400))
        .execute(db)
        .await;
    let _ = sqlx::query("DELETE FROM channel_activity WHERE day < ?")
        .bind(time::day_offset(400))
        .execute(db)
        .await;

    // Closed voice sessions older than 90 days: the totals live on the member row.
    let _ = sqlx::query("DELETE FROM voice_sessions WHERE left_at IS NOT NULL AND left_at < ?")
        .bind(time::day_offset(90))
        .execute(db)
        .await;

    // Expired trivia answer keys.
    let _ = sqlx::query("DELETE FROM kv_settings WHERE key LIKE 'trivia.%'")
        .execute(db)
        .await;

    // Finished polls and giveaways older than 30 days.
    let _ = sqlx::query("DELETE FROM polls WHERE closed = 1 AND created_at < ?")
        .bind(time::day_offset(30))
        .execute(db)
        .await;
}

/// Record today's member count for every guild we can see.
async fn snapshot_guilds(sctx: &serenity::Context, data: &Data) {
    let guilds: Vec<GuildId> = sctx.cache.guilds();
    for guild_id in guilds {
        let count = guild_id
            .to_guild_cached(&sctx.cache)
            .map(|g| g.member_count as i64);
        if let Some(count) = count {
            crate::modules::analytics::snapshot_member_count(
                &data.db,
                guild_id.get() as i64,
                count,
            )
            .await;
        }
    }
}

/// Jobs that should happen once a day: birthdays, the daily question,
/// anniversaries and the achievement sweep.
async fn run_daily(sctx: &serenity::Context, data: &Data) {
    let today = time::today();
    let marker = crate::db::kv_get(&data.db, 0, "last_daily_run").await;
    if marker.as_deref() == Some(today.as_str()) {
        return;
    }

    // Only run after 09:00 UTC so announcements land at a sensible hour.
    use chrono::Timelike;
    if Utc::now().hour() < 9 {
        return;
    }

    let _ = crate::db::kv_set(&data.db, 0, "last_daily_run", &today).await;
    tracing::info!("running daily jobs for {today}");

    let guilds: Vec<GuildId> = sctx.cache.guilds();
    for guild_id in guilds {
        let guild_i64 = guild_id.get() as i64;

        announce_birthdays(sctx, data, guild_id).await;
        community::games::post_daily_question(sctx, data, guild_i64, None).await;
        community::misc::check_anniversary(sctx, data, guild_id).await;
        refresh_achievements(data, guild_i64).await;
    }
}

/// Announce today's birthdays and hand out the birthday role.
async fn announce_birthdays(sctx: &serenity::Context, data: &Data, guild_id: GuildId) {
    let guild_i64 = guild_id.get() as i64;

    let Ok(cfg) = crate::db::guild_config(&data.db, guild_i64).await else {
        return;
    };

    let today = Utc::now();
    use chrono::Datelike;
    let key = format!("{:02}-{:02}", today.month(), today.day());

    let birthday_people: Vec<i64> = sqlx::query_scalar(
        "SELECT user_id FROM profiles WHERE guild_id = ? AND birthday = ?",
    )
    .bind(guild_i64)
    .bind(&key)
    .fetch_all(&data.db)
    .await
    .unwrap_or_default();

    // Take yesterday's birthday role off everyone first.
    if let Some(role_i64) = cfg.birthday_role {
        let role = RoleId::new(role_i64 as u64);
        let holders: Vec<UserId> = match guild_id.to_guild_cached(&sctx.cache) {
            Some(guild) => guild
                .members
                .values()
                .filter(|m| m.roles.contains(&role))
                .map(|m| m.user.id)
                .collect(),
            None => Vec::new(),
        };
        for user_id in holders {
            if !birthday_people.contains(&(user_id.get() as i64)) {
                let _ = crate::compat::remove_role(&sctx.http, guild_id, user_id, role).await;
            }
        }
    }

    if birthday_people.is_empty() {
        return;
    }

    // Give the role to today's birthday members.
    if let Some(role_i64) = cfg.birthday_role {
        for user_i64 in &birthday_people {
            let _ = crate::compat::add_role(
                &sctx.http,
                guild_id,
                UserId::new(*user_i64 as u64),
                RoleId::new(role_i64 as u64),
            )
            .await;
        }
    }

    let Some(channel_i64) = cfg.birthday_channel else {
        return;
    };

    let mentions = birthday_people
        .iter()
        .map(|id| format!("<@{id}>"))
        .collect::<Vec<_>>()
        .join(" ");

    let body = if birthday_people.len() == 1 {
        format!("🎉 Happy birthday {mentions}! Have a great one.")
    } else {
        format!("🎉 Happy birthday to {mentions}! Have a great one, all of you.")
    };

    let _ = ChannelId::new(channel_i64 as u64)
        .send_message(
            &sctx.http,
            CreateMessage::new().content(mentions).embed(
                embed::titled("🎂 Birthdays today", embed::BRAND).description(body),
            ),
        )
        .await;
}

/// Re-check achievements for members who were active yesterday, so streak and
/// voice badges land without waiting for a level-up.
async fn refresh_achievements(data: &Data, guild_id: i64) {
    let recent: Vec<i64> = sqlx::query_scalar(
        "SELECT DISTINCT user_id FROM member_activity WHERE guild_id = ? AND day >= ?",
    )
    .bind(guild_id)
    .bind(time::day_offset(2))
    .fetch_all(&data.db)
    .await
    .unwrap_or_default();

    for user_id in recent {
        let _ = leveling::check_achievements(&data.db, guild_id, user_id).await;
    }
}

/// Load a phishing-domain feed, if one is configured.
pub async fn refresh_phish_list(data: &Data) {
    let Some(url) = data.config.phish_list_url.clone() else {
        return;
    };

    match data.http.get(&url).send().await {
        Ok(response) if response.status().is_success() => match response.text().await {
            Ok(body) => {
                let domains: std::collections::HashSet<String> = body
                    .lines()
                    .map(|line| line.trim().to_lowercase())
                    .filter(|line| !line.is_empty() && !line.starts_with('#') && line.contains('.'))
                    .collect();
                let count = domains.len();
                if count > 0 {
                    let mut list = data.phish_domains.write();
                    list.extend(domains);
                    tracing::info!(count, total = list.len(), "loaded phishing domains");
                }
            }
            Err(why) => tracing::warn!("could not read the phishing feed: {why}"),
        },
        Ok(response) => {
            tracing::warn!(status = %response.status(), "phishing feed returned an error")
        }
        Err(why) => tracing::warn!("phishing feed unreachable: {why}"),
    }
}
