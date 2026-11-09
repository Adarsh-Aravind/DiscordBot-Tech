//! Ticket commands: setup, panels, categories, in-ticket controls and stats.

use poise::serenity_prelude as serenity;
use serenity::{
    ChannelId, CreateActionRow, CreateButton, CreateMessage, CreateSelectMenu,
    CreateSelectMenuKind, CreateSelectMenuOption, GuildChannel, PermissionOverwrite,
    PermissionOverwriteType, Permissions, ReactionType, Role, User,
};

use super::{priority_emoji, ticket_by_channel, Ticket};
use crate::db;
use crate::state::{Ctx, Data, Error};
use crate::util::{embed, perms, text, time};

#[derive(Debug, poise::ChoiceParameter)]
pub enum Priority {
    #[name = "low"]
    Low,
    #[name = "normal"]
    Normal,
    #[name = "high"]
    High,
    #[name = "urgent"]
    Urgent,
}

impl Priority {
    fn key(&self) -> &'static str {
        match self {
            Priority::Low => "low",
            Priority::Normal => "normal",
            Priority::High => "high",
            Priority::Urgent => "urgent",
        }
    }
}

// ===========================================================================
// Setup
// ===========================================================================

/// Set up and run the ticket system.
#[poise::command(
    slash_command,
    guild_only,
    category = "Tickets",
    subcommands(
        "tickets_setup",
        "tickets_panel",
        "tickets_category",
        "tickets_categories",
        "tickets_list",
        "tickets_stats",
        "tickets_staff"
    ),
    check = "perms::admin_only"
)]
pub async fn tickets(_ctx: Ctx<'_>) -> Result<(), Error> {
    Ok(())
}

/// Choose the category, log channel and support roles.
#[poise::command(slash_command, rename = "setup", guild_only)]
pub async fn tickets_setup(
    ctx: Ctx<'_>,
    #[description = "Channel category new tickets are created in"] category: Option<GuildChannel>,
    #[description = "Where ticket events are logged"] log_channel: Option<GuildChannel>,
    #[description = "Where transcripts are posted"] transcript_channel: Option<GuildChannel>,
    #[description = "A support role (run again to add more)"] support_role: Option<Role>,
    #[description = "Maximum open tickets per member"]
    #[min = 1]
    #[max = 10]
    max_open: Option<i64>,
    #[description = "Auto-close after this many idle hours (0 to disable)"]
    #[min = 0]
    auto_close_hours: Option<i64>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let dbh = &ctx.data().db;

    if let Some(category) = &category {
        if category.kind != serenity::ChannelType::Category {
            ctx.send(
                poise::CreateReply::default()
                    .embed(embed::err("That needs to be a channel **category**, not a text channel.")),
            )
            .await?;
            return Ok(());
        }
        db::set_opt_i64(dbh, "ticket_config", "category_id", guild_id, Some(category.id.get() as i64))
            .await?;
    }
    if let Some(channel) = &log_channel {
        db::set_opt_i64(
            dbh,
            "ticket_config",
            "log_channel",
            guild_id,
            Some(channel.id.get() as i64),
        )
        .await?;
    }
    if let Some(channel) = &transcript_channel {
        db::set_opt_i64(
            dbh,
            "ticket_config",
            "transcript_channel",
            guild_id,
            Some(channel.id.get() as i64),
        )
        .await?;
    }
    if let Some(role) = &support_role {
        let cfg = db::ticket_config(dbh, guild_id).await?;
        let mut roles = db::csv_ids(&cfg.support_roles);
        let id = role.id.get() as i64;
        if !roles.contains(&id) {
            roles.push(id);
        }
        db::set_text(dbh, "ticket_config", "support_roles", guild_id, db::ids_csv(&roles)).await?;
    }
    if let Some(value) = max_open {
        db::set_i64(dbh, "ticket_config", "max_open", guild_id, value).await?;
    }
    if let Some(value) = auto_close_hours {
        db::set_i64(dbh, "ticket_config", "auto_close_hours", guild_id, value).await?;
    }

    let cfg = db::ticket_config(dbh, guild_id).await?;
    let roles = db::csv_ids(&cfg.support_roles);

    ctx.send(
        poise::CreateReply::default().embed(
            embed::titled("🎫 Ticket configuration", embed::SUCCESS)
                .field(
                    "Channel category",
                    cfg.category_id
                        .map(|c| format!("<#{c}>"))
                        .unwrap_or_else(|| "not set (tickets go to the server root)".into()),
                    false,
                )
                .field(
                    "Log channel",
                    cfg.log_channel
                        .map(|c| format!("<#{c}>"))
                        .unwrap_or_else(|| "mod log".into()),
                    true,
                )
                .field(
                    "Transcripts",
                    cfg.transcript_channel
                        .map(|c| format!("<#{c}>"))
                        .unwrap_or_else(|| "log channel".into()),
                    true,
                )
                .field(
                    "Support roles",
                    if roles.is_empty() {
                        "none yet".to_string()
                    } else {
                        roles
                            .iter()
                            .map(|r| format!("<@&{r}>"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    },
                    false,
                )
                .field("Max open per member", cfg.max_open.to_string(), true)
                .field(
                    "Auto-close",
                    if cfg.auto_close_hours > 0 {
                        format!("after {}h idle", cfg.auto_close_hours)
                    } else {
                        "off".to_string()
                    },
                    true,
                )
                .footer(embed::footer(
                    "Add categories with /tickets category, then publish /tickets panel.",
                )),
        ),
    )
    .await?;
    Ok(())
}

/// Add or update a ticket category.
#[poise::command(slash_command, rename = "category", guild_only)]
pub async fn tickets_category(
    ctx: Ctx<'_>,
    #[description = "Short key, e.g. support"] key: String,
    #[description = "Label shown on the panel"] label: String,
    #[description = "Emoji"] emoji: Option<String>,
    #[description = "One-line description"] description: Option<String>,
    #[description = "Template shown inside the ticket. Use \\n for new lines."] template: Option<
        String,
    >,
    #[description = "Role that handles this category"] support_role: Option<Role>,
    #[description = "Channel category for these tickets"] channel_category: Option<GuildChannel>,
    #[description = "Delete this category instead"] remove: Option<bool>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let key = key.trim().to_lowercase();

    if remove.unwrap_or(false) {
        let result = sqlx::query("DELETE FROM ticket_categories WHERE guild_id = ? AND key = ?")
            .bind(guild_id)
            .bind(&key)
            .execute(&ctx.data().db)
            .await?;
        let body = if result.rows_affected() == 0 {
            embed::err("No category with that key.")
        } else {
            embed::ok(format!("Category `{key}` deleted."))
        };
        ctx.send(poise::CreateReply::default().embed(body)).await?;
        return Ok(());
    }

    sqlx::query(
        "INSERT INTO ticket_categories
            (guild_id, key, label, emoji, description, template, support_roles, category_id)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(guild_id, key) DO UPDATE SET
            label = excluded.label,
            emoji = excluded.emoji,
            description = excluded.description,
            template = excluded.template,
            support_roles = excluded.support_roles,
            category_id = excluded.category_id",
    )
    .bind(guild_id)
    .bind(&key)
    .bind(label.trim())
    .bind(emoji.unwrap_or_else(|| "🎫".to_string()))
    .bind(description.unwrap_or_default())
    .bind(template.unwrap_or_default())
    .bind(
        support_role
            .map(|r| r.id.get().to_string())
            .unwrap_or_default(),
    )
    .bind(channel_category.map(|c| c.id.get() as i64))
    .execute(&ctx.data().db)
    .await?;

    ctx.send(poise::CreateReply::default().embed(embed::ok(format!(
        "Category `{key}` saved. Re-run `/tickets panel` to refresh the panel."
    ))))
    .await?;
    Ok(())
}

/// List the ticket categories.
#[poise::command(slash_command, rename = "categories", guild_only)]
pub async fn tickets_categories(ctx: Ctx<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let rows: Vec<(String, String, String, String)> = sqlx::query_as(
        "SELECT key, label, emoji, description FROM ticket_categories WHERE guild_id = ? ORDER BY id",
    )
    .bind(guild_id)
    .fetch_all(&ctx.data().db)
    .await?;

    let body = if rows.is_empty() {
        "No categories yet. Add one with `/tickets category`.".to_string()
    } else {
        rows.iter()
            .map(|(key, label, emoji, description)| {
                format!("{emoji} **{label}** `{key}`\n└ {}", if description.is_empty() { "-" } else { description })
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    ctx.send(
        poise::CreateReply::default()
            .embed(embed::titled("Ticket categories", embed::INFO).description(body)),
    )
    .await?;
    Ok(())
}

#[derive(Debug, poise::ChoiceParameter)]
pub enum PanelStyle {
    #[name = "buttons"]
    Buttons,
    #[name = "dropdown"]
    Dropdown,
}

/// Publish the ticket panel members use to open tickets.
#[poise::command(slash_command, rename = "panel", guild_only)]
pub async fn tickets_panel(
    ctx: Ctx<'_>,
    #[description = "Channel for the panel, defaults to this one"] channel: Option<GuildChannel>,
    #[description = "Panel title"] title: Option<String>,
    #[description = "Panel body"] body: Option<String>,
    #[description = "Buttons or dropdown"] style: Option<PanelStyle>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let target = channel.map(|c| c.id).unwrap_or_else(|| ctx.channel_id());

    let rows: Vec<(String, String, String, String)> = sqlx::query_as(
        "SELECT key, label, emoji, description FROM ticket_categories WHERE guild_id = ? ORDER BY id",
    )
    .bind(guild_id)
    .fetch_all(&ctx.data().db)
    .await?;

    // With no categories configured, offer a single general ticket.
    let rows = if rows.is_empty() {
        vec![(
            "general".to_string(),
            "General support".to_string(),
            "🎫".to_string(),
            "Anything else".to_string(),
        )]
    } else {
        rows
    };

    let listing = rows
        .iter()
        .map(|(_, label, emoji, description)| {
            if description.is_empty() {
                format!("{emoji} **{label}**")
            } else {
                format!("{emoji} **{label}** — {description}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n");

    let panel = embed::titled(
        title.unwrap_or_else(|| "🎫 Need help?".to_string()),
        embed::BRAND,
    )
    .description(format!(
        "{}\n\n{listing}",
        body.unwrap_or_else(|| {
            "Pick the option that fits and a private channel will be opened for you.".to_string()
        })
    ))
    .footer(embed::footer(
        "Only you and the support team can see your ticket.",
    ));

    let components = match style.unwrap_or(PanelStyle::Buttons) {
        PanelStyle::Dropdown => {
            let options: Vec<CreateSelectMenuOption> = rows
                .iter()
                .take(25)
                .map(|(key, label, emoji, description)| {
                    let mut option = CreateSelectMenuOption::new(
                        text::truncate(label, 90),
                        key.clone(),
                    );
                    if !description.is_empty() {
                        option = option.description(text::truncate(description, 90));
                    }
                    if let Ok(reaction) = emoji.parse::<ReactionType>() {
                        option = option.emoji(reaction);
                    }
                    option
                })
                .collect();
            vec![CreateActionRow::SelectMenu(
                CreateSelectMenu::new("ticket:select", CreateSelectMenuKind::String { options })
                    .placeholder("Choose a topic"),
            )]
        }
        PanelStyle::Buttons => {
            let mut action_rows = Vec::new();
            for chunk in rows.iter().take(25).collect::<Vec<_>>().chunks(5) {
                let buttons: Vec<CreateButton> = chunk
                    .iter()
                    .map(|(key, label, emoji, _)| {
                        let mut button = CreateButton::new(format!("ticket:open:{key}"))
                            .label(text::truncate(label, 80))
                            .style(serenity::ButtonStyle::Primary);
                        if let Ok(reaction) = emoji.parse::<ReactionType>() {
                            button = button.emoji(reaction);
                        }
                        button
                    })
                    .collect();
                action_rows.push(CreateActionRow::Buttons(buttons));
            }
            action_rows
        }
    };

    target
        .send_message(
            ctx.http(),
            CreateMessage::new().embed(panel).components(components),
        )
        .await?;

    ctx.send(
        poise::CreateReply::default()
            .embed(embed::ok(format!("Panel posted in <#{}>.", target.get())))
            .ephemeral(true),
    )
    .await?;
    Ok(())
}

/// Open tickets right now.
#[poise::command(slash_command, rename = "list", guild_only)]
pub async fn tickets_list(ctx: Ctx<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let rows: Vec<Ticket> = sqlx::query_as(
        "SELECT * FROM tickets WHERE guild_id = ? AND status = 'open'
         ORDER BY CASE priority
            WHEN 'urgent' THEN 0 WHEN 'high' THEN 1 WHEN 'normal' THEN 2 ELSE 3 END,
            created_at ASC
         LIMIT 20",
    )
    .bind(guild_id)
    .fetch_all(&ctx.data().db)
    .await?;

    let body = if rows.is_empty() {
        "No open tickets.".to_string()
    } else {
        rows.iter()
            .map(|t| {
                let age = time::from_sql(&t.created_at)
                    .map(|c| time::humanize((chrono::Utc::now() - c).num_seconds()))
                    .unwrap_or_default();
                format!(
                    "{} **#{}** <#{}> · <@{}> · open {age}\n└ {}",
                    priority_emoji(&t.priority),
                    t.number,
                    t.channel_id,
                    t.user_id,
                    t.claimed_by
                        .map(|id| format!("claimed by <@{id}>"))
                        .unwrap_or_else(|| "**unclaimed**".to_string())
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    ctx.send(
        poise::CreateReply::default()
            .embed(embed::titled("Open tickets", embed::INFO).description(body)),
    )
    .await?;
    Ok(())
}

/// Ticket volume, response times and ratings.
#[poise::command(slash_command, rename = "stats", guild_only)]
pub async fn tickets_stats(ctx: Ctx<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let dbh = &ctx.data().db;

    let (open, closed): (i64, i64) = sqlx::query_as(
        "SELECT
            SUM(CASE WHEN status = 'open' THEN 1 ELSE 0 END),
            SUM(CASE WHEN status = 'closed' THEN 1 ELSE 0 END)
         FROM tickets WHERE guild_id = ?",
    )
    .bind(guild_id)
    .fetch_optional(dbh)
    .await?
    .unwrap_or((0, 0));

    // Average first-response and resolution time, in seconds.
    let avg_response: Option<f64> = sqlx::query_scalar(
        "SELECT AVG(strftime('%s', first_reply_at) - strftime('%s', created_at))
         FROM tickets WHERE guild_id = ? AND first_reply_at IS NOT NULL",
    )
    .bind(guild_id)
    .fetch_optional(dbh)
    .await?
    .flatten();

    let avg_resolution: Option<f64> = sqlx::query_scalar(
        "SELECT AVG(strftime('%s', closed_at) - strftime('%s', created_at))
         FROM tickets WHERE guild_id = ? AND closed_at IS NOT NULL",
    )
    .bind(guild_id)
    .fetch_optional(dbh)
    .await?
    .flatten();

    let avg_rating: Option<f64> = sqlx::query_scalar(
        "SELECT AVG(rating) FROM tickets WHERE guild_id = ? AND rating IS NOT NULL",
    )
    .bind(guild_id)
    .fetch_optional(dbh)
    .await?
    .flatten();

    let rated: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM tickets WHERE guild_id = ? AND rating IS NOT NULL")
            .bind(guild_id)
            .fetch_one(dbh)
            .await?;

    let by_category: Vec<(String, i64)> = sqlx::query_as(
        "SELECT category, COUNT(*) FROM tickets WHERE guild_id = ?
         GROUP BY category ORDER BY COUNT(*) DESC LIMIT 10",
    )
    .bind(guild_id)
    .fetch_all(dbh)
    .await?;

    let unclaimed: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM tickets WHERE guild_id = ? AND status = 'open' AND claimed_by IS NULL",
    )
    .bind(guild_id)
    .fetch_one(dbh)
    .await?;

    let category_body = if by_category.is_empty() {
        "-".to_string()
    } else {
        by_category
            .iter()
            .map(|(category, count)| format!("**{category}** · {count}"))
            .collect::<Vec<_>>()
            .join("\n")
    };

    ctx.send(
        poise::CreateReply::default().embed(
            embed::titled("🎫 Ticket statistics", embed::INFO)
                .field("Open", open.to_string(), true)
                .field("Closed", closed.to_string(), true)
                .field("Unclaimed", unclaimed.to_string(), true)
                .field(
                    "Average first response",
                    avg_response
                        .map(|s| time::humanize(s as i64))
                        .unwrap_or_else(|| "no data".into()),
                    true,
                )
                .field(
                    "Average time to close",
                    avg_resolution
                        .map(|s| time::humanize(s as i64))
                        .unwrap_or_else(|| "no data".into()),
                    true,
                )
                .field(
                    "Average rating",
                    avg_rating
                        .map(|r| format!("{r:.2} / 5 ({rated} rated)"))
                        .unwrap_or_else(|| "no ratings yet".into()),
                    true,
                )
                .field("By category", category_body, false),
        ),
    )
    .await?;
    Ok(())
}

/// Support staff leaderboard.
#[poise::command(slash_command, rename = "staff", guild_only)]
pub async fn tickets_staff(ctx: Ctx<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;

    let rows: Vec<(i64, i64, Option<f64>, Option<f64>)> = sqlx::query_as(
        "SELECT claimed_by,
                COUNT(*),
                AVG(rating),
                AVG(strftime('%s', first_reply_at) - strftime('%s', created_at))
         FROM tickets
         WHERE guild_id = ? AND claimed_by IS NOT NULL
         GROUP BY claimed_by
         ORDER BY COUNT(*) DESC
         LIMIT 15",
    )
    .bind(guild_id)
    .fetch_all(&ctx.data().db)
    .await?;

    let body = if rows.is_empty() {
        "No tickets have been claimed yet.".to_string()
    } else {
        rows.iter()
            .enumerate()
            .map(|(i, (staff_id, handled, rating, response))| {
                let medal = match i {
                    0 => "🥇",
                    1 => "🥈",
                    2 => "🥉",
                    _ => "▫️",
                };
                format!(
                    "{medal} <@{staff_id}> — **{handled}** handled · rating {} · first reply {}",
                    rating
                        .map(|r| format!("{r:.1}/5"))
                        .unwrap_or_else(|| "n/a".into()),
                    response
                        .map(|s| time::humanize(s as i64))
                        .unwrap_or_else(|| "n/a".into())
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    ctx.send(
        poise::CreateReply::default()
            .embed(embed::titled("🏅 Support staff leaderboard", embed::BRAND).description(body)),
    )
    .await?;
    Ok(())
}

// ===========================================================================
// In-ticket controls
// ===========================================================================

/// Controls for the ticket you are in.
#[poise::command(
    slash_command,
    guild_only,
    category = "Tickets",
    subcommands(
        "ticket_new",
        "ticket_close",
        "ticket_claim",
        "ticket_assign",
        "ticket_add",
        "ticket_remove",
        "ticket_priority",
        "ticket_rename",
        "ticket_reopen",
        "ticket_transcript",
        "ticket_info"
    )
)]
pub async fn ticket(_ctx: Ctx<'_>) -> Result<(), Error> {
    Ok(())
}

/// Open a ticket without using the panel.
#[poise::command(slash_command, rename = "new", guild_only)]
pub async fn ticket_new(
    ctx: Ctx<'_>,
    #[description = "What do you need help with?"] subject: String,
    #[description = "Category key, see /tickets categories"] category: Option<String>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap();
    ctx.defer_ephemeral().await?;

    let body = match super::open_ticket(
        ctx.serenity_context(),
        ctx.data(),
        guild_id,
        ctx.author().id,
        &ctx.author().tag(),
        &category.unwrap_or_else(|| "general".to_string()),
        Some(subject),
    )
    .await
    {
        Ok(result) => embed::ok(format!(
            "Your ticket is open: <#{}> (#{}).",
            result.channel_id.get(),
            result.number
        )),
        Err(why) => embed::err(why),
    };

    ctx.send(poise::CreateReply::default().embed(body).ephemeral(true))
        .await?;
    Ok(())
}

async fn current(ctx: &Ctx<'_>) -> Option<Ticket> {
    ticket_by_channel(&ctx.data().db, ctx.channel_id().get() as i64).await
}

async fn require_ticket(ctx: &Ctx<'_>) -> Result<Option<Ticket>, Error> {
    match current(ctx).await {
        Some(ticket) => Ok(Some(ticket)),
        None => {
            ctx.send(
                poise::CreateReply::default()
                    .embed(embed::err("Run this inside a ticket channel."))
                    .ephemeral(true),
            )
            .await?;
            Ok(None)
        }
    }
}

/// Close this ticket.
#[poise::command(slash_command, rename = "close", guild_only)]
pub async fn ticket_close(
    ctx: Ctx<'_>,
    #[description = "Why is it being closed?"] reason: Option<String>,
) -> Result<(), Error> {
    let Some(ticket) = require_ticket(&ctx).await? else {
        return Ok(());
    };

    let is_owner = ctx.author().id.get() as i64 == ticket.user_id;
    if !is_owner && !perms::is_staff(&ctx).await {
        ctx.send(
            poise::CreateReply::default()
                .embed(embed::err("Only the ticket owner or support staff can close this.")),
        )
        .await?;
        return Ok(());
    }

    ctx.send(poise::CreateReply::default().embed(embed::ok(
        "Closing this ticket and saving the transcript...",
    )))
    .await?;

    super::close_ticket(
        ctx.serenity_context(),
        ctx.data(),
        &ticket,
        ctx.author().id,
        reason,
    )
    .await?;
    Ok(())
}

/// Claim this ticket.
#[poise::command(slash_command, rename = "claim", guild_only, check = "perms::staff_only")]
pub async fn ticket_claim(ctx: Ctx<'_>) -> Result<(), Error> {
    let Some(ticket) = require_ticket(&ctx).await? else {
        return Ok(());
    };

    if let Some(existing) = ticket.claimed_by {
        if existing != ctx.author().id.get() as i64 {
            ctx.send(poise::CreateReply::default().embed(embed::err(format!(
                "Already claimed by <@{existing}>. Use `/ticket assign` to hand it over."
            ))))
            .await?;
            return Ok(());
        }
    }

    sqlx::query("UPDATE tickets SET claimed_by = ? WHERE id = ?")
        .bind(ctx.author().id.get() as i64)
        .bind(ticket.id)
        .execute(&ctx.data().db)
        .await?;

    ctx.send(
        poise::CreateReply::default().embed(
            embed::titled("🙋 Ticket claimed", embed::SUCCESS)
                .description(format!("<@{}> is handling this ticket.", ctx.author().id.get())),
        ),
    )
    .await?;
    Ok(())
}

/// Hand this ticket to another staff member.
#[poise::command(slash_command, rename = "assign", guild_only, check = "perms::staff_only")]
pub async fn ticket_assign(
    ctx: Ctx<'_>,
    #[description = "Staff member to assign it to"] staff: User,
) -> Result<(), Error> {
    let Some(ticket) = require_ticket(&ctx).await? else {
        return Ok(());
    };

    sqlx::query("UPDATE tickets SET assigned_to = ?, claimed_by = ? WHERE id = ?")
        .bind(staff.id.get() as i64)
        .bind(staff.id.get() as i64)
        .bind(ticket.id)
        .execute(&ctx.data().db)
        .await?;

    // Make sure they can actually see the channel.
    let _ = ctx
        .channel_id()
        .create_permission(
            ctx.http(),
            PermissionOverwrite {
                allow: Permissions::VIEW_CHANNEL
                    | Permissions::SEND_MESSAGES
                    | Permissions::READ_MESSAGE_HISTORY,
                deny: Permissions::empty(),
                kind: PermissionOverwriteType::Member(staff.id),
            },
            Some("Assigned to ticket"),
        )
        .await;

    ctx.send(
        poise::CreateReply::default().embed(
            embed::titled("📋 Ticket assigned", embed::INFO)
                .description(format!("Assigned to <@{}>.", staff.id.get())),
        ),
    )
    .await?;
    Ok(())
}

/// Add someone to this ticket.
#[poise::command(slash_command, rename = "add", guild_only, check = "perms::staff_only")]
pub async fn ticket_add(
    ctx: Ctx<'_>,
    #[description = "Member to add"] user: User,
) -> Result<(), Error> {
    let Some(ticket) = require_ticket(&ctx).await? else {
        return Ok(());
    };

    ctx.channel_id()
        .create_permission(
            ctx.http(),
            PermissionOverwrite {
                allow: Permissions::VIEW_CHANNEL
                    | Permissions::SEND_MESSAGES
                    | Permissions::READ_MESSAGE_HISTORY
                    | Permissions::ATTACH_FILES,
                deny: Permissions::empty(),
                kind: PermissionOverwriteType::Member(user.id),
            },
            Some("Added to ticket"),
        )
        .await?;

    sqlx::query("INSERT OR IGNORE INTO ticket_members (ticket_id, user_id) VALUES (?, ?)")
        .bind(ticket.id)
        .bind(user.id.get() as i64)
        .execute(&ctx.data().db)
        .await?;

    ctx.send(poise::CreateReply::default().embed(embed::ok(format!(
        "Added <@{}> to this ticket.",
        user.id.get()
    ))))
    .await?;
    Ok(())
}

/// Remove someone from this ticket.
#[poise::command(slash_command, rename = "remove", guild_only, check = "perms::staff_only")]
pub async fn ticket_remove(
    ctx: Ctx<'_>,
    #[description = "Member to remove"] user: User,
) -> Result<(), Error> {
    let Some(ticket) = require_ticket(&ctx).await? else {
        return Ok(());
    };

    if user.id.get() as i64 == ticket.user_id {
        ctx.send(
            poise::CreateReply::default()
                .embed(embed::err("You cannot remove the person who opened the ticket.")),
        )
        .await?;
        return Ok(());
    }

    ctx.channel_id()
        .delete_permission(
            ctx.http(),
            PermissionOverwriteType::Member(user.id),
            Some("Removed from ticket"),
        )
        .await?;

    sqlx::query("DELETE FROM ticket_members WHERE ticket_id = ? AND user_id = ?")
        .bind(ticket.id)
        .bind(user.id.get() as i64)
        .execute(&ctx.data().db)
        .await?;

    ctx.send(poise::CreateReply::default().embed(embed::ok(format!(
        "Removed <@{}> from this ticket.",
        user.id.get()
    ))))
    .await?;
    Ok(())
}

/// Set the priority of this ticket.
#[poise::command(slash_command, rename = "priority", guild_only, check = "perms::staff_only")]
pub async fn ticket_priority(
    ctx: Ctx<'_>,
    #[description = "New priority"] priority: Priority,
) -> Result<(), Error> {
    let Some(ticket) = require_ticket(&ctx).await? else {
        return Ok(());
    };

    sqlx::query("UPDATE tickets SET priority = ? WHERE id = ?")
        .bind(priority.key())
        .bind(ticket.id)
        .execute(&ctx.data().db)
        .await?;

    // Reflect it in the channel name so the queue is readable at a glance.
    let prefix = match priority.key() {
        "urgent" => "urgent-",
        "high" => "high-",
        _ => "",
    };
    let _ = ctx
        .channel_id()
        .edit(
            ctx.http(),
            serenity::EditChannel::new()
                .name(format!("{prefix}ticket-{:04}", ticket.number)),
        )
        .await;

    ctx.send(poise::CreateReply::default().embed(embed::ok(format!(
        "{} Priority is now **{}**.",
        priority_emoji(priority.key()),
        priority.key()
    ))))
    .await?;
    Ok(())
}

/// Rename this ticket channel.
#[poise::command(slash_command, rename = "rename", guild_only, check = "perms::staff_only")]
pub async fn ticket_rename(
    ctx: Ctx<'_>,
    #[description = "New channel name"] name: String,
) -> Result<(), Error> {
    if require_ticket(&ctx).await?.is_none() {
        return Ok(());
    }
    let clean: String = name
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .take(90)
        .collect();

    ctx.channel_id()
        .edit(ctx.http(), serenity::EditChannel::new().name(clean.clone()))
        .await?;
    ctx.send(poise::CreateReply::default().embed(embed::ok(format!("Renamed to `{clean}`."))))
        .await?;
    Ok(())
}

/// Reopen a closed ticket by number. A fresh channel is created.
#[poise::command(slash_command, rename = "reopen", guild_only, check = "perms::staff_only")]
pub async fn ticket_reopen(
    ctx: Ctx<'_>,
    #[description = "Ticket number"] number: i64,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap();
    let guild_i64 = guild_id.get() as i64;

    let found: Option<Ticket> =
        sqlx::query_as("SELECT * FROM tickets WHERE guild_id = ? AND number = ?")
            .bind(guild_i64)
            .bind(number)
            .fetch_optional(&ctx.data().db)
            .await?;

    let Some(old) = found else {
        ctx.send(poise::CreateReply::default().embed(embed::err("No ticket with that number.")))
            .await?;
        return Ok(());
    };
    if old.status == "open" {
        ctx.send(poise::CreateReply::default().embed(embed::warn(format!(
            "Ticket #{number} is still open: <#{}>.",
            old.channel_id
        ))))
        .await?;
        return Ok(());
    }

    ctx.defer().await?;

    let result = super::open_ticket(
        ctx.serenity_context(),
        ctx.data(),
        guild_id,
        serenity::UserId::new(old.user_id as u64),
        &format!("user {}", old.user_id),
        &old.category,
        Some(format!("Reopened from ticket #{number}: {}", old.subject)),
    )
    .await;

    let body = match result {
        Ok(result) => {
            // Carry the old conversation across so context is not lost.
            let history: Vec<(String, String, i64)> = sqlx::query_as(
                "SELECT author_tag, content, is_staff FROM ticket_messages
                 WHERE ticket_id = ? ORDER BY id ASC LIMIT 25",
            )
            .bind(old.id)
            .fetch_all(&ctx.data().db)
            .await
            .unwrap_or_default();

            if !history.is_empty() {
                let recap = history
                    .iter()
                    .map(|(author, content, staff)| {
                        format!(
                            "{} **{author}:** {}",
                            if *staff == 1 { "🛠️" } else { "💬" },
                            text::truncate(content, 150)
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                let _ = result
                    .channel_id
                    .send_message(
                        ctx.http(),
                        CreateMessage::new().embed(
                            embed::titled(
                                format!("Previous conversation from ticket #{number}"),
                                embed::MUTED,
                            )
                            .description(text::truncate(&recap, 3800)),
                        ),
                    )
                    .await;
            }

            embed::ok(format!(
                "Reopened as <#{}> (#{}).",
                result.channel_id.get(),
                result.number
            ))
        }
        Err(why) => embed::err(why),
    };

    ctx.send(poise::CreateReply::default().embed(body)).await?;
    Ok(())
}

/// Get the transcript of a ticket without closing it.
#[poise::command(
    slash_command,
    rename = "transcript",
    guild_only,
    check = "perms::staff_only"
)]
pub async fn ticket_transcript(
    ctx: Ctx<'_>,
    #[description = "Ticket number, defaults to this channel"] number: Option<i64>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;

    let ticket: Option<Ticket> = match number {
        Some(number) => {
            sqlx::query_as("SELECT * FROM tickets WHERE guild_id = ? AND number = ?")
                .bind(guild_id)
                .bind(number)
                .fetch_optional(&ctx.data().db)
                .await?
        }
        None => current(&ctx).await,
    };

    let Some(ticket) = ticket else {
        ctx.send(
            poise::CreateReply::default()
                .embed(embed::err("No such ticket. Run this in a ticket channel or pass a number.")),
        )
        .await?;
        return Ok(());
    };

    let rows: Vec<(String, String, i64)> = sqlx::query_as(
        "SELECT author_tag, content, is_staff FROM ticket_messages
         WHERE ticket_id = ? ORDER BY id ASC",
    )
    .bind(ticket.id)
    .fetch_all(&ctx.data().db)
    .await?;

    let mut body = format!(
        "Ticket #{:04} ({})\nCategory: {}\nOpened: {} UTC\n{}\n",
        ticket.number,
        ticket.status,
        ticket.category,
        ticket.created_at,
        "-".repeat(50)
    );
    for (author, content, staff) in rows {
        body.push_str(&format!(
            "{} {author}: {content}\n",
            if staff == 1 { "[staff]" } else { "       " }
        ));
    }

    ctx.send(
        poise::CreateReply::default()
            .attachment(serenity::CreateAttachment::bytes(
                body.into_bytes(),
                format!("ticket-{:04}.txt", ticket.number),
            ))
            .ephemeral(true),
    )
    .await?;
    Ok(())
}

/// Details about this ticket.
#[poise::command(slash_command, rename = "info", guild_only)]
pub async fn ticket_info(ctx: Ctx<'_>) -> Result<(), Error> {
    let Some(ticket) = require_ticket(&ctx).await? else {
        return Ok(());
    };

    let message_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM ticket_messages WHERE ticket_id = ?")
            .bind(ticket.id)
            .fetch_one(&ctx.data().db)
            .await?;

    let extra: Vec<i64> =
        sqlx::query_scalar("SELECT user_id FROM ticket_members WHERE ticket_id = ?")
            .bind(ticket.id)
            .fetch_all(&ctx.data().db)
            .await?;

    let age = time::from_sql(&ticket.created_at)
        .map(|c| time::humanize((chrono::Utc::now() - c).num_seconds()))
        .unwrap_or_default();

    ctx.send(
        poise::CreateReply::default().embed(
            embed::titled(format!("Ticket #{}", ticket.number), embed::INFO)
                .field("Opened by", format!("<@{}>", ticket.user_id), true)
                .field("Category", ticket.category.clone(), true)
                .field(
                    "Priority",
                    format!("{} {}", priority_emoji(&ticket.priority), ticket.priority),
                    true,
                )
                .field(
                    "Claimed by",
                    ticket
                        .claimed_by
                        .map(|id| format!("<@{id}>"))
                        .unwrap_or_else(|| "unclaimed".into()),
                    true,
                )
                .field("Open for", age, true)
                .field("Messages recorded", message_count.to_string(), true)
                .field(
                    "Also in this ticket",
                    if extra.is_empty() {
                        "-".to_string()
                    } else {
                        extra
                            .iter()
                            .map(|id| format!("<@{id}>"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    },
                    false,
                )
                .field(
                    "Subject",
                    if ticket.subject.is_empty() {
                        "-".to_string()
                    } else {
                        text::truncate(&ticket.subject, 800)
                    },
                    false,
                ),
        ),
    )
    .await?;
    Ok(())
}

pub fn commands() -> Vec<poise::Command<Data, Error>> {
    vec![tickets(), ticket()]
}
