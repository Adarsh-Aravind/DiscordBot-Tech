//! Background jobs. Each loop is independent so one slow job cannot stall the
//! others, and every iteration catches its own errors.

use std::time::Duration;

use chrono::Utc;
use poise::serenity_prelude as serenity;
use serenity::GuildId;

use crate::db::Db;
use crate::modules::{automation, community, leveling, moderation, voice};
use crate::state::Data;
use crate::util::time;

/// Start every background loop. Called once the bot is ready.
pub fn spawn_all(sctx: serenity::Context, data: Data) {
    spawn_loop("fast", 30, sctx.clone(), data.clone(), |sctx, data| async move {
        moderation::sweep_expired(&sctx, &data.db).await;
        automation::run_reminders(&sctx, &data).await;
        voice::sweep_pending_deletes(&sctx, &data).await;
    });

    spawn_loop("minute", 60, sctx.clone(), data.clone(), |sctx, data| async move {
        community::polls::sweep(&sctx, &data).await;
        community::giveaways::sweep(&sctx, &data).await;
        community::events::sweep(&sctx, &data).await;
    });

    spawn_loop("five-minute", 300, sctx.clone(), data.clone(), |sctx, data| async move {
        voice::tick_voice_xp(&sctx, &data).await;
        voice::sweep_afk(&sctx, &data).await;
    });

    spawn_loop("hourly", 3600, sctx.clone(), data.clone(), |sctx, data| async move {
        community::misc::sweep_challenges(&sctx, &data).await;
        prune_old_data(&data.db).await;
        prune_memory(&data);
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

    // Closed voice sessions older than 90 days: the totals live on the member row.
    let _ = sqlx::query("DELETE FROM voice_sessions WHERE left_at IS NOT NULL AND left_at < ?")
        .bind(time::day_offset(90))
        .execute(db)
        .await;

    // Short-lived settings rows - in-flight trivia answers and the record of
    // who has answered - once they are actually past their expiry. This used to
    // be `DELETE ... WHERE key LIKE 'trivia.%'`, which ran on the hour and took
    // every open question with it.
    let _ = sqlx::query("DELETE FROM kv_settings WHERE expires_at IS NOT NULL AND expires_at <= ?")
        .bind(time::sql_now())
        .execute(db)
        .await;

    // Finished polls and giveaways older than 30 days.
    let _ = sqlx::query("DELETE FROM polls WHERE closed = 1 AND created_at < ?")
        .bind(time::day_offset(30))
        .execute(db)
        .await;

}

/// Forget in-memory tracking for members and channels that have gone quiet.
///
/// The automod windows, the duplicate tracker and the snipe slots were only
/// ever removed when a threshold tripped, so a long-running bot kept one entry
/// per member it had ever seen. It also means `/snipe` stops being able to
/// resurrect a message somebody deleted days ago.
fn prune_memory(data: &Data) {
    const QUIET: u64 = 900;
    const NUKE_QUIET: u64 = 3600;
    const SNIPE_HOURS: i64 = 24;
    const DUPLICATE_QUIET: Duration = Duration::from_secs(900);

    let before = data.spam.len() + data.duplicates.len() + data.snipes.len();

    data.spam.retain(|_, window| window.count(QUIET) > 0);
    data.flood.retain(|_, window| window.count(QUIET) > 0);
    data.joins.retain(|_, window| window.count(QUIET) > 0);
    data.nuke.retain(|_, window| window.count(NUKE_QUIET) > 0);
    data.duplicates
        .retain(|_, last| last.at.elapsed() < DUPLICATE_QUIET);
    data.snipes
        .retain(|_, snipe| (Utc::now() - snipe.at).num_hours() < SNIPE_HOURS);
    data.edit_snipes
        .retain(|_, snipe| (Utc::now() - snipe.at).num_hours() < SNIPE_HOURS);
    data.raid_mode.retain(|_, until| *until > Utc::now());

    let after = data.spam.len() + data.duplicates.len() + data.snipes.len();
    if before != after {
        tracing::debug!(dropped = before - after, "pruned in-memory tracking");
    }
}

/// Jobs that should happen once a day: the daily question,
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

        community::games::post_daily_question(sctx, data, guild_i64, None).await;
        community::misc::check_anniversary(sctx, data, guild_id).await;
        refresh_achievements(data, guild_i64).await;
    }
}

/// Re-check achievements for members who were active yesterday, so streak and
/// voice badges land without waiting for a level-up.
async fn refresh_achievements(data: &Data, guild_id: i64) {
    let recent: Vec<i64> = sqlx::query_scalar(
        "SELECT user_id FROM levels WHERE guild_id = ? AND last_active >= ?",
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
