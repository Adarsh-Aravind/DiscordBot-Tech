//! Database layer: pool setup, schema bootstrap, typed config rows and small helpers.

use std::str::FromStr;
use std::time::Duration;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{FromRow, SqlitePool};

pub type Db = SqlitePool;

const SCHEMA: &str = include_str!("../migrations/schema.sql");

pub async fn connect(url: &str) -> anyhow::Result<Db> {
    // Make sure the parent directory of a file-backed database exists.
    if let Some(path) = url.strip_prefix("sqlite://") {
        let path = path.split('?').next().unwrap_or(path);
        if path != ":memory:" {
            if let Some(dir) = std::path::Path::new(path).parent() {
                if !dir.as_os_str().is_empty() {
                    std::fs::create_dir_all(dir).ok();
                }
            }
        }
    }

    let opts = SqliteConnectOptions::from_str(url)?
        .create_if_missing(true)
        .foreign_keys(true)
        .busy_timeout(Duration::from_secs(15));

    let pool = SqlitePoolOptions::new()
        .max_connections(8)
        .acquire_timeout(Duration::from_secs(20))
        .connect_with(opts)
        .await?;

    bootstrap(&pool).await?;
    Ok(pool)
}

/// Run the schema. Every statement is `IF NOT EXISTS`, so this is safe on each boot.
async fn bootstrap(pool: &Db) -> anyhow::Result<()> {
    for statement in SCHEMA.split(';') {
        let trimmed = statement.trim();
        // Skip fragments that are only comments / whitespace.
        let is_blank = trimmed
            .lines()
            .map(str::trim)
            .all(|l| l.is_empty() || l.starts_with("--"));
        if is_blank {
            continue;
        }
        sqlx::query(trimmed)
            .execute(pool)
            .await
            .map_err(|e| anyhow::anyhow!("schema statement failed: {e}\n--- sql ---\n{trimmed}"))?;
    }
    seed_trivia(pool).await?;
    seed_daily_questions(pool).await?;
    Ok(())
}

// ===========================================================================
// Typed config rows
// ===========================================================================

#[derive(Debug, Clone, FromRow)]
pub struct GuildConfig {
    pub guild_id: i64,
    pub prefix: String,
    pub modlog_channel: Option<i64>,
    pub msglog_channel: Option<i64>,
    pub joinlog_channel: Option<i64>,
    pub voicelog_channel: Option<i64>,
    pub serverlog_channel: Option<i64>,
    pub mute_role: Option<i64>,
    pub mod_roles: String,
    pub admin_roles: String,
    pub staff_roles: String,
    pub birthday_channel: Option<i64>,
    pub birthday_role: Option<i64>,
    pub appeal_channel: Option<i64>,
    pub report_channel: Option<i64>,
    pub timezone: String,
    pub locked_down: i64,
}

#[derive(Debug, Clone, FromRow)]
pub struct AutomodConfig {
    pub guild_id: i64,
    pub antispam: i64,
    pub antispam_msgs: i64,
    pub antispam_window: i64,
    pub antiflood: i64,
    pub antiflood_msgs: i64,
    pub antiflood_window: i64,
    pub antiraid: i64,
    pub antiraid_joins: i64,
    pub antiraid_window: i64,
    pub antiraid_action: String,
    pub antinuke: i64,
    pub antinuke_threshold: i64,
    pub antinuke_window: i64,
    pub antiphish: i64,
    pub antiinvite: i64,
    pub antimention: i64,
    pub mention_limit: i64,
    pub badwords_enabled: i64,
    pub duplicate_enabled: i64,
    pub duplicate_limit: i64,
    pub caps_enabled: i64,
    pub caps_percent: i64,
    pub suspicious_accounts: i64,
    pub suspicious_min_age: i64,
    pub suspicious_action: String,
    pub alert_channel: Option<i64>,
    pub delete_violations: i64,
}

#[derive(Debug, Clone, FromRow)]
pub struct WelcomeConfig {
    pub guild_id: i64,
    pub welcome_channel: Option<i64>,
    pub welcome_message: String,
    pub welcome_embed: i64,
    pub welcome_image: Option<String>,
    pub welcome_dm: Option<String>,
    pub goodbye_channel: Option<i64>,
    pub goodbye_message: String,
    pub autoroles: String,
    pub bot_autoroles: String,
    pub rules_channel: Option<i64>,
    pub rules_message: Option<i64>,
    pub rules_role: Option<i64>,
    pub verify_channel: Option<i64>,
    pub verify_role: Option<i64>,
    pub verify_mode: String,
    pub verify_min_age: i64,
}

#[derive(Debug, Clone, FromRow)]
pub struct LevelingConfig {
    pub guild_id: i64,
    pub enabled: i64,
    pub xp_min: i64,
    pub xp_max: i64,
    pub cooldown: i64,
    pub voice_xp_per_min: i64,
    pub voice_enabled: i64,
    pub levelup_channel: Option<i64>,
    pub levelup_message: String,
    pub levelup_dm: i64,
    pub announce: i64,
    pub stack_rewards: i64,
    pub global_rate: f64,
    pub prestige_level: i64,
    pub no_xp_channels: String,
    pub no_xp_roles: String,
}

#[derive(Debug, Clone, FromRow)]
pub struct TicketConfig {
    pub guild_id: i64,
    pub category_id: Option<i64>,
    pub log_channel: Option<i64>,
    pub transcript_channel: Option<i64>,
    pub support_roles: String,
    pub max_open: i64,
    pub auto_close_hours: i64,
    pub ask_feedback: i64,
    pub next_number: i64,
}

#[derive(Debug, Clone, FromRow)]
pub struct VoiceConfig {
    pub guild_id: i64,
    pub j2c_channel: Option<i64>,
    pub j2c_category: Option<i64>,
    pub default_name: String,
    pub default_limit: i64,
    pub afk_channel: Option<i64>,
    pub afk_timeout: i64,
    pub delete_empty: i64,
    pub empty_grace: i64,
}

#[derive(Debug, Clone, FromRow)]
pub struct StarboardConfig {
    pub guild_id: i64,
    pub channel_id: Option<i64>,
    pub threshold: i64,
    pub emoji: String,
    pub self_star: i64,
    pub ignore_bots: i64,
    pub nsfw_allowed: i64,
}

// ===========================================================================
// Row bootstrapping + fetching
// ===========================================================================

/// Insert the default row in every per-guild config table. Cheap and idempotent.
pub async fn ensure_guild(db: &Db, guild_id: i64) -> anyhow::Result<()> {
    for table in [
        "guild_config",
        "automod_config",
        "welcome_config",
        "leveling_config",
        "ticket_config",
        "voice_config",
        "starboard_config",
    ] {
        let sql = format!("INSERT OR IGNORE INTO {table} (guild_id) VALUES (?)");
        sqlx::query(&sql).bind(guild_id).execute(db).await?;
    }
    Ok(())
}

macro_rules! fetch_config {
    ($name:ident, $ty:ty, $table:literal) => {
        pub async fn $name(db: &Db, guild_id: i64) -> anyhow::Result<$ty> {
            let sql = concat!("SELECT * FROM ", $table, " WHERE guild_id = ?");
            if let Some(row) = sqlx::query_as::<_, $ty>(sql)
                .bind(guild_id)
                .fetch_optional(db)
                .await?
            {
                return Ok(row);
            }
            ensure_guild(db, guild_id).await?;
            Ok(sqlx::query_as::<_, $ty>(sql)
                .bind(guild_id)
                .fetch_one(db)
                .await?)
        }
    };
}

fetch_config!(guild_config, GuildConfig, "guild_config");
fetch_config!(automod_config, AutomodConfig, "automod_config");
fetch_config!(welcome_config, WelcomeConfig, "welcome_config");
fetch_config!(leveling_config, LevelingConfig, "leveling_config");
fetch_config!(ticket_config, TicketConfig, "ticket_config");
fetch_config!(voice_config, VoiceConfig, "voice_config");
fetch_config!(starboard_config, StarboardConfig, "starboard_config");

/// Update a single column in a per-guild config table.
/// `table` and `column` are never user supplied — always literals from a command module.
macro_rules! setter {
    ($name:ident, $ty:ty) => {
        pub async fn $name(
            db: &Db,
            table: &str,
            column: &str,
            guild_id: i64,
            value: $ty,
        ) -> anyhow::Result<()> {
            ensure_guild(db, guild_id).await?;
            let sql = format!("UPDATE {table} SET {column} = ? WHERE guild_id = ?");
            sqlx::query(&sql)
                .bind(value)
                .bind(guild_id)
                .execute(db)
                .await?;
            Ok(())
        }
    };
}

setter!(set_i64, i64);
setter!(set_opt_i64, Option<i64>);
setter!(set_text, String);
setter!(set_opt_text, Option<String>);
setter!(set_real, f64);

/// Convenience wrapper for the many boolean-ish `INTEGER NOT NULL DEFAULT 0/1` columns.
pub async fn set_bool(
    db: &Db,
    table: &str,
    column: &str,
    guild_id: i64,
    value: bool,
) -> anyhow::Result<()> {
    set_i64(db, table, column, guild_id, i64::from(value)).await
}

// ===========================================================================
// Key/value settings for the long tail
// ===========================================================================

pub async fn kv_get(db: &Db, guild_id: i64, key: &str) -> Option<String> {
    sqlx::query_scalar::<_, String>("SELECT value FROM kv_settings WHERE guild_id = ? AND key = ?")
        .bind(guild_id)
        .bind(key)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
}

pub async fn kv_set(db: &Db, guild_id: i64, key: &str, value: &str) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO kv_settings (guild_id, key, value) VALUES (?, ?, ?)
         ON CONFLICT(guild_id, key) DO UPDATE SET value = excluded.value",
    )
    .bind(guild_id)
    .bind(key)
    .bind(value)
    .execute(db)
    .await?;
    Ok(())
}

pub async fn kv_get_i64(db: &Db, guild_id: i64, key: &str, default: i64) -> i64 {
    kv_get(db, guild_id, key)
        .await
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

// ===========================================================================
// Small shared helpers
// ===========================================================================

/// Parse a comma separated id list as stored in config columns.
pub fn csv_ids(raw: &str) -> Vec<i64> {
    raw.split(',')
        .filter_map(|s| s.trim().parse::<i64>().ok())
        .collect()
}

pub fn ids_csv(ids: &[i64]) -> String {
    ids.iter()
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join(",")
}

/// Next sequential number for a per-guild counter (cases, suggestions, quotes...).
pub async fn next_number(db: &Db, table: &str, column: &str, guild_id: i64) -> anyhow::Result<i64> {
    let sql = format!("SELECT COALESCE(MAX({column}), 0) + 1 FROM {table} WHERE guild_id = ?");
    let n: i64 = sqlx::query_scalar(&sql).bind(guild_id).fetch_one(db).await?;
    Ok(n)
}

// ===========================================================================
// Seed data
// ===========================================================================

async fn seed_trivia(db: &Db) -> anyhow::Result<()> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM trivia_questions WHERE guild_id = 0")
        .fetch_one(db)
        .await?;
    if count > 0 {
        return Ok(());
    }

    // question, answer, three wrong answers, difficulty
    let seed: &[(&str, &str, [&str; 3], &str)] = &[
        ("What does CPU stand for?", "Central Processing Unit",
         ["Computer Personal Unit", "Central Program Utility", "Core Power Unit"], "easy"),
        ("How many bits are in a byte?", "8", ["4", "16", "32"], "easy"),
        ("What does RAM stand for?", "Random Access Memory",
         ["Rapid Access Module", "Read Access Memory", "Runtime Allocated Memory"], "easy"),
        ("Which company develops the Rust programming language toolchain today?", "The Rust Foundation",
         ["Microsoft", "Oracle", "Canonical"], "medium"),
        ("What port does HTTPS use by default?", "443", ["80", "8080", "22"], "easy"),
        ("What does SSD stand for?", "Solid State Drive",
         ["Super Speed Disk", "Secondary Storage Device", "Serial Storage Drive"], "easy"),
        ("Which protocol resolves domain names to IP addresses?", "DNS", ["DHCP", "FTP", "SMTP"], "easy"),
        ("What is the binary representation of the decimal number 10?", "1010",
         ["1100", "1001", "1110"], "medium"),
        ("Who is credited with writing the first computer algorithm?", "Ada Lovelace",
         ["Grace Hopper", "Alan Turing", "Charles Babbage"], "medium"),
        ("What does GPU stand for?", "Graphics Processing Unit",
         ["General Purpose Unit", "Graphical Power Unit", "Grid Processing Unit"], "easy"),
        ("Which company created the first commercially available microprocessor?", "Intel",
         ["AMD", "IBM", "Motorola"], "medium"),
        ("What does the acronym API stand for?", "Application Programming Interface",
         ["Applied Program Instruction", "Automated Process Integration", "Application Process Identifier"], "easy"),
        ("In networking, what does VPN stand for?", "Virtual Private Network",
         ["Verified Public Node", "Virtual Protected Name", "Variable Packet Network"], "easy"),
        ("What is the maximum value of an unsigned 8-bit integer?", "255",
         ["256", "127", "128"], "medium"),
        ("Which key combination force-quits an application on Windows?", "Alt + F4",
         ["Ctrl + Q", "Ctrl + W", "Shift + Esc"], "easy"),
        ("What does HTTP status code 404 mean?", "Not Found",
         ["Server Error", "Forbidden", "Unauthorized"], "easy"),
        ("Which language is primarily used to style web pages?", "CSS",
         ["HTML", "XML", "JSON"], "easy"),
        ("What does SQL stand for?", "Structured Query Language",
         ["Simple Question Language", "Sequential Query Logic", "Server Query Layer"], "easy"),
        ("Roughly how many bytes are in a kibibyte?", "1024", ["1000", "512", "2048"], "medium"),
        ("What hardware component stores the BIOS/UEFI firmware?", "Flash memory chip on the motherboard",
         ["The hard drive", "The CPU cache", "The RAM modules"], "hard"),
        ("Which wireless standard is also known as Wi-Fi 6?", "802.11ax",
         ["802.11ac", "802.11n", "802.11be"], "hard"),
        ("What does the 'G' in 5G stand for?", "Generation",
         ["Gigahertz", "Global", "Grid"], "easy"),
        ("Which sorting algorithm has O(n log n) average time and is in-place?", "Quicksort",
         ["Bubble sort", "Merge sort", "Counting sort"], "hard"),
        ("What is the default SSH port?", "22", ["21", "23", "25"], "easy"),
        ("What does OLED stand for?", "Organic Light Emitting Diode",
         ["Optical Light Emission Display", "Organic Layered Electronic Display", "Oscillating LED"], "medium"),
    ];

    for (q, a, wrong, diff) in seed {
        let mut choices = vec![*a];
        choices.extend_from_slice(wrong);
        let json = serde_json::to_string(&choices)?;
        sqlx::query(
            "INSERT INTO trivia_questions (guild_id, question, answer, choices, category, difficulty)
             VALUES (0, ?, ?, ?, 'tech', ?)",
        )
        .bind(q)
        .bind(a)
        .bind(json)
        .bind(diff)
        .execute(db)
        .await?;
    }
    Ok(())
}

async fn seed_daily_questions(db: &Db) -> anyhow::Result<()> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM daily_questions WHERE guild_id = 0")
        .fetch_one(db)
        .await?;
    if count > 0 {
        return Ok(());
    }
    const QUESTIONS: &[&str] = &[
        "What is the single best tech purchase you have ever made?",
        "Which piece of old hardware do you still miss?",
        "What is your current desktop or phone wallpaper?",
        "Mechanical keyboard or low profile, and why?",
        "What is the most useless gadget you own?",
        "Which tech company do you think will not exist in ten years?",
        "What is your go-to app that nobody else seems to use?",
        "Windows, macOS or Linux, and defend it in one sentence.",
        "What was your first ever computer or console?",
        "Which upcoming piece of tech are you most excited about?",
        "What is the worst tech advice you have ever been given?",
        "How many browser tabs do you have open right now?",
        "What is the longest you have gone without your phone?",
        "Which YouTube video taught you something genuinely useful?",
        "What piece of software would you pay double for?",
        "What is the strangest bug you have ever run into?",
        "Wired or wireless headphones, and why?",
        "What is one feature you wish your phone still had?",
        "Which tech trend do you think is completely overhyped?",
        "What does your dream setup look like?",
    ];
    for q in QUESTIONS {
        sqlx::query("INSERT INTO daily_questions (guild_id, question) VALUES (0, ?)")
            .bind(q)
            .execute(db)
            .await?;
    }
    Ok(())
}
