//! Database layer: pool setup, schema bootstrap, typed config rows and small helpers.

use std::str::FromStr;
use std::time::Duration;

use once_cell::sync::Lazy;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{FromRow, SqlitePool};

use crate::util::cache::TtlCache;

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

/// Columns added after the first release.
///
/// `CREATE TABLE IF NOT EXISTS` does nothing to a table that already exists, so
/// a new column needs an `ALTER`. SQLite has no `ADD COLUMN IF NOT EXISTS`, so
/// each of these runs on every boot and the "duplicate column" error that comes
/// back on an already-migrated database is expected.
const ADDITIVE_MIGRATIONS: &[&str] = &["ALTER TABLE kv_settings ADD COLUMN expires_at TEXT"];

/// Tables of features that were removed (analytics, tickets, starboard,
/// suggestions, confessions, invite tracking, scheduled messages, custom
/// commands, workflows, webhooks and weekly reports). Dropped on every boot;
/// `IF EXISTS` makes that a no-op once they are gone. Their indexes go with them.
const RETIRED_TABLES: &[&str] = &[
    "stats_daily",
    "member_activity",
    "channel_activity",
    "hourly_activity",
    "command_usage",
    "ticket_messages",
    "ticket_members",
    "tickets",
    "ticket_categories",
    "ticket_config",
    "starboard_entries",
    "starboard_config",
    "suggestion_votes",
    "suggestions",
    "confessions",
    "invite_cache",
    "invite_stats",
    "scheduled_messages",
    "workflow_queue",
    "workflows",
    "custom_commands",
    "webhook_targets",
    "weekly_reports",
];

/// Columns of removed features on tables that stay. SQLite has no
/// `DROP COLUMN IF EXISTS`, so "no such column" on an already-cleaned
/// database is expected and ignored.
const RETIRED_COLUMNS: &[(&str, &str)] = &[
    ("guild_config", "birthday_channel"),
    ("guild_config", "birthday_role"),
    ("profiles", "birthday"),
    ("members", "inviter_id"),
    ("members", "invite_code"),
];

/// Settings rows and counters that only removed features read.
const RETIRED_ROWS: &[&str] = &[
    "DELETE FROM kv_settings
      WHERE key IN ('confession_channel', 'count_channel', 'count_template', 'suggestion_channel')",
    // Counter names are `table.column`.
    "DELETE FROM counters
      WHERE name LIKE 'suggestions.%' OR name LIKE 'confessions.%' OR name LIKE 'tickets.%'",
];

/// Run the schema. Every statement is `IF NOT EXISTS`, so this is safe on each boot.
async fn bootstrap(pool: &Db) -> anyhow::Result<()> {
    for statement in split_statements(SCHEMA) {
        sqlx::query(&statement)
            .execute(pool)
            .await
            .map_err(|e| anyhow::anyhow!("schema statement failed: {e}\n--- sql ---\n{statement}"))?;
    }

    for statement in ADDITIVE_MIGRATIONS {
        if let Err(why) = sqlx::query(statement).execute(pool).await {
            let message = why.to_string();
            if message.contains("duplicate column") {
                continue;
            }
            return Err(anyhow::anyhow!("migration failed: {statement}: {why}"));
        }
    }

    retire_removed_features(pool).await?;

    seed_trivia(pool).await?;
    seed_daily_questions(pool).await?;
    Ok(())
}

/// Delete the storage of features that no longer exist. Idempotent.
async fn retire_removed_features(pool: &Db) -> anyhow::Result<()> {
    for table in RETIRED_TABLES {
        sqlx::query(&format!("DROP TABLE IF EXISTS {table}"))
            .execute(pool)
            .await
            .map_err(|e| anyhow::anyhow!("could not drop retired table {table}: {e}"))?;
    }

    for (table, column) in RETIRED_COLUMNS {
        let sql = format!("ALTER TABLE {table} DROP COLUMN {column}");
        if let Err(why) = sqlx::query(&sql).execute(pool).await {
            if why.to_string().contains("no such column") {
                continue;
            }
            return Err(anyhow::anyhow!("could not drop retired column {table}.{column}: {why}"));
        }
    }

    for statement in RETIRED_ROWS {
        sqlx::query(statement)
            .execute(pool)
            .await
            .map_err(|e| anyhow::anyhow!("could not clear retired rows: {e}"))?;
    }
    Ok(())
}

/// Split a SQL file into executable statements.
///
/// Naively splitting on `;` breaks the moment the schema gains a trigger or a
/// string literal containing a semicolon, and it breaks silently - half a
/// statement is still valid SQL often enough to be confusing. This tracks
/// quoting and `BEGIN ... END` blocks instead.
fn split_statements(sql: &str) -> Vec<String> {
    let mut statements = Vec::new();
    let mut current = String::new();
    let mut word = String::new();
    let mut in_string = false;
    let mut block_depth = 0usize;
    let mut chars = sql.chars().peekable();

    while let Some(ch) = chars.next() {
        if in_string {
            current.push(ch);
            if ch == '\'' {
                // '' inside a string is an escaped quote, not the end of one.
                if chars.peek() == Some(&'\'') {
                    current.push('\'');
                    chars.next();
                } else {
                    in_string = false;
                }
            }
            continue;
        }

        // A `--` comment runs to the end of the line.
        if ch == '-' && chars.peek() == Some(&'-') {
            for c in chars.by_ref() {
                if c == '\n' {
                    break;
                }
            }
            flush_word(&mut word, &mut block_depth);
            current.push('\n');
            continue;
        }

        if ch == '\'' {
            in_string = true;
            current.push(ch);
            continue;
        }

        if ch.is_alphanumeric() || ch == '_' {
            word.push(ch.to_ascii_uppercase());
            current.push(ch);
            continue;
        }

        // Anything else ends the word we were reading.
        flush_word(&mut word, &mut block_depth);

        if ch == ';' && block_depth == 0 {
            push_statement(&mut statements, &mut current);
        } else {
            current.push(ch);
        }
    }

    flush_word(&mut word, &mut block_depth);
    push_statement(&mut statements, &mut current);
    statements
}

/// `BEGIN ... END` (a trigger body) and `CASE ... END` both nest, and the
/// semicolons inside them belong to the enclosing statement.
fn flush_word(word: &mut String, block_depth: &mut usize) {
    match word.as_str() {
        "BEGIN" | "CASE" => *block_depth += 1,
        "END" => *block_depth = block_depth.saturating_sub(1),
        _ => {}
    }
    word.clear();
}

fn push_statement(statements: &mut Vec<String>, current: &mut String) {
    let trimmed = current.trim();
    if !trimmed.is_empty() {
        statements.push(trimmed.to_string());
    }
    current.clear();
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

// ===========================================================================
// Row bootstrapping + fetching
// ===========================================================================

/// The prefix a brand-new guild row starts with.
///
/// `BOT_PREFIX` is the process-wide default and `/config prefix` overrides it
/// per guild. Now that the framework asks the guild row for its prefix, a new
/// row has to be seeded with the configured default, or `BOT_PREFIX` would
/// quietly stop applying anywhere but DMs.
static DEFAULT_PREFIX: once_cell::sync::OnceCell<String> = once_cell::sync::OnceCell::new();

/// Called once at startup, before any guild row is created.
pub fn set_default_prefix(prefix: &str) {
    let _ = DEFAULT_PREFIX.set(prefix.to_string());
}

fn default_prefix() -> &'static str {
    DEFAULT_PREFIX.get().map(String::as_str).unwrap_or("!")
}

/// Insert the default row in every per-guild config table. Cheap and idempotent.
pub async fn ensure_guild(db: &Db, guild_id: i64) -> anyhow::Result<()> {
    sqlx::query("INSERT OR IGNORE INTO guild_config (guild_id, prefix) VALUES (?, ?)")
        .bind(guild_id)
        .bind(default_prefix())
        .execute(db)
        .await?;

    for table in [
        "automod_config",
        "welcome_config",
        "leveling_config",
        "voice_config",
    ] {
        let sql = format!("INSERT OR IGNORE INTO {table} (guild_id) VALUES (?)");
        sqlx::query(&sql).bind(guild_id).execute(db).await?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Caching
//
// These rows are read on the hot paths - every message asks for the guild,
// leveling and automod settings - and written about once a month. Each read
// goes through a short-lived cache, and every write goes through the setters
// below, which drop the matching entry. A `/config` change is therefore visible
// immediately; the TTL only covers rows edited some other way.
// ---------------------------------------------------------------------------

/// How long a cached config row may be served for.
const CONFIG_TTL_SECS: u64 = 60;
/// Enough for a large bot; over this the cache prunes itself.
const CONFIG_CAPACITY: usize = 4096;

fn config_cache<T: Clone>() -> TtlCache<i64, T> {
    TtlCache::new(CONFIG_TTL_SECS, CONFIG_CAPACITY)
}

static GUILD_CACHE: Lazy<TtlCache<i64, GuildConfig>> = Lazy::new(config_cache);
static AUTOMOD_CACHE: Lazy<TtlCache<i64, AutomodConfig>> = Lazy::new(config_cache);
static WELCOME_CACHE: Lazy<TtlCache<i64, WelcomeConfig>> = Lazy::new(config_cache);
static LEVELING_CACHE: Lazy<TtlCache<i64, LevelingConfig>> = Lazy::new(config_cache);
static VOICE_CACHE: Lazy<TtlCache<i64, VoiceConfig>> = Lazy::new(config_cache);

macro_rules! fetch_config {
    ($name:ident, $ty:ty, $table:literal, $cache:ident) => {
        pub async fn $name(db: &Db, guild_id: i64) -> anyhow::Result<$ty> {
            if let Some(cached) = $cache.get(&guild_id) {
                return Ok(cached);
            }

            let sql = concat!("SELECT * FROM ", $table, " WHERE guild_id = ?");
            let row = match sqlx::query_as::<_, $ty>(sql)
                .bind(guild_id)
                .fetch_optional(db)
                .await?
            {
                Some(row) => row,
                None => {
                    ensure_guild(db, guild_id).await?;
                    sqlx::query_as::<_, $ty>(sql)
                        .bind(guild_id)
                        .fetch_one(db)
                        .await?
                }
            };

            $cache.put(guild_id, row.clone());
            Ok(row)
        }
    };
}

fetch_config!(guild_config, GuildConfig, "guild_config", GUILD_CACHE);
fetch_config!(automod_config, AutomodConfig, "automod_config", AUTOMOD_CACHE);
fetch_config!(welcome_config, WelcomeConfig, "welcome_config", WELCOME_CACHE);
fetch_config!(
    leveling_config,
    LevelingConfig,
    "leveling_config",
    LEVELING_CACHE
);
fetch_config!(voice_config, VoiceConfig, "voice_config", VOICE_CACHE);

/// Forget the cached row for one config table, after writing to it.
pub fn invalidate_config(table: &str, guild_id: i64) {
    match table {
        "guild_config" => GUILD_CACHE.invalidate(&guild_id),
        "automod_config" => AUTOMOD_CACHE.invalidate(&guild_id),
        "welcome_config" => WELCOME_CACHE.invalidate(&guild_id),
        "leveling_config" => LEVELING_CACHE.invalidate(&guild_id),
        "voice_config" => VOICE_CACHE.invalidate(&guild_id),
        _ => {}
    }
}

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
            // Drop the stale copy *after* the write lands, so a concurrent read
            // cannot put it straight back.
            invalidate_config(table, guild_id);
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
    sqlx::query_scalar::<_, String>(
        "SELECT value FROM kv_settings
         WHERE guild_id = ? AND key = ?
           AND (expires_at IS NULL OR expires_at > ?)",
    )
    .bind(guild_id)
    .bind(key)
    .bind(crate::util::time::sql_now())
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
}

pub async fn kv_set(db: &Db, guild_id: i64, key: &str, value: &str) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO kv_settings (guild_id, key, value, expires_at) VALUES (?, ?, ?, NULL)
         ON CONFLICT(guild_id, key) DO UPDATE SET
            value = excluded.value,
            expires_at = excluded.expires_at",
    )
    .bind(guild_id)
    .bind(key)
    .bind(value)
    .execute(db)
    .await?;
    Ok(())
}

/// Store a value that stops being true after `ttl_secs`.
///
/// Used for the short-lived keys - an in-flight trivia answer, the record of
/// who has answered it - so the sweep in [`crate::tasks`] can drop expired rows
/// without taking live games with them.
pub async fn kv_set_with_ttl(
    db: &Db,
    guild_id: i64,
    key: &str,
    value: &str,
    ttl_secs: i64,
) -> anyhow::Result<()> {
    let expires_at =
        crate::util::time::to_sql(chrono::Utc::now() + chrono::Duration::seconds(ttl_secs));
    sqlx::query(
        "INSERT INTO kv_settings (guild_id, key, value, expires_at) VALUES (?, ?, ?, ?)
         ON CONFLICT(guild_id, key) DO UPDATE SET
            value = excluded.value,
            expires_at = excluded.expires_at",
    )
    .bind(guild_id)
    .bind(key)
    .bind(value)
    .bind(expires_at)
    .execute(db)
    .await?;
    Ok(())
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

/// Next sequential number for a per-guild counter (cases, quotes...).
///
/// This is one statement on purpose. Reading `MAX(n) + 1` and then inserting is
/// two steps, and two callers running at the same time - an automod burst, a
/// raid cleanup, two moderators - get handed the same number. For `mod_cases`
/// that meant the second insert hit the unique index *after* the ban had
/// already been carried out.
///
/// The counter seeds itself from the rows that already exist, so an older
/// database carries on where it left off.
pub async fn next_number(db: &Db, table: &str, column: &str, guild_id: i64) -> anyhow::Result<i64> {
    // `table` and `column` are always literals from a command module.
    let sql = format!(
        "INSERT INTO counters (guild_id, name, value)
         VALUES (?, ?, (SELECT COALESCE(MAX({column}), 0) + 1 FROM {table} WHERE guild_id = ?))
         ON CONFLICT(guild_id, name) DO UPDATE SET value = value + 1
         RETURNING value"
    );

    let value: i64 = sqlx::query_scalar(&sql)
        .bind(guild_id)
        .bind(format!("{table}.{column}"))
        .bind(guild_id)
        .fetch_one(db)
        .await?;
    Ok(value)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_plain_statements() {
        let sql = "CREATE TABLE a (x INTEGER);\nCREATE TABLE b (y TEXT);";
        let parts = split_statements(sql);
        assert_eq!(parts.len(), 2);
        assert!(parts[0].starts_with("CREATE TABLE a"));
        assert!(parts[1].starts_with("CREATE TABLE b"));
    }

    #[test]
    fn comments_and_blank_fragments_are_dropped() {
        let sql = "-- a comment\nCREATE TABLE a (x INTEGER);\n\n-- trailing comment\n";
        assert_eq!(split_statements(sql).len(), 1);
    }

    #[test]
    fn semicolons_inside_strings_do_not_split() {
        let sql = "INSERT INTO a (x) VALUES ('one; two');";
        let parts = split_statements(sql);
        assert_eq!(parts.len(), 1, "{parts:?}");
        assert!(parts[0].contains("'one; two'"));
    }

    #[test]
    fn trigger_bodies_stay_in_one_piece() {
        let sql = "CREATE TRIGGER t AFTER INSERT ON a BEGIN \
                   DELETE FROM b; UPDATE c SET d = 1; END;\
                   \nCREATE TABLE e (f INTEGER);";
        let parts = split_statements(sql);
        assert_eq!(parts.len(), 2, "{parts:?}");
        assert!(parts[0].contains("DELETE FROM b"));
        assert!(parts[0].contains("UPDATE c"));
        assert!(parts[1].starts_with("CREATE TABLE e"));
    }

    /// The real schema has to come out in one piece or the bot will not boot.
    #[test]
    fn the_shipped_schema_parses() {
        let parts = split_statements(SCHEMA);
        assert!(parts.len() > 40, "only got {} statements", parts.len());
        for part in &parts {
            let upper = part.to_uppercase();
            assert!(
                upper.starts_with("CREATE") || upper.starts_with("PRAGMA"),
                "unexpected statement: {part}"
            );
        }
    }
}
