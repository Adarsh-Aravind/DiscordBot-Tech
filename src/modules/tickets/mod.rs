//! Ticket support system: panels, private channels, claiming, transcripts,
//! ratings and auto-close.

pub mod commands;

use chrono::Utc;
use poise::serenity_prelude as serenity;
use serenity::{
    ChannelId, ChannelType, ComponentInteraction, CreateActionRow, CreateAttachment, CreateButton,
    CreateChannel, CreateInteractionResponse, CreateInteractionResponseMessage, CreateMessage,
    GuildId, Message, PermissionOverwrite, PermissionOverwriteType, Permissions, RoleId, UserId,
};
use sqlx::FromRow;

use crate::db::{self, Db};
use crate::logs::{self, Log};
use crate::state::{Data, Error};
use crate::util::{embed, text, time};

#[derive(Debug, Clone, FromRow)]
pub struct Ticket {
    pub id: i64,
    pub guild_id: i64,
    pub number: i64,
    pub channel_id: i64,
    pub user_id: i64,
    pub category: String,
    pub subject: String,
    pub priority: String,
    pub status: String,
    pub claimed_by: Option<i64>,
    pub assigned_to: Option<i64>,
    pub created_at: String,
    pub first_reply_at: Option<String>,
    pub first_reply_by: Option<i64>,
    pub last_activity: String,
    pub closed_at: Option<String>,
    pub closed_by: Option<i64>,
    pub close_reason: Option<String>,
    pub rating: Option<i64>,
    pub feedback: Option<String>,
}

#[derive(Debug, Clone, FromRow)]
pub struct TicketCategory {
    pub id: i64,
    pub key: String,
    pub label: String,
    pub emoji: String,
    pub description: String,
    pub template: String,
    pub support_roles: String,
    pub category_id: Option<i64>,
}

pub fn priority_emoji(priority: &str) -> &'static str {
    match priority {
        "urgent" => "🔴",
        "high" => "🟠",
        "low" => "🔵",
        _ => "🟢",
    }
}

pub async fn ticket_by_channel(db: &Db, channel_id: i64) -> Option<Ticket> {
    sqlx::query_as("SELECT * FROM tickets WHERE channel_id = ?")
        .bind(channel_id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
}

async fn categories(db: &Db, guild_id: i64) -> Vec<TicketCategory> {
    sqlx::query_as(
        "SELECT id, key, label, emoji, description, template, support_roles, category_id
         FROM ticket_categories WHERE guild_id = ? ORDER BY id",
    )
    .bind(guild_id)
    .fetch_all(db)
    .await
    .unwrap_or_default()
}

/// Everyone who should be able to see a ticket of this category.
async fn support_roles(db: &Db, guild_id: i64, category: Option<&TicketCategory>) -> Vec<i64> {
    let cfg = match db::ticket_config(db, guild_id).await {
        Ok(cfg) => cfg,
        Err(_) => return Vec::new(),
    };
    let mut roles = db::csv_ids(&cfg.support_roles);
    if let Some(category) = category {
        for role in db::csv_ids(&category.support_roles) {
            if !roles.contains(&role) {
                roles.push(role);
            }
        }
    }
    roles
}

// ===========================================================================
// Opening
// ===========================================================================

pub struct OpenResult {
    pub channel_id: ChannelId,
    pub number: i64,
}

pub async fn open_ticket(
    sctx: &serenity::Context,
    data: &Data,
    guild_id: GuildId,
    opener: UserId,
    opener_tag: &str,
    category_key: &str,
    subject: Option<String>,
) -> Result<OpenResult, String> {
    let guild_i64 = guild_id.get() as i64;
    let cfg = db::ticket_config(&data.db, guild_i64)
        .await
        .map_err(|e| e.to_string())?;

    // One member should not be able to flood the queue.
    let open_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM tickets WHERE guild_id = ? AND user_id = ? AND status = 'open'",
    )
    .bind(guild_i64)
    .bind(opener.get() as i64)
    .fetch_one(&data.db)
    .await
    .unwrap_or(0);

    if open_count >= cfg.max_open {
        return Err(format!(
            "You already have **{open_count}** open ticket(s), which is the limit here. Close one first."
        ));
    }

    let all = categories(&data.db, guild_i64).await;
    let category = all.iter().find(|c| c.key == category_key);
    let roles = support_roles(&data.db, guild_i64, category).await;

    let number = cfg.next_number;
    let channel_name = format!("ticket-{number:04}");

    // Private by default: nobody but the opener, support and the bot.
    let mut overwrites = vec![
        PermissionOverwrite {
            allow: Permissions::empty(),
            deny: Permissions::VIEW_CHANNEL,
            kind: PermissionOverwriteType::Role(RoleId::new(guild_id.get())),
        },
        PermissionOverwrite {
            allow: Permissions::VIEW_CHANNEL
                | Permissions::SEND_MESSAGES
                | Permissions::READ_MESSAGE_HISTORY
                | Permissions::ATTACH_FILES
                | Permissions::EMBED_LINKS,
            deny: Permissions::empty(),
            kind: PermissionOverwriteType::Member(opener),
        },
        PermissionOverwrite {
            allow: Permissions::VIEW_CHANNEL
                | Permissions::SEND_MESSAGES
                | Permissions::READ_MESSAGE_HISTORY
                | Permissions::MANAGE_MESSAGES
                | Permissions::ATTACH_FILES
                | Permissions::EMBED_LINKS,
            deny: Permissions::empty(),
            kind: PermissionOverwriteType::Member(crate::util::perms::bot_id(sctx)),
        },
    ];
    for role in &roles {
        overwrites.push(PermissionOverwrite {
            allow: Permissions::VIEW_CHANNEL
                | Permissions::SEND_MESSAGES
                | Permissions::READ_MESSAGE_HISTORY
                | Permissions::ATTACH_FILES
                | Permissions::EMBED_LINKS,
            deny: Permissions::empty(),
            kind: PermissionOverwriteType::Role(RoleId::new(*role as u64)),
        });
    }

    let parent = category
        .and_then(|c| c.category_id)
        .or(cfg.category_id)
        .map(|id| ChannelId::new(id as u64));

    let mut builder = CreateChannel::new(channel_name)
        .kind(ChannelType::Text)
        .topic(format!(
            "Ticket #{number} · opened by {opener_tag} · {}",
            category.map(|c| c.label.as_str()).unwrap_or("general")
        ))
        .permissions(overwrites);
    if let Some(parent) = parent {
        builder = builder.category(parent);
    }

    let channel = guild_id
        .create_channel(&sctx.http, builder)
        .await
        .map_err(|e| {
            format!("I could not create the ticket channel: {e}. Check that I have **Manage Channels**.")
        })?;

    // Persist before posting, so a failure to post still leaves a usable ticket.
    let ticket_id = sqlx::query(
        "INSERT INTO tickets
            (guild_id, number, channel_id, user_id, category, subject, created_at, last_activity)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(guild_i64)
    .bind(number)
    .bind(channel.id.get() as i64)
    .bind(opener.get() as i64)
    .bind(category.map(|c| c.key.as_str()).unwrap_or("general"))
    .bind(subject.clone().unwrap_or_default())
    .bind(time::sql_now())
    .bind(time::sql_now())
    .execute(&data.db)
    .await
    .map_err(|e| e.to_string())?
    .last_insert_rowid();

    let _ = db::set_i64(
        &data.db,
        "ticket_config",
        "next_number",
        guild_i64,
        number + 1,
    )
    .await;

    // ---- intro message ---------------------------------------------------
    let mut intro = embed::titled(
        format!(
            "{} Ticket #{number}",
            category.map(|c| c.emoji.as_str()).unwrap_or("🎫")
        ),
        embed::BRAND,
    )
    .field("Opened by", format!("<@{}>", opener.get()), true)
    .field(
        "Category",
        category.map(|c| c.label.clone()).unwrap_or_else(|| "General".into()),
        true,
    )
    .field("Priority", format!("{} normal", priority_emoji("normal")), true)
    .footer(embed::footer(
        "Staff: use the buttons below, or /ticket for the full set of controls.",
    ))
    .timestamp(serenity::Timestamp::now());

    if let Some(subject) = &subject {
        if !subject.trim().is_empty() {
            intro = intro.description(format!("**{}**", text::truncate(subject, 250)));
        }
    }
    if let Some(category) = category {
        if !category.template.trim().is_empty() {
            intro = intro.field(
                "Please include",
                text::truncate(&category.template.replace("\\n", "\n"), 1000),
                false,
            );
        }
    }

    let ping = if roles.is_empty() {
        format!("<@{}>", opener.get())
    } else {
        format!(
            "<@{}> {}",
            opener.get(),
            roles
                .iter()
                .map(|r| format!("<@&{r}>"))
                .collect::<Vec<_>>()
                .join(" ")
        )
    };

    let _ = channel
        .send_message(
            &sctx.http,
            CreateMessage::new()
                .content(ping)
                .embed(intro)
                .components(vec![CreateActionRow::Buttons(vec![
                    CreateButton::new("ticket:claim")
                        .label("Claim")
                        .style(serenity::ButtonStyle::Primary)
                        .emoji('🙋'),
                    CreateButton::new("ticket:close")
                        .label("Close")
                        .style(serenity::ButtonStyle::Danger)
                        .emoji('🔒'),
                ])]),
        )
        .await;

    // ---- log -------------------------------------------------------------
    let log_embed = embed::titled(format!("🎫 Ticket #{number} opened"), embed::INFO)
        .field("Member", format!("<@{}>", opener.get()), true)
        .field("Channel", format!("<#{}>", channel.id.get()), true)
        .field(
            "Category",
            category.map(|c| c.label.clone()).unwrap_or_else(|| "General".into()),
            true,
        );

    if let Some(log_channel) = cfg.log_channel {
        let _ = ChannelId::new(log_channel as u64)
            .send_message(&sctx.http, CreateMessage::new().embed(log_embed))
            .await;
    } else {
        logs::send(sctx, &data.db, guild_i64, Log::Mod, log_embed).await;
    }

    let _ = ticket_id;
    Ok(OpenResult {
        channel_id: channel.id,
        number,
    })
}

// ===========================================================================
// Closing
// ===========================================================================

pub async fn close_ticket(
    sctx: &serenity::Context,
    data: &Data,
    ticket: &Ticket,
    closed_by: UserId,
    reason: Option<String>,
) -> Result<(), Error> {
    let guild_i64 = ticket.guild_id;
    let cfg = db::ticket_config(&data.db, guild_i64).await?;

    sqlx::query(
        "UPDATE tickets SET status = 'closed', closed_at = ?, closed_by = ?, close_reason = ?
         WHERE id = ?",
    )
    .bind(time::sql_now())
    .bind(closed_by.get() as i64)
    .bind(&reason)
    .bind(ticket.id)
    .execute(&data.db)
    .await?;

    let transcript = build_transcript(data, ticket).await;
    let channel = ChannelId::new(ticket.channel_id as u64);

    let open_duration = time::from_sql(&ticket.created_at)
        .map(|t| time::humanize((Utc::now() - t).num_seconds()))
        .unwrap_or_else(|| "unknown".to_string());

    let response_time = match ticket.first_reply_at.as_deref().and_then(time::from_sql) {
        Some(first) => time::from_sql(&ticket.created_at)
            .map(|created| time::humanize((first - created).num_seconds()))
            .unwrap_or_else(|| "unknown".to_string()),
        None => "no staff reply".to_string(),
    };

    let summary = embed::titled(format!("🔒 Ticket #{} closed", ticket.number), embed::MUTED)
        .field("Opened by", format!("<@{}>", ticket.user_id), true)
        .field("Closed by", format!("<@{}>", closed_by.get()), true)
        .field(
            "Claimed by",
            ticket
                .claimed_by
                .map(|id| format!("<@{id}>"))
                .unwrap_or_else(|| "nobody".to_string()),
            true,
        )
        .field("Open for", open_duration, true)
        .field("First response", response_time, true)
        .field("Priority", ticket.priority.clone(), true)
        .field(
            "Reason",
            reason.clone().unwrap_or_else(|| "No reason given".to_string()),
            false,
        );

    // Transcript goes wherever it is configured, then the log channel.
    let transcript_target = cfg
        .transcript_channel
        .or(cfg.log_channel)
        .map(|id| ChannelId::new(id as u64));

    if let Some(target) = transcript_target {
        let attachment = CreateAttachment::bytes(
            transcript.clone().into_bytes(),
            format!("ticket-{:04}.txt", ticket.number),
        );
        let _ = target
            .send_message(
                &sctx.http,
                CreateMessage::new()
                    .embed(summary.clone())
                    .add_file(attachment),
            )
            .await;
    } else {
        logs::send(sctx, &data.db, guild_i64, Log::Mod, summary.clone()).await;
    }

    // DM the opener their transcript and ask for a rating.
    let opener = UserId::new(ticket.user_id as u64);
    if let Ok(dm) = opener.create_dm_channel(sctx).await {
        let mut message = CreateMessage::new()
            .embed(
                embed::titled(
                    format!("Your ticket #{} was closed", ticket.number),
                    embed::MUTED,
                )
                .description(format!(
                    "Server: **{}**\nReason: {}",
                    crate::modules::moderation::guild_name(sctx, GuildId::new(guild_i64 as u64)),
                    reason.unwrap_or_else(|| "No reason given".to_string())
                )),
            )
            .add_file(CreateAttachment::bytes(
                transcript.into_bytes(),
                format!("ticket-{:04}.txt", ticket.number),
            ));

        if cfg.ask_feedback == 1 {
            message = message.components(vec![CreateActionRow::Buttons(
                (1..=5)
                    .map(|n| {
                        CreateButton::new(format!("ticket:rate:{}:{n}", ticket.id))
                            .label(format!("{n}"))
                            .style(if n >= 4 {
                                serenity::ButtonStyle::Success
                            } else if n == 3 {
                                serenity::ButtonStyle::Secondary
                            } else {
                                serenity::ButtonStyle::Danger
                            })
                    })
                    .collect(),
            )]);
        }
        let _ = dm.send_message(&sctx.http, message).await;
    }

    // Give people a moment to read before the channel disappears.
    let _ = channel
        .send_message(
            &sctx.http,
            CreateMessage::new().embed(
                embed::titled("Ticket closed", embed::MUTED)
                    .description("This channel will be deleted in 10 seconds."),
            ),
        )
        .await;

    let http = sctx.http.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
        let _ = crate::compat::delete_channel(&http, channel, "Ticket closed").await;
    });

    Ok(())
}

/// Plain-text transcript from the messages we recorded.
async fn build_transcript(data: &Data, ticket: &Ticket) -> String {
    let rows: Vec<(String, String, i64)> = sqlx::query_as(
        "SELECT author_tag, content, is_staff FROM ticket_messages
         WHERE ticket_id = ? ORDER BY id ASC",
    )
    .bind(ticket.id)
    .fetch_all(&data.db)
    .await
    .unwrap_or_default();

    let mut out = String::new();
    out.push_str(&format!("Ticket #{:04}\n", ticket.number));
    out.push_str(&format!("Category: {}\n", ticket.category));
    out.push_str(&format!("Opened by user id {}\n", ticket.user_id));
    out.push_str(&format!("Opened at: {} UTC\n", ticket.created_at));
    if !ticket.subject.is_empty() {
        out.push_str(&format!("Subject: {}\n", ticket.subject));
    }
    out.push_str(&format!("Priority: {}\n", ticket.priority));
    out.push_str(&"-".repeat(60));
    out.push('\n');

    if rows.is_empty() {
        out.push_str("(no messages were recorded)\n");
    }
    for (author, content, is_staff) in rows {
        let marker = if is_staff == 1 { "[staff]" } else { "      " };
        out.push_str(&format!("{marker} {author}: {content}\n"));
    }

    out.push_str(&"-".repeat(60));
    out.push_str("\nEnd of transcript.\n");
    out
}

/// Record a message in a ticket channel, and stamp the first staff reply.
pub async fn record_message(data: &Data, message: &Message, is_staff: bool) {
    let Some(ticket) = ticket_by_channel(&data.db, message.channel_id.get() as i64).await else {
        return;
    };
    if ticket.status != "open" {
        return;
    }

    let content = if message.attachments.is_empty() {
        message.content.clone()
    } else {
        format!(
            "{} [attachments: {}]",
            message.content,
            message
                .attachments
                .iter()
                .map(|a| a.filename.clone())
                .collect::<Vec<_>>()
                .join(", ")
        )
    };

    let _ = sqlx::query(
        "INSERT INTO ticket_messages (ticket_id, author_id, author_tag, is_staff, content, created_at)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(ticket.id)
    .bind(message.author.id.get() as i64)
    .bind(message.author.tag())
    .bind(i64::from(is_staff))
    .bind(text::truncate(&content, 1800))
    .bind(time::sql_now())
    .execute(&data.db)
    .await;

    let _ = sqlx::query("UPDATE tickets SET last_activity = ? WHERE id = ?")
        .bind(time::sql_now())
        .bind(ticket.id)
        .execute(&data.db)
        .await;

    // Staff response tracking: remember who answered first and when.
    if is_staff
        && ticket.first_reply_at.is_none()
        && message.author.id.get() as i64 != ticket.user_id
    {
        let _ = sqlx::query("UPDATE tickets SET first_reply_at = ?, first_reply_by = ? WHERE id = ?")
            .bind(time::sql_now())
            .bind(message.author.id.get() as i64)
            .bind(ticket.id)
            .execute(&data.db)
            .await;
    }
}

// ===========================================================================
// Components
// ===========================================================================

pub async fn handle_component(
    sctx: &serenity::Context,
    data: &Data,
    interaction: &ComponentInteraction,
) -> Result<bool, Error> {
    let custom_id = interaction.data.custom_id.clone();
    if !custom_id.starts_with("ticket") {
        return Ok(false);
    }

    // ---- rating (works in DMs, so no guild check) ------------------------
    if let Some(rest) = custom_id.strip_prefix("ticket:rate:") {
        let mut parts = rest.split(':');
        let ticket_id: i64 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
        let score: i64 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);

        sqlx::query("UPDATE tickets SET rating = ? WHERE id = ? AND user_id = ?")
            .bind(score)
            .bind(ticket_id)
            .bind(interaction.user.id.get() as i64)
            .execute(&data.db)
            .await?;

        interaction
            .create_response(
                &sctx.http,
                CreateInteractionResponse::Message(
                    CreateInteractionResponseMessage::new()
                        .content(format!(
                            "Thanks! You rated this ticket **{score}/5**. Your feedback helps the staff team."
                        ))
                        .ephemeral(true),
                ),
            )
            .await?;
        return Ok(true);
    }

    let Some(guild_id) = interaction.guild_id else {
        return Ok(true);
    };
    let guild_i64 = guild_id.get() as i64;

    // ---- open from a panel ----------------------------------------------
    if let Some(key) = custom_id.strip_prefix("ticket:open:") {
        interaction
            .create_response(&sctx.http, CreateInteractionResponse::Defer(
                CreateInteractionResponseMessage::new().ephemeral(true),
            ))
            .await?;

        let body = match open_ticket(
            sctx,
            data,
            guild_id,
            interaction.user.id,
            &interaction.user.tag(),
            key,
            None,
        )
        .await
        {
            Ok(result) => format!(
                "Your ticket is open: <#{}> (#{}).",
                result.channel_id.get(),
                result.number
            ),
            Err(why) => why,
        };

        interaction
            .create_followup(
                &sctx.http,
                serenity::CreateInteractionResponseFollowup::new()
                    .content(body)
                    .ephemeral(true),
            )
            .await?;
        return Ok(true);
    }

    // ---- select menu version of the panel --------------------------------
    if custom_id == "ticket:select" {
        let chosen = match &interaction.data.kind {
            serenity::ComponentInteractionDataKind::StringSelect { values } => {
                values.first().cloned()
            }
            _ => None,
        };
        let Some(key) = chosen else {
            return Ok(true);
        };

        interaction
            .create_response(&sctx.http, CreateInteractionResponse::Defer(
                CreateInteractionResponseMessage::new().ephemeral(true),
            ))
            .await?;

        let body = match open_ticket(
            sctx,
            data,
            guild_id,
            interaction.user.id,
            &interaction.user.tag(),
            &key,
            None,
        )
        .await
        {
            Ok(result) => format!(
                "Your ticket is open: <#{}> (#{}).",
                result.channel_id.get(),
                result.number
            ),
            Err(why) => why,
        };

        interaction
            .create_followup(
                &sctx.http,
                serenity::CreateInteractionResponseFollowup::new()
                    .content(body)
                    .ephemeral(true),
            )
            .await?;
        return Ok(true);
    }

    // The remaining buttons act on the ticket in the current channel.
    let Some(ticket) = ticket_by_channel(&data.db, interaction.channel_id.get() as i64).await
    else {
        interaction
            .create_response(
                &sctx.http,
                CreateInteractionResponse::Message(
                    CreateInteractionResponseMessage::new()
                        .content("This is not a ticket channel.")
                        .ephemeral(true),
                ),
            )
            .await?;
        return Ok(true);
    };

    let is_support = {
        let roles = support_roles(&data.db, guild_i64, None).await;
        interaction
            .member
            .as_ref()
            .map(|m| m.roles.iter().any(|r| roles.contains(&(r.get() as i64))))
            .unwrap_or(false)
            || interaction
                .member
                .as_ref()
                .and_then(|m| m.permissions)
                .map(|p| p.manage_messages() || p.administrator())
                .unwrap_or(false)
    };

    if custom_id == "ticket:claim" {
        if !is_support {
            interaction
                .create_response(
                    &sctx.http,
                    CreateInteractionResponse::Message(
                        CreateInteractionResponseMessage::new()
                            .content("Only support staff can claim tickets.")
                            .ephemeral(true),
                    ),
                )
                .await?;
            return Ok(true);
        }

        let body = match ticket.claimed_by {
            Some(existing) if existing != interaction.user.id.get() as i64 => {
                format!("This ticket is already claimed by <@{existing}>.")
            }
            _ => {
                sqlx::query("UPDATE tickets SET claimed_by = ? WHERE id = ?")
                    .bind(interaction.user.id.get() as i64)
                    .bind(ticket.id)
                    .execute(&data.db)
                    .await?;
                let _ = interaction
                    .channel_id
                    .send_message(
                        &sctx.http,
                        CreateMessage::new().embed(
                            embed::titled("🙋 Ticket claimed", embed::SUCCESS).description(
                                format!("<@{}> is handling this ticket.", interaction.user.id.get()),
                            ),
                        ),
                    )
                    .await;
                "You claimed this ticket.".to_string()
            }
        };

        interaction
            .create_response(
                &sctx.http,
                CreateInteractionResponse::Message(
                    CreateInteractionResponseMessage::new()
                        .content(body)
                        .ephemeral(true),
                ),
            )
            .await?;
        return Ok(true);
    }

    if custom_id == "ticket:close" {
        let may_close = is_support || interaction.user.id.get() as i64 == ticket.user_id;
        if !may_close {
            interaction
                .create_response(
                    &sctx.http,
                    CreateInteractionResponse::Message(
                        CreateInteractionResponseMessage::new()
                            .content("Only the person who opened this ticket or support staff can close it.")
                            .ephemeral(true),
                    ),
                )
                .await?;
            return Ok(true);
        }

        interaction
            .create_response(
                &sctx.http,
                CreateInteractionResponse::Message(
                    CreateInteractionResponseMessage::new()
                        .content("Closing this ticket and saving the transcript...")
                        .ephemeral(true),
                ),
            )
            .await?;

        close_ticket(sctx, data, &ticket, interaction.user.id, None).await?;
        return Ok(true);
    }

    Ok(false)
}

/// Close tickets that have gone quiet. Driven by the background scheduler.
pub async fn auto_close_stale(sctx: &serenity::Context, data: &Data) {
    let rows: Vec<Ticket> = sqlx::query_as("SELECT * FROM tickets WHERE status = 'open'")
        .fetch_all(&data.db)
        .await
        .unwrap_or_default();

    for ticket in rows {
        let Ok(cfg) = db::ticket_config(&data.db, ticket.guild_id).await else {
            continue;
        };
        if cfg.auto_close_hours <= 0 {
            continue;
        }
        let Some(last) = time::from_sql(&ticket.last_activity) else {
            continue;
        };
        let idle_hours = (Utc::now() - last).num_hours();
        if idle_hours < cfg.auto_close_hours {
            continue;
        }

        tracing::info!(
            ticket = ticket.number,
            guild = ticket.guild_id,
            "auto-closing idle ticket"
        );
        let _ = close_ticket(
            sctx,
            data,
            &ticket,
            crate::util::perms::bot_id(sctx),
            Some(format!(
                "Closed automatically after {idle_hours} hours with no activity."
            )),
        )
        .await;
    }
}

pub fn commands() -> Vec<poise::Command<Data, Error>> {
    commands::commands()
}
