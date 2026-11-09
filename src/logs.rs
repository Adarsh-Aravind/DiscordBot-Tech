//! Central log dispatch. Every module routes its audit output through here so the
//! destination channels are configured in exactly one place.

use poise::serenity_prelude as serenity;
use serenity::{ChannelId, CreateEmbed, CreateMessage};

use crate::db::{self, Db};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Log {
    /// Bans, kicks, warns, timeouts, cases.
    Mod,
    /// Message edits and deletions, purges.
    Message,
    /// Joins, leaves, invite attribution, verification.
    Join,
    /// Voice joins, leaves, moves, temp-room lifecycle.
    Voice,
    /// Channel/role/permission changes, anti-nuke, lockdowns.
    Server,
}

impl Log {
    fn column(self) -> &'static str {
        match self {
            Log::Mod => "modlog_channel",
            Log::Message => "msglog_channel",
            Log::Join => "joinlog_channel",
            Log::Voice => "voicelog_channel",
            Log::Server => "serverlog_channel",
        }
    }
}

/// Resolve the configured channel for a log kind, falling back to the modlog.
pub async fn channel_for(db: &Db, guild_id: i64, kind: Log) -> Option<ChannelId> {
    let cfg = db::guild_config(db, guild_id).await.ok()?;
    let direct = match kind {
        Log::Mod => cfg.modlog_channel,
        Log::Message => cfg.msglog_channel,
        Log::Join => cfg.joinlog_channel,
        Log::Voice => cfg.voicelog_channel,
        Log::Server => cfg.serverlog_channel,
    };
    direct
        .or(cfg.modlog_channel)
        .map(|id| ChannelId::new(id as u64))
}

/// Fire-and-forget: a failed log should never break the command that triggered it.
pub async fn send(
    ctx: &serenity::Context,
    db: &Db,
    guild_id: i64,
    kind: Log,
    embed: CreateEmbed,
) {
    let Some(channel) = channel_for(db, guild_id, kind).await else {
        return;
    };
    if let Err(why) = channel
        .send_message(&ctx.http, CreateMessage::new().embed(embed))
        .await
    {
        tracing::warn!(
            guild = guild_id,
            channel = channel.get(),
            ?kind,
            "failed to write log: {why}"
        );
    }
}

/// Same as [`send`] but also returns the message id, for logs we later edit.
pub async fn send_tracked(
    ctx: &serenity::Context,
    db: &Db,
    guild_id: i64,
    kind: Log,
    embed: CreateEmbed,
) -> Option<u64> {
    let channel = channel_for(db, guild_id, kind).await?;
    channel
        .send_message(&ctx.http, CreateMessage::new().embed(embed))
        .await
        .ok()
        .map(|m| m.id.get())
}

/// Which column `/config log` should write for a given kind name.
pub fn column_for_name(name: &str) -> Option<&'static str> {
    Some(match name {
        "mod" => Log::Mod.column(),
        "message" => Log::Message.column(),
        "join" => Log::Join.column(),
        "voice" => Log::Voice.column(),
        "server" => Log::Server.column(),
        _ => return None,
    })
}
