//! Process-level configuration, read once from the environment / `.env`.

use std::env;

#[derive(Debug, Clone)]
pub struct Config {
    /// Bot token from the Discord developer portal.
    pub token: String,
    /// sqlx connection string, e.g. `sqlite://data/bot.db?mode=rwc`.
    pub database_url: String,
    /// Users who may run owner-only commands regardless of guild permissions.
    pub owners: Vec<u64>,
    /// When set, slash commands are registered to this guild only (instant updates).
    pub dev_guild: Option<u64>,
    /// Text prefix for the handful of legacy prefix commands.
    pub prefix: String,
    /// LibreTranslate-compatible endpoint for `/translate`. Optional.
    pub translate_url: Option<String>,
    pub translate_key: Option<String>,
    /// TrueType font used to render rank cards. Falls back to an embed when absent.
    pub font_path: Option<String>,
    /// Phishing-domain feed (newline separated) refreshed on boot. Optional.
    pub phish_list_url: Option<String>,
    /// ops-status heartbeat (the Live Ops panel on adarsharavind.com). All
    /// three must be set or the heartbeat stays off.
    pub status_url: Option<String>,
    pub status_token: Option<String>,
    pub status_bot_id: Option<String>,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let token = env::var("DISCORD_TOKEN")
            .map_err(|_| anyhow::anyhow!("DISCORD_TOKEN is not set (see .env.example)"))?;

        let database_url =
            env::var("DATABASE_URL").unwrap_or_else(|_| "sqlite://data/bot.db?mode=rwc".to_string());

        let owners = env::var("OWNER_IDS")
            .unwrap_or_default()
            .split(',')
            .filter_map(|s| s.trim().parse::<u64>().ok())
            .collect();

        let dev_guild = env::var("DEV_GUILD_ID").ok().and_then(|s| s.trim().parse().ok());

        let prefix = env::var("BOT_PREFIX").unwrap_or_else(|_| "!".to_string());

        Ok(Self {
            token,
            database_url,
            owners,
            dev_guild,
            prefix,
            translate_url: non_empty(env::var("TRANSLATE_API_URL").ok()),
            translate_key: non_empty(env::var("TRANSLATE_API_KEY").ok()),
            font_path: non_empty(env::var("RANK_CARD_FONT").ok()).or_else(default_font),
            phish_list_url: non_empty(env::var("PHISH_LIST_URL").ok()),
            status_url: non_empty(env::var("STATUS_HEARTBEAT_URL").ok()),
            status_token: non_empty(env::var("STATUS_HEARTBEAT_TOKEN").ok()),
            status_bot_id: non_empty(env::var("STATUS_BOT_ID").ok()),
        })
    }
}

fn non_empty(v: Option<String>) -> Option<String> {
    v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// Best-effort guess at a usable system font so rank cards work out of the box.
fn default_font() -> Option<String> {
    const CANDIDATES: &[&str] = &[
        "assets/font.ttf",
        "C:/Windows/Fonts/segoeui.ttf",
        "C:/Windows/Fonts/arial.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
        "/System/Library/Fonts/Supplemental/Arial.ttf",
    ];
    CANDIDATES
        .iter()
        .find(|p| std::path::Path::new(p).exists())
        .map(|p| p.to_string())
}
