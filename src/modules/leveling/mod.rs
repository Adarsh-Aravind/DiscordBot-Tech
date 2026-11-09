//! Leveling: the XP engine, level curve, role rewards, streaks and achievements.

pub mod commands;

use chrono::Utc;
use poise::serenity_prelude as serenity;
use serenity::{ChannelId, CreateMessage, GuildId, Message, RoleId, UserId};

use crate::compat;
use crate::db::{self, Db, LevelingConfig};
use crate::state::{Data, Error};
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

    // Message count and activity always tick, even inside the XP cooldown.
    sqlx::query(
        "UPDATE levels SET messages = messages + 1 WHERE guild_id = ? AND user_id = ?",
    )
    .bind(guild_i64)
    .bind(user_i64)
    .execute(&data.db)
    .await?;

    let last_xp: Option<String> =
        sqlx::query_scalar("SELECT last_xp FROM levels WHERE guild_id = ? AND user_id = ?")
            .bind(guild_i64)
            .bind(user_i64)
            .fetch_optional(&data.db)
            .await?
            .flatten();

    let on_cooldown = last_xp
        .as_deref()
        .and_then(crate::util::time::from_sql)
        .map(|t| (Utc::now() - t).num_seconds() < cfg.cooldown)
        .unwrap_or(false);

    // Daily streak is independent of the XP cooldown.
    let streak_bonus = update_streak(&data.db, guild_i64, user_i64).await?;

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

/// Highest applicable role multiplier times the channel multiplier times the
/// server-wide rate.
async fn resolve_multiplier(
    db: &Db,
    guild_id: i64,
    channel_id: i64,
    roles: &[i64],
    global_rate: f64,
) -> f64 {
    let rows: Vec<(String, i64, f64)> =
        sqlx::query_as("SELECT kind, target_id, multiplier FROM xp_multipliers WHERE guild_id = ?")
            .bind(guild_id)
            .fetch_all(db)
            .await
            .unwrap_or_default();

    let mut role_mult: f64 = 1.0;
    let mut channel_mult: f64 = 1.0;
    for (kind, target, multiplier) in rows {
        match kind.as_str() {
            "role" if roles.contains(&target) => role_mult = role_mult.max(multiplier),
            "channel" if target == channel_id => channel_mult = multiplier,
            _ => {}
        }
    }
    (global_rate.max(0.0) * role_mult * channel_mult).clamp(0.0, 20.0)
}

/// Maintain the daily activity streak. Returns a small XP bonus for long runs.
async fn update_streak(db: &Db, guild_id: i64, user_id: i64) -> Result<i64, Error> {
    let today = Utc::now().format("%Y-%m-%d").to_string();
    let yesterday = (Utc::now() - chrono::Duration::days(1))
        .format("%Y-%m-%d")
        .to_string();

    let row: Option<(Option<String>, i64, i64)> = sqlx::query_as(
        "SELECT last_active, streak, best_streak FROM levels WHERE guild_id = ? AND user_id = ?",
    )
    .bind(guild_id)
    .bind(user_id)
    .fetch_optional(db)
    .await?;

    let (last_active, streak, best) = row.unwrap_or((None, 0, 0));
    let last = last_active.unwrap_or_default();

    if last == today {
        return Ok(streak_bonus(streak));
    }

    let new_streak = if last == yesterday { streak + 1 } else { 1 };
    let new_best = best.max(new_streak);

    sqlx::query(
        "UPDATE levels SET last_active = ?, streak = ?, best_streak = ?
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
pub async fn add_xp(
    db: &Db,
    guild_id: i64,
    user_id: i64,
    amount: i64,
) -> Result<Option<LevelUp>, Error> {
    ensure_row(db, guild_id, user_id).await?;

    let (mut xp, mut level, total): (i64, i64, i64) = sqlx::query_as(
        "SELECT xp, level, total_xp FROM levels WHERE guild_id = ? AND user_id = ?",
    )
    .bind(guild_id)
    .bind(user_id)
    .fetch_one(db)
    .await?;

    xp += amount;
    let new_total = (total + amount).max(0);
    let start_level = level;

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

    sqlx::query(
        "UPDATE levels SET xp = ?, level = ?, total_xp = ?, last_xp = ?
         WHERE guild_id = ? AND user_id = ?",
    )
    .bind(xp)
    .bind(level)
    .bind(new_total)
    .bind(crate::util::time::sql_now())
    .bind(guild_id)
    .bind(user_id)
    .execute(db)
    .await?;

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

    crate::modules::analytics::record_voice(&data.db, guild_i64, user_i64, seconds).await;

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

    let closed_tickets: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM tickets WHERE guild_id = ? AND claimed_by = ? AND status = 'closed'",
    )
    .bind(guild_id)
    .bind(user_id)
    .fetch_one(db)
    .await
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
        ("helper", closed_tickets >= 25),
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
