//! Channel locking and whole-server emergency lockdown.
//!
//! The previous `@everyone` overwrite is saved before locking so unlocking puts
//! the channel back exactly as it was rather than wiping staff-made permissions.

use poise::serenity_prelude as serenity;
use serenity::{
    ChannelId, ChannelType, GuildChannel, GuildId, PermissionOverwrite, PermissionOverwriteType,
    Permissions, RoleId,
};

use crate::db::Db;
use crate::logs::{self, Log};
use crate::state::{Ctx, Data, Error};
use crate::util::{embed, perms};

const LOCK_DENY: Permissions = Permissions::SEND_MESSAGES;

/// Snapshot of the `@everyone` overwrite, pulled out of the cache.
fn everyone_overwrite(
    sctx: &serenity::Context,
    guild_id: GuildId,
    channel_id: ChannelId,
) -> Option<(i64, i64, bool)> {
    let guild = guild_id.to_guild_cached(&sctx.cache)?;
    let channel = guild.channels.get(&channel_id)?;
    let everyone = RoleId::new(guild_id.get());
    for overwrite in &channel.permission_overwrites {
        if overwrite.kind == PermissionOverwriteType::Role(everyone) {
            return Some((
                overwrite.allow.bits() as i64,
                overwrite.deny.bits() as i64,
                true,
            ));
        }
    }
    Some((0, 0, false))
}

fn text_channels(sctx: &serenity::Context, guild_id: GuildId) -> Vec<ChannelId> {
    match guild_id.to_guild_cached(&sctx.cache) {
        Some(guild) => guild
            .channels
            .iter()
            .filter(|(_, c): &(&ChannelId, &GuildChannel)| {
                matches!(c.kind, ChannelType::Text | ChannelType::News)
            })
            .map(|(id, _)| *id)
            .collect(),
        None => Vec::new(),
    }
}

async fn lock_channel(
    sctx: &serenity::Context,
    db: &Db,
    guild_id: GuildId,
    channel_id: ChannelId,
    reason: &str,
) -> Result<(), Error> {
    let everyone = RoleId::new(guild_id.get());
    let (prev_allow, prev_deny, had) =
        everyone_overwrite(sctx, guild_id, channel_id).unwrap_or((0, 0, false));

    sqlx::query(
        "INSERT INTO lockdowns (guild_id, channel_id, prev_allow, prev_deny, had_override, locked_at)
         VALUES (?, ?, ?, ?, ?, datetime('now'))
         ON CONFLICT(guild_id, channel_id) DO NOTHING",
    )
    .bind(guild_id.get() as i64)
    .bind(channel_id.get() as i64)
    .bind(prev_allow)
    .bind(prev_deny)
    .bind(i64::from(had))
    .execute(db)
    .await?;

    let deny = Permissions::from_bits_truncate(prev_deny as u64) | LOCK_DENY;
    let allow = Permissions::from_bits_truncate(prev_allow as u64) & !LOCK_DENY;

    channel_id
        .create_permission(
            &sctx.http,
            PermissionOverwrite {
                allow,
                deny,
                kind: PermissionOverwriteType::Role(everyone),
            },
            Some(reason),
        )
        .await?;
    Ok(())
}

async fn unlock_channel(
    sctx: &serenity::Context,
    db: &Db,
    guild_id: GuildId,
    channel_id: ChannelId,
    reason: &str,
) -> Result<(), Error> {
    let everyone = RoleId::new(guild_id.get());
    let saved: Option<(i64, i64, i64)> = sqlx::query_as(
        "SELECT prev_allow, prev_deny, had_override FROM lockdowns
         WHERE guild_id = ? AND channel_id = ?",
    )
    .bind(guild_id.get() as i64)
    .bind(channel_id.get() as i64)
    .fetch_optional(db)
    .await?;

    match saved {
        Some((_, _, 0)) => {
            // There was no overwrite before we locked it, so remove ours entirely.
            channel_id
                .delete_permission(
                    &sctx.http,
                    PermissionOverwriteType::Role(everyone),
                    Some(reason),
                )
                .await?;
        }
        Some((allow, deny, _)) => {
            channel_id
                .create_permission(
                    &sctx.http,
                    PermissionOverwrite {
                        allow: Permissions::from_bits_truncate(allow as u64),
                        deny: Permissions::from_bits_truncate(deny as u64),
                        kind: PermissionOverwriteType::Role(everyone),
                    },
                    Some(reason),
                )
                .await?;
        }
        None => {
            // Not recorded: just clear the send-messages denial.
            channel_id
                .create_permission(
                    &sctx.http,
                    PermissionOverwrite {
                        allow: Permissions::empty(),
                        deny: Permissions::empty(),
                        kind: PermissionOverwriteType::Role(everyone),
                    },
                    Some(reason),
                )
                .await?;
        }
    }

    sqlx::query("DELETE FROM lockdowns WHERE guild_id = ? AND channel_id = ?")
        .bind(guild_id.get() as i64)
        .bind(channel_id.get() as i64)
        .execute(db)
        .await?;
    Ok(())
}

/// Stop `@everyone` from talking in a channel.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    category = "Moderation",
    required_permissions = "MANAGE_CHANNELS",
    default_member_permissions = "MANAGE_CHANNELS"
)]
pub async fn lock(
    ctx: Ctx<'_>,
    #[description = "Channel to lock, defaults to this one"] channel: Option<GuildChannel>,
    #[description = "Reason shown in the channel"] reason: Option<String>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap();
    let target = channel.map(|c| c.id).unwrap_or_else(|| ctx.channel_id());
    let reason = reason.unwrap_or_else(|| "No reason provided".to_string());

    ctx.defer().await?;
    lock_channel(
        ctx.serenity_context(),
        &ctx.data().db,
        guild_id,
        target,
        &reason,
    )
    .await?;

    let notice = embed::titled("🔒 Channel locked", embed::WARN)
        .description(format!("**Reason:** {reason}"))
        .footer(embed::footer(format!("Locked by {}", ctx.author().tag())));

    if target == ctx.channel_id() {
        ctx.send(poise::CreateReply::default().embed(notice.clone()))
            .await?;
    } else {
        let _ = target
            .send_message(ctx.http(), serenity::CreateMessage::new().embed(notice.clone()))
            .await;
        ctx.send(
            poise::CreateReply::default()
                .embed(embed::ok(format!("Locked <#{}>.", target.get()))),
        )
        .await?;
    }

    logs::send(
        ctx.serenity_context(),
        &ctx.data().db,
        guild_id.get() as i64,
        Log::Server,
        notice,
    )
    .await;
    Ok(())
}

/// Let `@everyone` talk again.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    category = "Moderation",
    required_permissions = "MANAGE_CHANNELS",
    default_member_permissions = "MANAGE_CHANNELS"
)]
pub async fn unlock(
    ctx: Ctx<'_>,
    #[description = "Channel to unlock, defaults to this one"] channel: Option<GuildChannel>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap();
    let target = channel.map(|c| c.id).unwrap_or_else(|| ctx.channel_id());

    ctx.defer().await?;
    unlock_channel(
        ctx.serenity_context(),
        &ctx.data().db,
        guild_id,
        target,
        "Channel unlocked",
    )
    .await?;

    ctx.send(
        poise::CreateReply::default()
            .embed(embed::ok(format!("🔓 <#{}> is open again.", target.get()))),
    )
    .await?;

    logs::send(
        ctx.serenity_context(),
        &ctx.data().db,
        guild_id.get() as i64,
        Log::Server,
        embed::titled("🔓 Channel unlocked", embed::SUCCESS)
            .description(format!("<#{}>", target.get()))
            .footer(embed::footer(format!("By {}", ctx.author().tag()))),
    )
    .await;
    Ok(())
}

/// Emergency controls for the whole server.
#[poise::command(
    slash_command,
    guild_only,
    category = "Moderation",
    subcommands("lockdown_start", "lockdown_end", "lockdown_status"),
    check = "perms::admin_only"
)]
pub async fn lockdown(_ctx: Ctx<'_>) -> Result<(), Error> {
    Ok(())
}

/// Lock every text channel at once.
#[poise::command(slash_command, rename = "start", guild_only)]
pub async fn lockdown_start(
    ctx: Ctx<'_>,
    #[description = "Why the server is being locked"] reason: Option<String>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap();
    ctx.defer().await?;

    let reason = reason.unwrap_or_else(|| "Emergency lockdown".to_string());
    let channels = text_channels(ctx.serenity_context(), guild_id);
    if channels.is_empty() {
        ctx.send(poise::CreateReply::default().embed(embed::err(
            "I could not read the channel list. Give me a moment after startup and try again.",
        )))
        .await?;
        return Ok(());
    }

    let mut locked = 0usize;
    let mut failed = 0usize;
    for channel in &channels {
        match lock_channel(
            ctx.serenity_context(),
            &ctx.data().db,
            guild_id,
            *channel,
            &reason,
        )
        .await
        {
            Ok(()) => locked += 1,
            Err(_) => failed += 1,
        }
    }

    crate::db::set_i64(
        &ctx.data().db,
        "guild_config",
        "locked_down",
        guild_id.get() as i64,
        1,
    )
    .await?;

    let summary = format!(
        "Locked **{locked}** channel(s){}.\n**Reason:** {reason}",
        if failed > 0 {
            format!(", {failed} failed (missing permissions)")
        } else {
            String::new()
        }
    );

    ctx.send(
        poise::CreateReply::default()
            .embed(embed::titled("🚨 Server lockdown active", embed::DANGER).description(summary.clone())),
    )
    .await?;

    logs::send(
        ctx.serenity_context(),
        &ctx.data().db,
        guild_id.get() as i64,
        Log::Server,
        embed::titled("🚨 Server lockdown started", embed::DANGER)
            .description(summary)
            .footer(embed::footer(format!("By {}", ctx.author().tag()))),
    )
    .await;
    Ok(())
}

/// Reverse a lockdown, restoring the permissions we saved.
#[poise::command(slash_command, rename = "end", guild_only)]
pub async fn lockdown_end(ctx: Ctx<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap();
    ctx.defer().await?;

    let saved: Vec<i64> = sqlx::query_scalar("SELECT channel_id FROM lockdowns WHERE guild_id = ?")
        .bind(guild_id.get() as i64)
        .fetch_all(&ctx.data().db)
        .await?;

    if saved.is_empty() {
        ctx.send(poise::CreateReply::default().embed(embed::warn(
            "Nothing is locked down right now (or it was locked before I started tracking).",
        )))
        .await?;
        return Ok(());
    }

    let mut restored = 0usize;
    for channel_id in saved {
        if unlock_channel(
            ctx.serenity_context(),
            &ctx.data().db,
            guild_id,
            ChannelId::new(channel_id as u64),
            "Lockdown lifted",
        )
        .await
        .is_ok()
        {
            restored += 1;
        }
    }

    crate::db::set_i64(
        &ctx.data().db,
        "guild_config",
        "locked_down",
        guild_id.get() as i64,
        0,
    )
    .await?;

    ctx.send(poise::CreateReply::default().embed(embed::ok(format!(
        "Lockdown lifted. Restored **{restored}** channel(s)."
    ))))
    .await?;

    logs::send(
        ctx.serenity_context(),
        &ctx.data().db,
        guild_id.get() as i64,
        Log::Server,
        embed::titled("✅ Lockdown lifted", embed::SUCCESS)
            .description(format!("{restored} channel(s) restored."))
            .footer(embed::footer(format!("By {}", ctx.author().tag()))),
    )
    .await;
    Ok(())
}

/// Is the server locked down, and which channels?
#[poise::command(slash_command, rename = "status", guild_only)]
pub async fn lockdown_status(ctx: Ctx<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let rows: Vec<(i64, String)> =
        sqlx::query_as("SELECT channel_id, locked_at FROM lockdowns WHERE guild_id = ?")
            .bind(guild_id)
            .fetch_all(&ctx.data().db)
            .await?;

    let body = if rows.is_empty() {
        "Nothing is locked.".to_string()
    } else {
        rows.iter()
            .map(|(id, at)| format!("<#{id}> — since `{at}` UTC"))
            .collect::<Vec<_>>()
            .join("\n")
    };

    ctx.send(
        poise::CreateReply::default().embed(
            embed::titled("Lockdown status", if rows.is_empty() { embed::SUCCESS } else { embed::DANGER })
                .description(crate::util::text::truncate(&body, 3800)),
        ),
    )
    .await?;
    Ok(())
}

/// Used by the anti-raid engine to lock the server without a command invocation.
pub async fn auto_lockdown(
    sctx: &serenity::Context,
    db: &Db,
    guild_id: GuildId,
    reason: &str,
) -> usize {
    let channels = text_channels(sctx, guild_id);
    let mut locked = 0usize;
    for channel in channels {
        if lock_channel(sctx, db, guild_id, channel, reason).await.is_ok() {
            locked += 1;
        }
    }
    let _ = crate::db::set_i64(db, "guild_config", "locked_down", guild_id.get() as i64, 1).await;
    locked
}

pub fn commands() -> Vec<poise::Command<Data, Error>> {
    vec![lock(), unlock(), lockdown()]
}
