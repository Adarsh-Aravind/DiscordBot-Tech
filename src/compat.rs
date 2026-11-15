//! Thin wrappers around the serenity calls whose signatures move between minor
//! versions (bulk delete, timeouts, bans). Keeping them here means a toolchain
//! bump only ever needs edits in this one file.

use chrono::{DateTime, Utc};
use poise::serenity_prelude as serenity;
use serenity::{
    ChannelId, EditMember, GuildId, Http, MessageId, PermissionOverwrite,
    PermissionOverwriteType, RoleId, TargetId, Timestamp, UserId,
};

pub type Result<T> = serenity::Result<T>;

/// Bulk delete 2-100 messages. Falls back to single deletes for the leftovers,
/// and for messages older than 14 days which Discord refuses to bulk delete.
pub async fn bulk_delete(
    http: &Http,
    channel: ChannelId,
    ids: &[MessageId],
) -> Result<usize> {
    let mut deleted = 0usize;
    for chunk in ids.chunks(100) {
        if chunk.len() == 1 {
            if delete_message(http, channel, chunk[0], None).await.is_ok() {
                deleted += 1;
            }
            continue;
        }
        match channel.delete_messages(http, chunk.to_vec()).await {
            Ok(()) => deleted += chunk.len(),
            Err(_) => {
                // Most likely a message older than two weeks; delete one by one.
                for id in chunk {
                    if delete_message(http, channel, *id, None).await.is_ok() {
                        deleted += 1;
                    }
                }
            }
        }
    }
    Ok(deleted)
}

/// Apply a Discord timeout (communication disabled) until `until`.
pub async fn timeout_until(
    http: &Http,
    guild: GuildId,
    user: UserId,
    until: DateTime<Utc>,
    reason: &str,
) -> Result<()> {
    let ts = Timestamp::from_unix_timestamp(until.timestamp())
        .unwrap_or_else(|_| Timestamp::now());
    guild
        .edit_member(
            http,
            user,
            EditMember::new()
                .disable_communication_until_datetime(ts)
                .audit_log_reason(reason),
        )
        .await
        .map(|_| ())
}

/// Lift a timeout early.
pub async fn clear_timeout(
    http: &Http,
    guild: GuildId,
    user: UserId,
    reason: &str,
) -> Result<()> {
    guild
        .edit_member(
            http,
            user,
            EditMember::new()
                .enable_communication()
                .audit_log_reason(reason),
        )
        .await
        .map(|_| ())
}

pub async fn ban(
    http: &Http,
    guild: GuildId,
    user: UserId,
    delete_message_days: u8,
    reason: &str,
) -> Result<()> {
    guild
        .ban_with_reason(http, user, delete_message_days.min(7), reason)
        .await
}

pub async fn unban(http: &Http, guild: GuildId, user: UserId) -> Result<()> {
    guild.unban(http, user).await
}

pub async fn kick(http: &Http, guild: GuildId, user: UserId, reason: &str) -> Result<()> {
    guild.kick_with_reason(http, user, reason).await
}

/// Delete a channel, going through the raw HTTP method so the audit-log reason
/// is accepted on every 0.12.x point release.
pub async fn delete_channel(http: &Http, channel: ChannelId, reason: &str) -> Result<()> {
    http.delete_channel(channel, Some(reason)).await.map(|_| ())
}

/// Delete one message. `ChannelId::delete_message` dropped its audit-log reason
/// argument in 0.12.5; the raw HTTP method still accepts one.
pub async fn delete_message(
    http: &Http,
    channel: ChannelId,
    message: MessageId,
    reason: Option<&str>,
) -> Result<()> {
    http.delete_message(channel, message, reason).await
}

/// Create or replace a channel permission overwrite, with an audit-log reason.
pub async fn set_permission(
    http: &Http,
    channel: ChannelId,
    overwrite: PermissionOverwrite,
    reason: Option<&str>,
) -> Result<()> {
    let (kind, id) = overwrite_target(overwrite.kind)?;
    // Same body serenity builds internally (its overwrite struct is crate-private).
    let body = serde_json::json!({
        "allow": overwrite.allow.bits().to_string(),
        "deny": overwrite.deny.bits().to_string(),
        "id": id.get().to_string(),
        "type": kind,
    });
    http.create_permission(channel, id, &body, reason).await
}

/// Remove a channel permission overwrite, with an audit-log reason.
pub async fn delete_permission(
    http: &Http,
    channel: ChannelId,
    kind: PermissionOverwriteType,
    reason: Option<&str>,
) -> Result<()> {
    let (_, id) = overwrite_target(kind)?;
    http.delete_permission(channel, id, reason).await
}

/// Discord's overwrite `type` (0 = role, 1 = member) and the target ID.
fn overwrite_target(kind: PermissionOverwriteType) -> Result<(u8, TargetId)> {
    match kind {
        PermissionOverwriteType::Role(id) => Ok((0, TargetId::new(id.get()))),
        PermissionOverwriteType::Member(id) => Ok((1, TargetId::new(id.get()))),
        // The enum is non-exhaustive in serenity.
        _ => Err(serenity::Error::Other("unsupported permission overwrite type")),
    }
}

pub async fn add_role(http: &Http, guild: GuildId, user: UserId, role: RoleId) -> Result<()> {
    http.add_member_role(guild, user, role, None).await
}

pub async fn remove_role(http: &Http, guild: GuildId, user: UserId, role: RoleId) -> Result<()> {
    http.remove_member_role(guild, user, role, None).await
}

// `set_channel_locked` used to live here. Nothing called it, and its unlock
// path wrote an empty overwrite rather than restoring the previous one, which
// would have wiped whatever permissions staff had set on the channel.
// `moderation::lockdown` does this properly: it saves the old overwrite first.
