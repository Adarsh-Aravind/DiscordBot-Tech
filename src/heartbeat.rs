//! Status heartbeat for the "Live Ops" panel on adarsharavind.com.
//!
//! Every minute the bot POSTs a small "I'm alive" ping to the ops-status server
//! on the home server. Pings are only sent while the gateway shard is
//! connected, so a dropped connection reads as offline even though the process
//! is still up. Inert unless `STATUS_HEARTBEAT_URL`, `STATUS_HEARTBEAT_TOKEN`
//! and `STATUS_BOT_ID` are all set.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use once_cell::sync::OnceCell;
use poise::serenity_prelude as serenity;
use serde_json::{json, Value};
use serenity::gateway::ConnectionStage;
use serenity::ShardManager;

use crate::state::Data;

const INTERVAL: Duration = Duration::from_secs(60);

struct Heartbeat {
    url: String,
    token: String,
    bot_id: String,
    sctx: serenity::Context,
    shards: Arc<ShardManager>,
    data: Data,
    failing: AtomicBool,
}

static HEARTBEAT: OnceCell<Arc<Heartbeat>> = OnceCell::new();

/// Start the once-a-minute ping. Called once the bot is ready.
pub fn start(sctx: serenity::Context, shards: Arc<ShardManager>, data: Data) {
    let cfg = &data.config;
    let (Some(url), Some(token), Some(bot_id)) = (
        cfg.status_url.clone(),
        cfg.status_token.clone(),
        cfg.status_bot_id.clone(),
    ) else {
        tracing::info!("heartbeat disabled (STATUS_HEARTBEAT_URL / _TOKEN / STATUS_BOT_ID not set)");
        return;
    };

    let hb = Arc::new(Heartbeat {
        url,
        token,
        bot_id,
        sctx,
        shards,
        data,
        failing: AtomicBool::new(false),
    });
    if HEARTBEAT.set(hb.clone()).is_err() {
        return; // setup ran twice; the first loop is already going
    }

    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            hb.beat().await;
        }
    });
}

/// One last ping saying the stop was deliberate, so the panel can tell a clean
/// restart from a crash. Short timeout: shutdown must not hang on a status
/// server that is itself down.
pub async fn farewell() {
    let Some(hb) = HEARTBEAT.get() else { return };
    match hb.post(true, Duration::from_secs(2)).await {
        Ok(()) => tracing::info!("heartbeat signed off"),
        Err(e) => tracing::warn!("heartbeat could not sign off: {e}"),
    }
}

impl Heartbeat {
    async fn beat(&self) {
        if !self.connected().await {
            return;
        }
        match self.post(false, Duration::from_secs(10)).await {
            Ok(()) => {
                if self.failing.swap(false, Ordering::Relaxed) {
                    tracing::info!("heartbeat recovered");
                }
            }
            // Once per outage, not once a minute.
            Err(e) => {
                if !self.failing.swap(true, Ordering::Relaxed) {
                    tracing::warn!("heartbeat failed: {e}");
                }
            }
        }
    }

    async fn connected(&self) -> bool {
        let runners = self.shards.runners.lock().await;
        !runners.is_empty() && runners.values().all(|r| r.stage == ConnectionStage::Connected)
    }

    async fn latency_ms(&self) -> Option<u64> {
        let runners = self.shards.runners.lock().await;
        let samples: Vec<u128> = runners
            .values()
            .filter_map(|r| r.latency.map(|d| d.as_millis()))
            .collect();
        if samples.is_empty() {
            None
        } else {
            Some((samples.iter().sum::<u128>() / samples.len() as u128) as u64)
        }
    }

    async fn payload(&self, stopping: bool) -> Value {
        let cache = &self.sctx.cache;
        let (username, avatar_url) = {
            let me = cache.current_user();
            (me.name.clone(), me.face())
        };
        let guild_ids = cache.guilds();
        let members: u64 = guild_ids
            .iter()
            .filter_map(|id| cache.guild(*id).map(|g| g.member_count))
            .sum();

        json!({
            "id": self.bot_id,
            "username": username,
            "avatar_url": avatar_url,
            "guilds": guild_ids.len(),
            "members": members,
            "latency_ms": self.latency_ms().await,
            "started_at": self.data.started_at.to_rfc3339(),
            "stopping": stopping,
        })
    }

    async fn post(&self, stopping: bool, timeout: Duration) -> anyhow::Result<()> {
        let body = self.payload(stopping).await;
        let res = self
            .data
            .http
            .post(&self.url)
            .bearer_auth(&self.token)
            .json(&body)
            .timeout(timeout)
            .send()
            .await?;
        if !res.status().is_success() {
            anyhow::bail!("HTTP {}", res.status());
        }
        Ok(())
    }
}
