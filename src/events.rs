//! The single gateway event handler. It dispatches to the modules and keeps
//! the bookkeeping (message log, analytics, snipes) in one place.

use chrono::Utc;
use poise::serenity_prelude as serenity;
use serenity::{FullEvent, GuildId};

use crate::logs::{self, Log};
use crate::modules::{analytics, automation, automod, community, leveling, tickets, voice, welcome};
use crate::state::{Data, Error, Snipe};
use crate::util::{embed, text, time};

pub async fn handle(
    sctx: &serenity::Context,
    event: &FullEvent,
    _framework: poise::FrameworkContext<'_, Data, Error>,
    data: &Data,
) -> Result<(), Error> {
    match event {
        // ===================================================================
        FullEvent::Ready { data_about_bot } => {
            tracing::info!(
                user = %data_about_bot.user.name,
                guilds = data_about_bot.guilds.len(),
                "connected to Discord"
            );
            sctx.set_activity(Some(serenity::ActivityData::watching(
                "MrTechTrivia · /help",
            )));
        }

        FullEvent::CacheReady { guilds } => {
            tracing::info!(guilds = guilds.len(), "cache ready");
            for guild_id in guilds {
                let _ = crate::db::ensure_guild(&data.db, guild_id.get() as i64).await;
                welcome::invites::refresh_cache(sctx, data, *guild_id).await;
            }
            voice::cleanup_orphans(sctx, &data.db).await;
        }

        FullEvent::GuildCreate { guild, is_new } => {
            let _ = crate::db::ensure_guild(&data.db, guild.id.get() as i64).await;
            welcome::invites::refresh_cache(sctx, data, guild.id).await;
            if is_new.unwrap_or(false) {
                tracing::info!(guild = %guild.name, "joined a new server");
            }
        }

        // ===================================================================
        // Messages
        // ===================================================================
        FullEvent::Message { new_message } => {
            if new_message.author.bot || new_message.guild_id.is_none() {
                return Ok(());
            }
            on_message(sctx, data, new_message).await?;
        }

        FullEvent::MessageUpdate { old_if_available, new, event } => {
            on_message_update(sctx, data, old_if_available.as_ref(), new.as_ref(), event).await?;
        }

        FullEvent::MessageDelete {
            channel_id,
            deleted_message_id,
            guild_id,
        } => {
            on_message_delete(sctx, data, *channel_id, *deleted_message_id, *guild_id).await?;
        }

        FullEvent::MessageDeleteBulk {
            channel_id,
            multiple_deleted_messages_ids,
            guild_id,
        } => {
            if let Some(guild_id) = guild_id {
                logs::send(
                    sctx,
                    &data.db,
                    guild_id.get() as i64,
                    Log::Message,
                    embed::titled("🧹 Bulk deletion", embed::WARN).description(format!(
                        "**{}** messages were removed from <#{}>.",
                        multiple_deleted_messages_ids.len(),
                        channel_id.get()
                    )),
                )
                .await;
            }
        }

        // ===================================================================
        // Members
        // ===================================================================
        FullEvent::GuildMemberAddition { new_member } => {
            welcome::handle_join(sctx, data, new_member).await?;
            automation::fire(
                sctx,
                data,
                new_member.guild_id,
                "member_join",
                automation::WorkflowContext {
                    user_id: new_member.user.id.get(),
                    ..Default::default()
                },
            )
            .await;
            community::misc::check_member_milestone(sctx, data, new_member.guild_id).await;
            automation::notify_webhooks(
                data,
                new_member.guild_id.get() as i64,
                "join",
                serde_json::json!({
                    "user_id": new_member.user.id.get().to_string(),
                    "tag": new_member.user.tag(),
                }),
            )
            .await;
        }

        FullEvent::GuildMemberRemoval { guild_id, user, .. } => {
            welcome::handle_leave(sctx, data, *guild_id, user).await?;
            automation::fire(
                sctx,
                data,
                *guild_id,
                "member_leave",
                automation::WorkflowContext {
                    user_id: user.id.get(),
                    ..Default::default()
                },
            )
            .await;
            automation::notify_webhooks(
                data,
                guild_id.get() as i64,
                "leave",
                serde_json::json!({
                    "user_id": user.id.get().to_string(),
                    "tag": user.tag(),
                }),
            )
            .await;
        }

        FullEvent::GuildMemberUpdate {
            old_if_available,
            new,
            ..
        } => {
            if let (Some(old), Some(new)) = (old_if_available, new) {
                on_member_update(sctx, data, old, new).await;
            }
        }

        FullEvent::GuildBanAddition { guild_id, banned_user } => {
            logs::send(
                sctx,
                &data.db,
                guild_id.get() as i64,
                Log::Mod,
                embed::titled("🔨 Member banned", embed::DANGER).description(format!(
                    "<@{}> `{}`\nIf this was not done through me, there is no case for it.",
                    banned_user.id.get(),
                    banned_user.tag()
                )),
            )
            .await;
        }

        FullEvent::GuildBanRemoval { guild_id, unbanned_user } => {
            logs::send(
                sctx,
                &data.db,
                guild_id.get() as i64,
                Log::Mod,
                embed::titled("♻️ Ban lifted", embed::SUCCESS).description(format!(
                    "<@{}> `{}`",
                    unbanned_user.id.get(),
                    unbanned_user.tag()
                )),
            )
            .await;
        }

        // ===================================================================
        // Voice
        // ===================================================================
        FullEvent::VoiceStateUpdate { old, new } => {
            voice::handle_voice_state(sctx, data, old.as_ref(), new).await?;
        }

        // ===================================================================
        // Reactions (starboard)
        // ===================================================================
        FullEvent::ReactionAdd { add_reaction } => {
            community::starboard::handle_reaction(sctx, data, add_reaction).await?;
        }

        FullEvent::ReactionRemove { removed_reaction } => {
            community::starboard::handle_reaction(sctx, data, removed_reaction).await?;
        }

        // ===================================================================
        // Interactions
        // ===================================================================
        FullEvent::InteractionCreate { interaction } => {
            if let serenity::Interaction::Component(component) = interaction {
                on_component(sctx, data, component).await;
            }
        }

        // ===================================================================
        // Server changes
        // ===================================================================
        FullEvent::GuildAuditLogEntryCreate { entry, guild_id } => {
            let label = format!("{:?}", entry.action);
            let target = entry.target_id.map(|id| format!("`{}`", id.get()));
            if let Some(actor) = entry.user_id {
                automod::nuke::handle_audit_entry(sctx, data, *guild_id, actor, &label, target)
                    .await;
            }
        }

        FullEvent::ChannelDelete { channel, .. } => {
            logs::send(
                sctx,
                &data.db,
                channel.guild_id.get() as i64,
                Log::Server,
                embed::titled("🗑️ Channel deleted", embed::DANGER)
                    .description(format!("`#{}` (`{}`)", channel.name, channel.id.get())),
            )
            .await;

            // A deleted ticket channel should not leave a dangling open ticket.
            let _ = sqlx::query(
                "UPDATE tickets SET status = 'closed', closed_at = ?, close_reason = ?
                 WHERE channel_id = ? AND status = 'open'",
            )
            .bind(time::sql_now())
            .bind("Channel was deleted")
            .bind(channel.id.get() as i64)
            .execute(&data.db)
            .await;

            let _ = sqlx::query("DELETE FROM temp_voice WHERE channel_id = ?")
                .bind(channel.id.get() as i64)
                .execute(&data.db)
                .await;
        }

        FullEvent::ChannelCreate { channel } => {
            logs::send(
                sctx,
                &data.db,
                channel.guild_id.get() as i64,
                Log::Server,
                embed::titled("➕ Channel created", embed::SUCCESS)
                    .description(format!("<#{}>", channel.id.get())),
            )
            .await;
        }

        FullEvent::GuildRoleCreate { new } => {
            logs::send(
                sctx,
                &data.db,
                new.guild_id.get() as i64,
                Log::Server,
                embed::titled("➕ Role created", embed::SUCCESS)
                    .description(format!("<@&{}> `{}`", new.id.get(), new.name)),
            )
            .await;
        }

        FullEvent::GuildRoleDelete {
            guild_id,
            removed_role_id,
            removed_role_data_if_available,
        } => {
            let name = removed_role_data_if_available
                .as_ref()
                .map(|r| r.name.clone())
                .unwrap_or_else(|| "unknown".to_string());
            logs::send(
                sctx,
                &data.db,
                guild_id.get() as i64,
                Log::Server,
                embed::titled("🗑️ Role deleted", embed::DANGER)
                    .description(format!("`{name}` (`{}`)", removed_role_id.get())),
            )
            .await;
        }

        // ===================================================================
        // Invites
        // ===================================================================
        FullEvent::InviteCreate { data: invite } => {
            if let Some(guild_id) = invite.guild_id {
                welcome::invites::refresh_cache(sctx, data, guild_id).await;
            }
        }

        FullEvent::InviteDelete { data: invite } => {
            if let Some(guild_id) = invite.guild_id {
                welcome::invites::refresh_cache(sctx, data, guild_id).await;
            }
        }

        _ => {}
    }

    Ok(())
}

// ===========================================================================
// Message pipeline
// ===========================================================================

async fn on_message(
    sctx: &serenity::Context,
    data: &Data,
    message: &serenity::Message,
) -> Result<(), Error> {
    let Some(guild_id) = message.guild_id else {
        return Ok(());
    };
    let guild_i64 = guild_id.get() as i64;

    // Keep a copy so edit and delete logs still work after a restart.
    let _ = sqlx::query(
        "INSERT OR REPLACE INTO message_log
            (message_id, guild_id, channel_id, author_id, author_tag, content, attachments, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(message.id.get() as i64)
    .bind(guild_i64)
    .bind(message.channel_id.get() as i64)
    .bind(message.author.id.get() as i64)
    .bind(message.author.tag())
    .bind(text::truncate(&message.content, 1800))
    .bind(
        message
            .attachments
            .iter()
            .map(|a| a.url.clone())
            .collect::<Vec<_>>()
            .join(" "),
    )
    .bind(time::sql_now())
    .execute(&data.db)
    .await;

    // Automod gets first say: it may delete the message.
    automod::scan::handle_message(sctx, data, message).await?;

    // Statistics and XP.
    analytics::record_message(data, message).await;
    leveling::handle_message(sctx, data, message).await?;

    // AFK bookkeeping and mention replies.
    crate::modules::utility::handle_afk(sctx, data, message).await?;

    // Channel games.
    if community::handle_message(sctx, data, message).await? {
        return Ok(());
    }

    // Ticket transcripts.
    if tickets::ticket_by_channel(&data.db, message.channel_id.get() as i64)
        .await
        .is_some()
    {
        let is_staff = is_support_staff(data, guild_i64, message).await;
        tickets::record_message(data, message, is_staff).await;
    }

    // Custom text commands.
    automation::try_custom_command(sctx, data, message).await?;

    Ok(())
}

/// Does this message's author count as support staff?
async fn is_support_staff(data: &Data, guild_id: i64, message: &serenity::Message) -> bool {
    let Ok(cfg) = crate::db::ticket_config(&data.db, guild_id).await else {
        return false;
    };
    let support = crate::db::csv_ids(&cfg.support_roles);

    if let Some(member) = &message.member {
        if member
            .roles
            .iter()
            .any(|r| support.contains(&(r.get() as i64)))
        {
            return true;
        }
    }
    if let Ok(guild) = crate::db::guild_config(&data.db, guild_id).await {
        let staff: Vec<i64> = crate::db::csv_ids(&guild.mod_roles)
            .into_iter()
            .chain(crate::db::csv_ids(&guild.admin_roles))
            .chain(crate::db::csv_ids(&guild.staff_roles))
            .collect();
        if let Some(member) = &message.member {
            if member.roles.iter().any(|r| staff.contains(&(r.get() as i64))) {
                return true;
            }
        }
    }
    false
}

async fn on_message_update(
    sctx: &serenity::Context,
    data: &Data,
    _old: Option<&serenity::Message>,
    new: Option<&serenity::Message>,
    event: &serenity::MessageUpdateEvent,
) -> Result<(), Error> {
    let Some(guild_id) = event.guild_id else {
        return Ok(());
    };
    let guild_i64 = guild_id.get() as i64;
    let message_id = event.id.get() as i64;

    // Our own copy is the most reliable "before".
    let previous: Option<(String, i64, String)> = sqlx::query_as(
        "SELECT content, author_id, author_tag FROM message_log WHERE message_id = ?",
    )
    .bind(message_id)
    .fetch_optional(&data.db)
    .await?;

    let after = new
        .map(|m| m.content.clone())
        .or_else(|| event.content.clone())
        .unwrap_or_default();

    let (before, author_id, author_tag) = match previous {
        Some((content, author_id, author_tag)) => (content, author_id, author_tag),
        None => (
            String::new(),
            new.map(|m| m.author.id.get() as i64).unwrap_or(0),
            new.map(|m| m.author.tag()).unwrap_or_default(),
        ),
    };

    // Bots and no-op edits (embed loading, for example) are not worth logging.
    if author_id == 0 || before == after || after.trim().is_empty() {
        return Ok(());
    }
    if new.map(|m| m.author.bot).unwrap_or(false) {
        return Ok(());
    }

    data.edit_snipes.insert(
        event.channel_id.get(),
        Snipe {
            author_id: author_id as u64,
            author_tag: author_tag.clone(),
            content: after.clone(),
            before: Some(before.clone()),
            attachments: Vec::new(),
            at: Utc::now(),
        },
    );

    let _ = sqlx::query("UPDATE message_log SET content = ? WHERE message_id = ?")
        .bind(text::truncate(&after, 1800))
        .bind(message_id)
        .execute(&data.db)
        .await;

    logs::send(
        sctx,
        &data.db,
        guild_i64,
        Log::Message,
        embed::titled("✏️ Message edited", embed::WARN)
            .description(format!(
                "<@{author_id}> in <#{}> · [jump](https://discord.com/channels/{}/{}/{})",
                event.channel_id.get(),
                guild_id.get(),
                event.channel_id.get(),
                event.id.get()
            ))
            .field(
                "Before",
                if before.is_empty() {
                    "*(not recorded)*".to_string()
                } else {
                    text::truncate(&before, 1000)
                },
                false,
            )
            .field("After", text::truncate(&after, 1000), false)
            .footer(embed::footer(format!("Author ID: {author_id}"))),
    )
    .await;

    Ok(())
}

async fn on_message_delete(
    sctx: &serenity::Context,
    data: &Data,
    channel_id: serenity::ChannelId,
    message_id: serenity::MessageId,
    guild_id: Option<GuildId>,
) -> Result<(), Error> {
    let Some(guild_id) = guild_id else {
        return Ok(());
    };
    let guild_i64 = guild_id.get() as i64;

    let found: Option<(String, i64, String, String)> = sqlx::query_as(
        "SELECT content, author_id, author_tag, attachments FROM message_log
         WHERE message_id = ?",
    )
    .bind(message_id.get() as i64)
    .fetch_optional(&data.db)
    .await?;

    let Some((content, author_id, author_tag, attachments)) = found else {
        return Ok(()); // never saw it, nothing useful to say
    };

    let attachment_list: Vec<String> = attachments
        .split_whitespace()
        .map(|s| s.to_string())
        .collect();

    data.snipes.insert(
        channel_id.get(),
        Snipe {
            author_id: author_id as u64,
            author_tag: author_tag.clone(),
            content: content.clone(),
            before: None,
            attachments: attachment_list.clone(),
            at: Utc::now(),
        },
    );

    let mut log_embed = embed::titled("🗑️ Message deleted", embed::DANGER)
        .description(format!(
            "<@{author_id}> `{author_tag}` in <#{}>",
            channel_id.get()
        ))
        .field(
            "Content",
            if content.trim().is_empty() {
                "*(no text)*".to_string()
            } else {
                text::truncate(&content, 1500)
            },
            false,
        )
        .footer(embed::footer(format!("Author ID: {author_id}")));

    if !attachment_list.is_empty() {
        log_embed = log_embed.field(
            "Attachments",
            text::truncate(&attachment_list.join("\n"), 900),
            false,
        );
    }

    logs::send(sctx, &data.db, guild_i64, Log::Message, log_embed).await;

    let _ = sqlx::query("DELETE FROM message_log WHERE message_id = ?")
        .bind(message_id.get() as i64)
        .execute(&data.db)
        .await;

    Ok(())
}

async fn on_member_update(
    sctx: &serenity::Context,
    data: &Data,
    old: &serenity::Member,
    new: &serenity::Member,
) {
    let guild_i64 = new.guild_id.get() as i64;

    // Nickname changes.
    if old.nick != new.nick {
        logs::send(
            sctx,
            &data.db,
            guild_i64,
            Log::Join,
            embed::titled("📝 Nickname changed", embed::INFO)
                .description(format!("<@{}>", new.user.id.get()))
                .field(
                    "Before",
                    old.nick.clone().unwrap_or_else(|| "none".into()),
                    true,
                )
                .field(
                    "After",
                    new.nick.clone().unwrap_or_else(|| "none".into()),
                    true,
                ),
        )
        .await;
    }

    // Role changes.
    let added: Vec<String> = new
        .roles
        .iter()
        .filter(|r| !old.roles.contains(r))
        .map(|r| format!("<@&{}>", r.get()))
        .collect();
    let removed: Vec<String> = old
        .roles
        .iter()
        .filter(|r| !new.roles.contains(r))
        .map(|r| format!("<@&{}>", r.get()))
        .collect();

    if !added.is_empty() || !removed.is_empty() {
        let mut body = String::new();
        if !added.is_empty() {
            body.push_str(&format!("**Added:** {}\n", added.join(", ")));
        }
        if !removed.is_empty() {
            body.push_str(&format!("**Removed:** {}", removed.join(", ")));
        }
        logs::send(
            sctx,
            &data.db,
            guild_i64,
            Log::Join,
            embed::titled("🎭 Roles changed", embed::INFO)
                .description(format!("<@{}>\n{body}", new.user.id.get())),
        )
        .await;
    }
}

// ===========================================================================
// Components
// ===========================================================================

async fn on_component(
    sctx: &serenity::Context,
    data: &Data,
    component: &serenity::ComponentInteraction,
) {
    // Each module reports whether the custom_id was theirs.
    let result = async {
        if welcome::roles::handle_component(sctx, data, component).await? {
            return Ok::<bool, Error>(true);
        }
        if tickets::handle_component(sctx, data, component).await? {
            return Ok(true);
        }
        if community::handle_component(sctx, data, component).await? {
            return Ok(true);
        }
        Ok(false)
    }
    .await;

    match result {
        Ok(true) => {}
        Ok(false) => {
            tracing::debug!(
                custom_id = %component.data.custom_id,
                "component interaction nobody claimed"
            );
        }
        Err(why) => {
            tracing::warn!(
                custom_id = %component.data.custom_id,
                "component handler failed: {why}"
            );
            // Always answer the interaction, or the user sees a failure spinner.
            let _ = component
                .create_response(
                    &sctx.http,
                    serenity::CreateInteractionResponse::Message(
                        serenity::CreateInteractionResponseMessage::new()
                            .content("Something went wrong handling that. Please try again.")
                            .ephemeral(true),
                    ),
                )
                .await;
        }
    }
}
