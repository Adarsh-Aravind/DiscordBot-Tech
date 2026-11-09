//! Thin wrappers around the serenity calls whose signatures move between minor
//! versions (bulk delete, timeouts, bans). Keeping them here means a toolchain
//! bump only ever needs edits in this one file.

use chrono::{DateTime, Utc};
use poise::serenity_prelude as serenity;
use serenity::{
    ChannelId, EditMember, GuildId, Http, MessageId, PermissionOverwrite, PermissionOverwriteType,
    Permissions, RoleId, Timestamp, UserId,
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
            if channel.delete_message(http, chunk[0], None).await.is_ok() {
                deleted += 1;
            }
            continue;
        }
        match channel.delete_messages(http, chunk.to_vec()).await {
            Ok(()) => deleted += chunk.len(),
            Err(_) => {
                // Most likely a message older than two weeks; delete one by one.
                for id in chunk {
                    if channel.delete_message(http, *id, None).await.is_ok() {
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

pub async fn add_role(http: &Http, guild: GuildId, user: UserId, role: RoleId) -> Result<()> {
    http.add_member_role(guild, user, role, None).await
}

pub async fn remove_role(http: &Http, guild: GuildId, user: UserId, role: RoleId) -> Result<()> {
    http.remove_member_role(guild, user, role, None).await
}

/// Deny or restore `Send Messages` for `@everyone` in a channel.
pub async fn set_channel_locked(
    http: &Http,
    channel: ChannelId,
    everyone: RoleId,
    locked: bool,
) -> Result<()> {
    let target = PermissionOverwriteType::Role(everyone);
    if locked {
        channel
            .create_permission(
                http,
                PermissionOverwrite {
                    allow: Permissions::empty(),
                    deny: Permissions::SEND_MESSAGES
                        | Permissions::SEND_MESSAGES_IN_THREADS
                        | Permissions::CREATE_PUBLIC_THREADS
                        | Permissions::ADD_REACTIONS,
                    kind: target,
                },
                None,
            )
            .await
    } else {
        channel
            .create_permission(
                http,
                PermissionOverwrite {
                    allow: Permissions::empty(),
                    deny: Permissions::empty(),
                    kind: target,
                },
                None,
            )
            .await
    }
}
