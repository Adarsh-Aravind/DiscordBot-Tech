//! Shared bot state. One instance lives for the whole process and is handed to
//! every command and event handler by poise.

use std::collections::HashSet;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::time::Instant;

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use sqlx::SqlitePool;

use crate::config::Config;

pub type Error = Box<dyn std::error::Error + Send + Sync>;

/// The data poise hands to every command and event handler.
///
/// It is an `Arc` so the background tasks in [`crate::tasks`] can hold the same
/// state. Field access goes through `Deref`, so `data.db` reads the same either
/// way and no call site needs to know.
pub type Data = Arc<BotState>;
pub type Ctx<'a> = poise::Context<'a, Data, Error>;
pub type AppCtx<'a> = poise::ApplicationContext<'a, Data, Error>;

/// A message we remember so `/snipe` and the edit/delete logs have something to show.
#[derive(Clone, Debug)]
pub struct Snipe {
    pub author_id: u64,
    pub author_tag: String,
    pub content: String,
    pub before: Option<String>,
    pub attachments: Vec<String>,
    pub at: DateTime<Utc>,
}

/// Sliding-window counter used by anti-spam / anti-flood / anti-raid.
#[derive(Default, Debug)]
pub struct Window {
    pub hits: Vec<Instant>,
}

impl Window {
    /// Record one hit, drop anything older than `window` seconds, return the count.
    pub fn bump(&mut self, window_secs: u64) -> usize {
        let now = Instant::now();
        self.hits
            .retain(|t| now.duration_since(*t).as_secs() < window_secs);
        self.hits.push(now);
        self.hits.len()
    }

    pub fn count(&self, window_secs: u64) -> usize {
        let now = Instant::now();
        self.hits
            .iter()
            .filter(|t| now.duration_since(**t).as_secs() < window_secs)
            .count()
    }
}

pub struct BotState {
    pub db: SqlitePool,
    pub config: Config,
    pub started_at: DateTime<Utc>,
    pub http: reqwest::Client,

    // ---- automod sliding windows -------------------------------------------
    /// (guild, user) -> message timestamps, for anti-spam.
    pub spam: DashMap<(u64, u64), Window>,
    /// channel -> message timestamps, for anti-flood.
    pub flood: DashMap<u64, Window>,
    /// guild -> join timestamps, for anti-raid.
    pub joins: DashMap<u64, Window>,
    /// (guild, user) -> last content + repeat count, for duplicate detection.
    pub duplicates: DashMap<(u64, u64), (String, u32)>,
    /// (guild, actor) -> destructive-action timestamps, for anti-nuke.
    pub nuke: DashMap<(u64, u64), Window>,
    /// Guilds currently in raid mode (joins gated, channels locked).
    pub raid_mode: DashMap<u64, DateTime<Utc>>,

    // ---- snipe / logging ---------------------------------------------------
    /// channel -> last deleted message.
    pub snipes: DashMap<u64, Snipe>,
    /// channel -> last edited message.
    pub edit_snipes: DashMap<u64, Snipe>,

    // ---- voice -------------------------------------------------------------
    /// (guild, user) -> moment they joined voice, for session length + voice XP.
    pub voice_since: DashMap<(u64, u64), Instant>,
    /// Temp voice channels pending deletion once empty.
    pub pending_voice_delete: DashMap<u64, Instant>,

    // ---- phishing ----------------------------------------------------------
    pub phish_domains: parking_lot::RwLock<HashSet<String>>,

    // ---- misc --------------------------------------------------------------
    pub commands_run: AtomicU64,
}

impl BotState {
    pub fn new(db: SqlitePool, config: Config) -> Self {
        Self {
            db,
            config,
            started_at: Utc::now(),
            http: reqwest::Client::builder()
                .user_agent("mrtechtrivia-bot/0.1")
                .timeout(std::time::Duration::from_secs(15))
                .build()
                .expect("failed to build HTTP client"),
            spam: DashMap::new(),
            flood: DashMap::new(),
            joins: DashMap::new(),
            duplicates: DashMap::new(),
            nuke: DashMap::new(),
            raid_mode: DashMap::new(),
            snipes: DashMap::new(),
            edit_snipes: DashMap::new(),
            voice_since: DashMap::new(),
            pending_voice_delete: DashMap::new(),
            phish_domains: parking_lot::RwLock::new(default_phish_domains()),
            commands_run: AtomicU64::new(0),
        }
    }

    pub fn is_owner(&self, id: u64) -> bool {
        self.config.owners.contains(&id)
    }
}

/// A small built-in seed list so anti-phishing is useful before any feed loads.
fn default_phish_domains() -> HashSet<String> {
    [
        "discordgift.site",
        "discord-gift.com",
        "discordnitro.info",
        "dliscord.com",
        "discorcl.com",
        "discordapp.io",
        "discord-nitro.org",
        "steamcommunity.ru",
        "steamcomunnity.com",
        "nitro-discord.info",
        "discord-airdrop.com",
        "discrod.gift",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}
