//! Leveling: the XP engine, level curve, role rewards, streaks and achievements.

pub mod commands;

use std::sync::Arc;

use chrono::Utc;
use once_cell::sync::Lazy;
use poise::serenity_prelude as serenity;
use serenity::{ChannelId, CreateMessage, GuildId, Message, RoleId, UserId};

use crate::compat;
use crate::db::{self, Db, LevelingConfig};
use crate::state::{Data, Error};
use crate::util::cache::TtlCache;
use crate::util::{embed, text};

// ---------------------------------------------------------------------------
// Level curve
// ---------------------------------------------------------------------------

/// XP required to go from `level` to `level + 1`.
pub fn xp_to_next(level: i64) -> i64 {
    let l = level.max(0);
    5 * l * l + 50 * l + 100
}

/// Cumulative XP required to reach `level` from zero.
pub fn total_xp_for_level(level: i64) -> i64 {
    (0..level.max(0)).map(xp_to_next).sum()
}

/// The level a given lifetime XP total corresponds to.
pub fn level_from_total(total_xp: i64) -> i64 {
    let mut level = 0;
    let mut remaining = total_xp;
    while remaining >= xp_to_next(level) {
        remaining -= xp_to_next(level);
        level += 1;
        if level > 1_000 {
            break;
        }
    }
    level
}

// ---------------------------------------------------------------------------
// Awarding XP
// ---------------------------------------------------------------------------

pub struct LevelUp {
    pub level: i64,
    pub rewards: Vec<i64>,
    pub new_achievements: Vec<String>,
}

/// Called for every non-bot guild message.
pub async fn handle_message(
    sctx: &serenity::Context,
    data: &Data,
    message: &Message,
) -> Result<(), Error> {
    let Some(guild_id) = message.guild_id else {
        return Ok(());
    };
    if message.author.bot {
        return Ok(());
    }

    let guild_i64 = guild_id.get() as i64;
    let user_i64 = message.author.id.get() as i64;

    let cfg = db::leveling_config(&data.db, guild_i64).await?;
    if cfg.enabled == 0 {
        return Ok(());
    }

    // Channel and role exclusions.
    if db::csv_ids(&cfg.no_xp_channels).contains(&(message.channel_id.get() as i64)) {
        return Ok(());
    }
    let member_roles: Vec<i64> = message
        .member
        .as_ref()
        .map(|m| m.roles.iter().map(|r| r.get() as i64).collect())
        .unwrap_or_default();
    let blocked = db::csv_ids(&cfg.no_xp_roles);
    if member_roles.iter().any(|r| blocked.contains(r)) {
        return Ok(());
    }

    ensure_row(&data.db, guild_i64, user_i64).await?;

    // One read for everything this function needs to decide, instead of one
    // read for the cooldown and another for the streak.
    let (last_xp, last_active, streak, best_streak): (
        Option<String>,
        Option<String>,
        i64,
        i64,
    ) = sqlx::query_as(
        "SELECT last_xp, last_active, streak, best_streak FROM levels
         WHERE guild_id = ? AND user_id = ?",
    )
    .bind(guild_i64)
    .bind(user_i64)
    .fetch_one(&data.db)
    .await?;

    let on_cooldown = last_xp
        .as_deref()
        .and_then(crate::util::time::from_sql)
        .map(|t| (Utc::now() - t).num_seconds() < cfg.cooldown)
        .unwrap_or(false);

    // The message counter and the daily streak both tick regardless of the XP
    // cooldown, and they go in the same statement so the common case - a
    // message from someone who is still on cooldown - is a single write.
    let streak_bonus = bump_activity(
        &data.db,
        guild_i64,
        user_i64,
        last_active.as_deref(),
        streak,
        best_streak,
    )
    .await?;

    if on_cooldown {
        return Ok(());
    }

    let base = {
        use rand::Rng;
        let mut rng = rand::thread_rng();
        let low = cfg.xp_min.min(cfg.xp_max).max(1);
        let high = cfg.xp_max.max(cfg.xp_min).max(low + 1);
        rng.gen_range(low..=high)
    };

    let multiplier = resolve_multiplier(
        &data.db,
        guild_i64,
        message.channel_id.get() as i64,
        &member_roles,
        cfg.global_rate,
    )
    .await;

    let amount = ((base as f64) * multiplier).round() as i64 + streak_bonus;

    if let Some(levelup) = add_xp(&data.db, guild_i64, user_i64, amount.max(1)).await? {
        apply_levelup(
            sctx,
            data,
            guild_id,
            message.author.id,
            Some(message.channel_id),
            &cfg,
            levelup,
        )
        .await;
    }

    Ok(())
}

pub async fn ensure_row(db: &Db, guild_id: i64, user_id: i64) -> Result<(), Error> {
    sqlx::query("INSERT OR IGNORE INTO levels (guild_id, user_id) VALUES (?, ?)")
        .bind(guild_id)
        .bind(user_id)
        .execute(db)
        .await?;
    Ok(())
}

/// Apply an XP delta to an in-level total, rolling the level over or back.
///
/// `level` is not derivable from the lifetime total - prestige resets the level
/// and deliberately keeps the total - so it is always carried forward from what
/// the row already said.
fn roll_level(mut xp: i64, mut level: i64) -> (i64, i64) {
    while xp >= xp_to_next(level) {
        xp -= xp_to_next(level);
        level += 1;
        if level > 1_000 {
            break;
        }
    }
    // Negative adjustments can push someone back down a level.
    while xp < 0 && level > 0 {
        level -= 1;
        xp += xp_to_next(level);
    }
    if xp < 0 {
        xp = 0;
    }
    (xp, level)
}

/// Take XP off a member, but only if they still have it.
///
/// The balance check and the write are one compare-and-set, so two commands
/// running at the same moment cannot both spend the same XP. Returns false when
/// the balance was not there.
pub async fn try_spend_xp(
    db: &Db,
    guild_id: i64,
    user_id: i64,
    amount: i64,
) -> Result<bool, Error> {
    if amount <= 0 {
        return Ok(false);
    }
    ensure_row(db, guild_id, user_id).await?;

    for _ in 0..5 {
        let (read_xp, read_level, read_total): (i64, i64, i64) = sqlx::query_as(
            "SELECT xp, level, total_xp FROM levels WHERE guild_id = ? AND user_id = ?",
        )
        .bind(guild_id)
        .bind(user_id)
        .fetch_one(db)
        .await?;

        if read_total < amount {
            return Ok(false);
        }

        let (xp, level) = roll_level(read_xp - amount, read_level);

        let affected = sqlx::query(
            "UPDATE levels SET xp = ?, level = ?, total_xp = ?
             WHERE guild_id = ? AND user_id = ? AND xp = ? AND total_xp = ?",
        )
        .bind(xp)
        .bind(level)
        .bind(read_total - amount)
        .bind(guild_id)
        .bind(user_id)
        .bind(read_xp)
        .bind(read_total)
        .execute(db)
        .await?
        .rows_affected();

        if affected == 1 {
            return Ok(true);
        }
        // Somebody else wrote first; look again at what they left.
    }

    Ok(false)
}

/// Cached multiplier rows. They are read for every message and written perhaps
/// twice a year; [`invalidate_multipliers`] drops the entry when they change.
static MULTIPLIER_CACHE: Lazy<TtlCache<i64, Arc<Vec<(String, i64, f64)>>>> =
    Lazy::new(|| TtlCache::new(300, 2048));

/// Forget the cached XP multipliers for a guild.
pub fn invalidate_multipliers(guild_id: i64) {
    MULTIPLIER_CACHE.invalidate(&guild_id);
}

async fn multipliers(db: &Db, guild_id: i64) -> Arc<Vec<(String, i64, f64)>> {
    if let Some(hit) = MULTIPLIER_CACHE.get(&guild_id) {
        return hit;
    }
    let rows: Vec<(String, i64, f64)> =
        sqlx::query_as("SELECT kind, target_id, multiplier FROM xp_multipliers WHERE guild_id = ?")
            .bind(guild_id)
            .fetch_all(db)
            .await
            .unwrap_or_default();
    let rows = Arc::new(rows);
    MULTIPLIER_CACHE.put(guild_id, Arc::clone(&rows));
    rows
}

/// Highest applicable role multiplier times the channel multiplier times the
/// server-wide rate.
async fn resolve_multiplier(
    db: &Db,
    guild_id: i64,
    channel_id: i64,
    roles: &[i64],
    global_rate: f64,
) -> f64 {
    let rows = multipliers(db, guild_id).await;

    let mut role_mult: f64 = 1.0;
    let mut channel_mult: f64 = 1.0;
    for (kind, target, multiplier) in rows.iter() {
        match kind.as_str() {
            "role" if roles.contains(target) => role_mult = role_mult.max(*multiplier),
            "channel" if *target == channel_id => channel_mult = *multiplier,
            _ => {}
        }
    }
    (global_rate.max(0.0) * role_mult * channel_mult).clamp(0.0, 20.0)
}

/// Count the message and maintain the daily activity streak, in one statement.
/// Returns the small XP bonus a long run earns.
async fn bump_activity(
    db: &Db,
    guild_id: i64,
    user_id: i64,
    last_active: Option<&str>,
    streak: i64,
    best_streak: i64,
) -> Result<i64, Error> {
    let today = Utc::now().format("%Y-%m-%d").to_string();
    let yesterday = (Utc::now() - chrono::Duration::days(1))
        .format("%Y-%m-%d")
        .to_string();

    let last = last_active.unwrap_or_default();
    let (new_streak, new_best) = if last == today {
        (streak, best_streak)
    } else if last == yesterday {
        (streak + 1, best_streak.max(streak + 1))
    } else {
        (1, best_streak.max(1))
    };

    sqlx::query(
        "UPDATE levels
            SET messages = messages + 1,
                last_active = ?,
                streak = ?,
                best_streak = ?
          WHERE guild_id = ? AND user_id = ?",
    )
    .bind(&today)
    .bind(new_streak)
    .bind(new_best)
    .bind(guild_id)
    .bind(user_id)
    .execute(db)
    .await?;

    Ok(streak_bonus(new_streak))
}

fn streak_bonus(streak: i64) -> i64 {
    match streak {
        0..=2 => 0,
        3..=6 => 2,
        7..=29 => 5,
        _ => 10,
    }
}

/// Add XP, roll over levels, and report a level-up if one happened.
///
/// The write is a compare-and-set: the `UPDATE` only lands if the row still
/// holds the values we based the new ones on. A plain read-then-write loses an
/// award whenever two land together - two messages in flight, or a message and
/// the voice-XP tick - because the second write overwrites the first.
pub async fn add_xp(
    db: &Db,
    guild_id: i64,
    user_id: i64,
    amount: i64,
) -> Result<Option<LevelUp>, Error> {
    ensure_row(db, guild_id, user_id).await?;

    const ATTEMPTS: usize = 5;
    let mut level = 0i64;
    let mut start_level = 0i64;
    let mut committed = false;

    for _ in 0..ATTEMPTS {
        let (read_xp, read_level, read_total): (i64, i64, i64) = sqlx::query_as(
            "SELECT xp, level, total_xp FROM levels WHERE guild_id = ? AND user_id = ?",
        )
        .bind(guild_id)
        .bind(user_id)
        .fetch_one(db)
        .await?;

        let new_total = (read_total + amount).max(0);
        start_level = read_level;

        let (xp, new_level) = roll_level(read_xp + amount, read_level);
        level = new_level;

        let affected = sqlx::query(
            "UPDATE levels SET xp = ?, level = ?, total_xp = ?, last_xp = ?
             WHERE guild_id = ? AND user_id = ? AND xp = ? AND total_xp = ?",
        )
        .bind(xp)
        .bind(level)
        .bind(new_total)
        .bind(crate::util::time::sql_now())
        .bind(guild_id)
        .bind(user_id)
        .bind(read_xp)
        .bind(read_total)
        .execute(db)
        .await?
        .rows_affected();

        if affected == 1 {
            committed = true;
            break;
        }
        // Somebody else got there first; read the new values and redo the sum.
    }

    if !committed {
        tracing::warn!(
            guild = guild_id,
            user = user_id,
            amount,
            "gave up adding XP after {ATTEMPTS} attempts under contention"
        );
        return Ok(None);
    }

    if level <= start_level {
        return Ok(None);
    }

    let rewards: Vec<i64> = sqlx::query_scalar(
        "SELECT role_id FROM level_rewards WHERE guild_id = ? AND level <= ? ORDER BY level",
    )
    .bind(guild_id)
    .bind(level)
    .fetch_all(db)
    .await
    .unwrap_or_default();

    let new_achievements = check_achievements(db, guild_id, user_id).await;

    Ok(Some(LevelUp {
        level,
        rewards,
        new_achievements,
    }))
}

/// Announce the level-up and hand out role rewards.
pub async fn apply_levelup(
    sctx: &serenity::Context,
    data: &Data,
    guild_id: GuildId,
    user_id: UserId,
    origin_channel: Option<ChannelId>,
    cfg: &LevelingConfig,
    levelup: LevelUp,
) {
    // ---- role rewards ----------------------------------------------------
    let mut granted = Vec::new();
    if !levelup.rewards.is_empty() {
        if cfg.stack_rewards == 1 {
            for role_id in &levelup.rewards {
                if compat::add_role(&sctx.http, guild_id, user_id, RoleId::new(*role_id as u64))
                    .await
                    .is_ok()
                {
                    granted.push(*role_id);
                }
            }
        } else {
            // Only the highest reward is kept; strip the lower ones.
            if let Some(top) = levelup.rewards.last() {
                if compat::add_role(&sctx.http, guild_id, user_id, RoleId::new(*top as u64))
                    .await
                    .is_ok()
                {
                    granted.push(*top);
                }
                for role_id in levelup.rewards.iter().filter(|r| *r != top) {
                    let _ = compat::remove_role(
                        &sctx.http,
                        guild_id,
                        user_id,
                        RoleId::new(*role_id as u64),
                    )
                    .await;
                }
            }
        }
    }

    if cfg.announce == 0 && cfg.levelup_dm == 0 {
        return;
    }

    let vars = vec![
        ("mention", format!("<@{}>", user_id.get())),
        ("level", levelup.level.to_string()),
        (
            "server",
            crate::modules::moderation::guild_name(sctx, guild_id),
        ),
    ];
    let mut body = text::render(&cfg.levelup_message, &vars);

    if !granted.is_empty() {
        body.push_str(&format!(
            "\nYou unlocked {}.",
            granted
                .iter()
                .map(|r| format!("<@&{r}>"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !levelup.new_achievements.is_empty() {
        body.push_str(&format!(
            "\n🏅 New achievement: {}",
            levelup
                .new_achievements
                .iter()
                .map(|k| achievement_label(k))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    let announcement = embed::titled("Level up!", embed::XP).description(body.clone());

    if cfg.announce == 1 {
        let channel = cfg
            .levelup_channel
            .map(|c| ChannelId::new(c as u64))
            .or(origin_channel);
        if let Some(channel) = channel {
            let _ = channel
                .send_message(&sctx.http, CreateMessage::new().embed(announcement.clone()))
                .await;
        }
    }

    if cfg.levelup_dm == 1 {
        if let Ok(channel) = user_id.create_dm_channel(sctx).await {
            let _ = channel
                .send_message(&sctx.http, CreateMessage::new().embed(announcement))
                .await;
        }
    }

    let _ = data;
}

// ---------------------------------------------------------------------------
// Voice XP
// ---------------------------------------------------------------------------

/// Grant voice XP for `seconds` spent in a voice channel and record the time.
pub async fn award_voice(
    sctx: &serenity::Context,
    data: &Data,
    guild_id: GuildId,
    user_id: UserId,
    seconds: i64,
) -> Result<(), Error> {
    if seconds < 60 {
        return Ok(());
    }
    let guild_i64 = guild_id.get() as i64;
    let user_i64 = user_id.get() as i64;

    let cfg = db::leveling_config(&data.db, guild_i64).await?;
    ensure_row(&data.db, guild_i64, user_i64).await?;

    sqlx::query("UPDATE levels SET voice_secs = voice_secs + ? WHERE guild_id = ? AND user_id = ?")
        .bind(seconds)
        .bind(guild_i64)
        .bind(user_i64)
        .execute(&data.db)
        .await?;

    if cfg.enabled == 0 || cfg.voice_enabled == 0 || cfg.voice_xp_per_min <= 0 {
        return Ok(());
    }

    let minutes = seconds / 60;
    let amount = ((minutes * cfg.voice_xp_per_min) as f64 * cfg.global_rate.max(0.0)).round() as i64;
    if amount <= 0 {
        return Ok(());
    }

    if let Some(levelup) = add_xp(&data.db, guild_i64, user_i64, amount).await? {
        apply_levelup(sctx, data, guild_id, user_id, None, &cfg, levelup).await;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Achievements
// ---------------------------------------------------------------------------

/// Every achievement the bot can award, with its display label.
pub const ACHIEVEMENTS: &[(&str, &str)] = &[
    ("first_steps", "👣 First Steps"),
    ("level_10", "🔟 Getting Started"),
    ("level_25", "⭐ Regular"),
    ("level_50", "🌟 Veteran"),
    ("level_100", "💫 Centurion"),
    ("prestige_1", "♻️ Prestiged"),
    ("chatterbox", "💬 Chatterbox"),
    ("voice_veteran", "🎙️ Voice Veteran"),
    ("streak_7", "🔥 Week Streak"),
    ("streak_30", "🏆 Month Streak"),
    ("trivia_buff", "🧠 Trivia Buff"),
    // No longer awarded (it counted closed tickets); kept so badges already
    // earned still show their name.
    ("helper", "🤝 Helper"),
];

pub fn achievement_label(key: &str) -> String {
    ACHIEVEMENTS
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, label)| label.to_string())
        .unwrap_or_else(|| format!("🏅 {key}"))
}

/// Grant any achievement whose condition is now met. Returns newly earned keys.
pub async fn check_achievements(db: &Db, guild_id: i64, user_id: i64) -> Vec<String> {
    let row: Option<(i64, i64, i64, i64, i64)> = sqlx::query_as(
        "SELECT level, prestige, messages, voice_secs, best_streak
         FROM levels WHERE guild_id = ? AND user_id = ?",
    )
    .bind(guild_id)
    .bind(user_id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();

    let Some((level, prestige, messages, voice_secs, best_streak)) = row else {
        return Vec::new();
    };

    let trivia: i64 = sqlx::query_scalar(
        "SELECT COALESCE(correct, 0) FROM trivia_scores WHERE guild_id = ? AND user_id = ?",
    )
    .bind(guild_id)
    .bind(user_id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .unwrap_or(0);

    let candidates: Vec<(&str, bool)> = vec![
        ("first_steps", messages >= 1),
        ("level_10", level >= 10),
        ("level_25", level >= 25),
        ("level_50", level >= 50),
        ("level_100", level >= 100),
        ("prestige_1", prestige >= 1),
        ("chatterbox", messages >= 1_000),
        ("voice_veteran", voice_secs >= 36_000),
        ("streak_7", best_streak >= 7),
        ("streak_30", best_streak >= 30),
        ("trivia_buff", trivia >= 50),
    ];

    let mut earned = Vec::new();
    for (key, met) in candidates {
        if !met {
            continue;
        }
        let result = sqlx::query(
            "INSERT OR IGNORE INTO achievements (guild_id, user_id, key) VALUES (?, ?, ?)",
        )
        .bind(guild_id)
        .bind(user_id)
        .bind(key)
        .execute(db)
        .await;
        if let Ok(result) = result {
            if result.rows_affected() > 0 {
                earned.push(key.to_string());
            }
        }
    }
    earned
}

pub fn commands() -> Vec<poise::Command<Data, Error>> {
    commands::commands()
}
