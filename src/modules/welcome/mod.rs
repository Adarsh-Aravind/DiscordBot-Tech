//! Welcome and goodbye messages, auto-roles, verification and the join/leave
//! bookkeeping every other module relies on.

pub mod invites;
pub mod profiles;
pub mod roles;

use poise::serenity_prelude as serenity;
use serenity::{
    ChannelId, CreateActionRow, CreateButton, CreateMessage, GuildChannel, GuildId, Member,
    RoleId, User,
};

use crate::compat;
use crate::db::{self, WelcomeConfig};
use crate::logs::{self, Log};
use crate::modules::automod::raid;
use crate::state::{Ctx, Data, Error};
use crate::util::{embed, perms, text, time};

/// Everything that happens when someone joins.
pub async fn handle_join(
    sctx: &serenity::Context,
    data: &Data,
    member: &Member,
) -> Result<(), Error> {
    let guild_id = member.guild_id;
    let guild_i64 = guild_id.get() as i64;
    let user_id = member.user.id.get() as i64;

    db::ensure_guild(&data.db, guild_i64).await?;

    // Remember the join even if the member is about to be removed - the
    // raid-cleanup command reads this table.
    sqlx::query(
        "INSERT INTO members (guild_id, user_id, first_join, last_join, join_count)
         VALUES (?, ?, ?, ?, 1)
         ON CONFLICT(guild_id, user_id) DO UPDATE SET
            last_join = excluded.last_join,
            join_count = members.join_count + 1",
    )
    .bind(guild_i64)
    .bind(user_id)
    .bind(time::sql_now())
    .bind(time::sql_now())
    .execute(&data.db)
    .await?;

    crate::modules::analytics::bump_daily(&data.db, guild_i64, "joins", 1).await;

    // Automod gets first look: it may kick or ban the account.
    let verdict = raid::handle_join(sctx, data, member).await?;

    // Invite attribution runs regardless so the inviter's stats stay honest.
    let inviter = invites::attribute_join(sctx, data, guild_id, member.user.id).await;

    let cfg = db::welcome_config(&data.db, guild_i64).await?;

    // ---- join log --------------------------------------------------------
    let account_age = chrono::Utc::now().timestamp() - member.user.id.created_at().unix_timestamp();
    let mut log_embed = embed::titled("📥 Member joined", embed::SUCCESS)
        .description(format!(
            "<@{}> `{}`",
            member.user.id.get(),
            member.user.tag()
        ))
        .field(
            "Account created",
            format!(
                "<t:{}:R> ({})",
                member.user.id.created_at().unix_timestamp(),
                time::humanize(account_age)
            ),
            true,
        )
        .thumbnail(member.user.face())
        .footer(embed::footer(format!("User ID: {}", member.user.id.get())));

    if let Some((inviter_id, uses)) = inviter {
        log_embed = log_embed.field(
            "Invited by",
            format!("<@{inviter_id}> ({uses} invite(s))"),
            true,
        );
    }
    if let Some(note) = &verdict.note {
        log_embed = log_embed.field("Automod", note.clone(), true);
    }
    logs::send(sctx, &data.db, guild_i64, Log::Join, log_embed).await;

    if verdict.blocked {
        return Ok(());
    }

    // ---- auto-roles ------------------------------------------------------
    // Bots get their own list so you are not handing out member roles to apps.
    let role_csv = if member.user.bot {
        &cfg.bot_autoroles
    } else {
        &cfg.autoroles
    };
    // With verification on, hold back the auto-roles until they verify.
    let gate_roles = cfg.verify_role.is_some() && !member.user.bot;
    if !gate_roles {
        for role_id in db::csv_ids(role_csv) {
            let _ = compat::add_role(
                &sctx.http,
                guild_id,
                member.user.id,
                RoleId::new(role_id as u64),
            )
            .await;
        }
    }

    // ---- welcome message -------------------------------------------------
    if !member.user.bot {
        send_welcome(sctx, data, member, &cfg, inviter.map(|(id, _)| id)).await;
    }

    Ok(())
}

async fn send_welcome(
    sctx: &serenity::Context,
    data: &Data,
    member: &Member,
    cfg: &WelcomeConfig,
    inviter: Option<i64>,
) {
    let guild_id = member.guild_id;
    let member_count = guild_id
        .to_guild_cached(&sctx.cache)
        .map(|g| g.member_count)
        .unwrap_or(0);

    let vars = vec![
        ("mention", format!("<@{}>", member.user.id.get())),
        ("user", member.user.name.clone()),
        ("tag", member.user.tag()),
        ("id", member.user.id.get().to_string()),
        (
            "server",
            crate::modules::moderation::guild_name(sctx, guild_id),
        ),
        ("count", member_count.to_string()),
        (
            "inviter",
            inviter
                .map(|id| format!("<@{id}>"))
                .unwrap_or_else(|| "someone".to_string()),
        ),
    ];

    // Channel message.
    if let Some(channel_id) = cfg.welcome_channel {
        let body = text::render(&cfg.welcome_message, &vars);
        let channel = ChannelId::new(channel_id as u64);
        let message = if cfg.welcome_embed == 1 {
            let mut e = embed::titled("Welcome!", embed::SUCCESS)
                .description(body)
                .thumbnail(member.user.face())
                .footer(embed::footer(format!("Member #{member_count}")))
                .timestamp(serenity::Timestamp::now());
            if let Some(image) = &cfg.welcome_image {
                e = e.image(image.clone());
            }
            CreateMessage::new()
                .content(format!("<@{}>", member.user.id.get()))
                .embed(e)
        } else {
            CreateMessage::new().content(body)
        };
        let _ = channel.send_message(&sctx.http, message).await;
    }

    // Direct message.
    if let Some(template) = &cfg.welcome_dm {
        if !template.trim().is_empty() {
            let body = text::render(template, &vars);
            if let Ok(channel) = member.user.id.create_dm_channel(sctx).await {
                let _ = channel
                    .send_message(
                        &sctx.http,
                        CreateMessage::new().embed(
                            embed::titled(
                                format!(
                                    "Welcome to {}",
                                    crate::modules::moderation::guild_name(sctx, guild_id)
                                ),
                                embed::SUCCESS,
                            )
                            .description(body),
                        ),
                    )
                    .await;
            }
        }
    }

    let _ = data;
}

/// Goodbye message, invite bookkeeping and the leave log.
pub async fn handle_leave(
    sctx: &serenity::Context,
    data: &Data,
    guild_id: GuildId,
    user: &User,
) -> Result<(), Error> {
    let guild_i64 = guild_id.get() as i64;
    let user_id = user.id.get() as i64;

    sqlx::query("UPDATE members SET last_leave = ? WHERE guild_id = ? AND user_id = ?")
        .bind(time::sql_now())
        .bind(guild_i64)
        .bind(user_id)
        .execute(&data.db)
        .await?;

    crate::modules::analytics::bump_daily(&data.db, guild_i64, "leaves", 1).await;

    // Credit the leave against whoever invited them.
    let inviter: Option<i64> =
        sqlx::query_scalar("SELECT inviter_id FROM members WHERE guild_id = ? AND user_id = ?")
            .bind(guild_i64)
            .bind(user_id)
            .fetch_optional(&data.db)
            .await?
            .flatten();
    if let Some(inviter_id) = inviter {
        sqlx::query(
            "INSERT INTO invite_stats (guild_id, user_id, departed) VALUES (?, ?, 1)
             ON CONFLICT(guild_id, user_id) DO UPDATE SET departed = invite_stats.departed + 1",
        )
        .bind(guild_i64)
        .bind(inviter_id)
        .execute(&data.db)
        .await?;
    }

    let cfg = db::welcome_config(&data.db, guild_i64).await?;

    let joined_at: Option<String> =
        sqlx::query_scalar("SELECT last_join FROM members WHERE guild_id = ? AND user_id = ?")
            .bind(guild_i64)
            .bind(user_id)
            .fetch_optional(&data.db)
            .await?;
    let stayed = joined_at
        .as_deref()
        .and_then(time::from_sql)
        .map(|t| time::humanize((chrono::Utc::now() - t).num_seconds()))
        .unwrap_or_else(|| "unknown".to_string());

    logs::send(
        sctx,
        &data.db,
        guild_i64,
        Log::Join,
        embed::titled("📤 Member left", embed::DANGER)
            .description(format!("<@{}> `{}`", user.id.get(), user.tag()))
            .field("Was here for", stayed, true)
            .field(
                "Invited by",
                inviter
                    .map(|id| format!("<@{id}>"))
                    .unwrap_or_else(|| "unknown".to_string()),
                true,
            )
            .thumbnail(user.face())
            .footer(embed::footer(format!("User ID: {}", user.id.get()))),
    )
    .await;

    if let Some(channel_id) = cfg.goodbye_channel {
        let vars = vec![
            ("mention", format!("<@{}>", user.id.get())),
            ("user", user.name.clone()),
            ("tag", user.tag()),
            ("id", user.id.get().to_string()),
            (
                "server",
                crate::modules::moderation::guild_name(sctx, guild_id),
            ),
        ];
        let body = text::render(&cfg.goodbye_message, &vars);
        let _ = ChannelId::new(channel_id as u64)
            .send_message(
                &sctx.http,
                CreateMessage::new().embed(
                    embed::titled("Goodbye", embed::MUTED)
                        .description(body)
                        .thumbnail(user.face()),
                ),
            )
            .await;
    }

    Ok(())
}

// ===========================================================================
// Commands
// ===========================================================================

/// Configure the welcome and goodbye flow.
#[poise::command(
    slash_command,
    guild_only,
    category = "Welcome",
    subcommands(
        "welcome_channel",
        "welcome_message",
        "welcome_dm",
        "welcome_goodbye",
        "welcome_autorole",
        "welcome_test",
        "welcome_status",
        "welcome_off"
    ),
    check = "perms::admin_only"
)]
pub async fn welcome(_ctx: Ctx<'_>) -> Result<(), Error> {
    Ok(())
}

const PLACEHOLDER_HELP: &str =
    "Placeholders: `{mention}` `{user}` `{tag}` `{id}` `{server}` `{count}` `{inviter}`";

/// Where welcome messages are posted.
#[poise::command(slash_command, rename = "channel", guild_only)]
pub async fn welcome_channel(
    ctx: Ctx<'_>,
    #[description = "Channel for welcome messages"] channel: GuildChannel,
    #[description = "Send as an embed? (default yes)"] as_embed: Option<bool>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    db::set_opt_i64(
        &ctx.data().db,
        "welcome_config",
        "welcome_channel",
        guild_id,
        Some(channel.id.get() as i64),
    )
    .await?;
    db::set_bool(
        &ctx.data().db,
        "welcome_config",
        "welcome_embed",
        guild_id,
        as_embed.unwrap_or(true),
    )
    .await?;

    ctx.send(poise::CreateReply::default().embed(embed::ok(format!(
        "Welcome messages will go to <#{}>.",
        channel.id.get()
    ))))
    .await?;
    Ok(())
}

/// Set the welcome text.
#[poise::command(slash_command, rename = "message", guild_only)]
pub async fn welcome_message(
    ctx: Ctx<'_>,
    #[description = "Message text, with placeholders"] message: String,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    db::set_text(
        &ctx.data().db,
        "welcome_config",
        "welcome_message",
        guild_id,
        message.clone(),
    )
    .await?;
    ctx.send(
        poise::CreateReply::default().embed(
            embed::ok("Welcome message saved.")
                .field("Preview", text::truncate(&message, 500), false)
                .footer(embed::footer(PLACEHOLDER_HELP)),
        ),
    )
    .await?;
    Ok(())
}

/// Set a DM that new members receive. Leave empty to switch it off.
#[poise::command(slash_command, rename = "dm", guild_only)]
pub async fn welcome_dm(
    ctx: Ctx<'_>,
    #[description = "DM text, or leave empty to disable"] message: Option<String>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    db::set_opt_text(
        &ctx.data().db,
        "welcome_config",
        "welcome_dm",
        guild_id,
        message.clone(),
    )
    .await?;
    let body = match message {
        Some(_) => "Welcome DM saved.",
        None => "Welcome DMs are off.",
    };
    ctx.send(poise::CreateReply::default().embed(embed::ok(body)))
        .await?;
    Ok(())
}

/// Configure the goodbye message.
#[poise::command(slash_command, rename = "goodbye", guild_only)]
pub async fn welcome_goodbye(
    ctx: Ctx<'_>,
    #[description = "Channel for goodbye messages"] channel: Option<GuildChannel>,
    #[description = "Goodbye text"] message: Option<String>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    if let Some(channel) = &channel {
        db::set_opt_i64(
            &ctx.data().db,
            "welcome_config",
            "goodbye_channel",
            guild_id,
            Some(channel.id.get() as i64),
        )
        .await?;
    }
    if let Some(message) = &message {
        db::set_text(
            &ctx.data().db,
            "welcome_config",
            "goodbye_message",
            guild_id,
            message.clone(),
        )
        .await?;
    }
    if channel.is_none() && message.is_none() {
        db::set_opt_i64(
            &ctx.data().db,
            "welcome_config",
            "goodbye_channel",
            guild_id,
            None,
        )
        .await?;
        ctx.send(poise::CreateReply::default().embed(embed::ok("Goodbye messages are off.")))
            .await?;
        return Ok(());
    }
    ctx.send(poise::CreateReply::default().embed(embed::ok("Goodbye settings saved.")))
        .await?;
    Ok(())
}

/// Roles given automatically on join.
#[poise::command(slash_command, rename = "autorole", guild_only)]
pub async fn welcome_autorole(
    ctx: Ctx<'_>,
    #[description = "Role to add or remove from the list"] role: serenity::Role,
    #[description = "Add it? (false removes it)"] add: Option<bool>,
    #[description = "Apply to bots instead of people"] for_bots: Option<bool>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let column = if for_bots.unwrap_or(false) {
        "bot_autoroles"
    } else {
        "autoroles"
    };

    let cfg = db::welcome_config(&ctx.data().db, guild_id).await?;
    let current = if column == "autoroles" {
        &cfg.autoroles
    } else {
        &cfg.bot_autoroles
    };
    let mut ids = db::csv_ids(current);
    let role_i64 = role.id.get() as i64;

    if add.unwrap_or(true) {
        if !ids.contains(&role_i64) {
            ids.push(role_i64);
        }
    } else {
        ids.retain(|r| *r != role_i64);
    }

    db::set_text(
        &ctx.data().db,
        "welcome_config",
        column,
        guild_id,
        db::ids_csv(&ids),
    )
    .await?;

    let listed = if ids.is_empty() {
        "none".to_string()
    } else {
        ids.iter()
            .map(|r| format!("<@&{r}>"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    ctx.send(poise::CreateReply::default().embed(embed::ok(format!(
        "Auto-roles for {}: {listed}",
        if column == "autoroles" { "members" } else { "bots" }
    ))))
    .await?;
    Ok(())
}

/// Preview the welcome message as if you had just joined.
#[poise::command(slash_command, rename = "test", guild_only)]
pub async fn welcome_test(ctx: Ctx<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap();
    let cfg = db::welcome_config(&ctx.data().db, guild_id.get() as i64).await?;

    if let Some(member) = ctx.author_member().await {
        send_welcome(
            ctx.serenity_context(),
            ctx.data(),
            &member,
            &cfg,
            Some(ctx.author().id.get() as i64),
        )
        .await;
        ctx.send(
            poise::CreateReply::default()
                .embed(embed::ok("Sent a test welcome using your account."))
                .ephemeral(true),
        )
        .await?;
    }
    Ok(())
}

/// Show the current welcome configuration.
#[poise::command(slash_command, rename = "status", guild_only)]
pub async fn welcome_status(ctx: Ctx<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let cfg = db::welcome_config(&ctx.data().db, guild_id).await?;

    let roles = |csv: &str| {
        let ids = db::csv_ids(csv);
        if ids.is_empty() {
            "none".to_string()
        } else {
            ids.iter()
                .map(|r| format!("<@&{r}>"))
                .collect::<Vec<_>>()
                .join(", ")
        }
    };

    ctx.send(
        poise::CreateReply::default().embed(
            embed::titled("Welcome configuration", embed::INFO)
                .field(
                    "Welcome channel",
                    cfg.welcome_channel
                        .map(|c| format!("<#{c}>"))
                        .unwrap_or_else(|| "not set".into()),
                    true,
                )
                .field(
                    "As embed",
                    if cfg.welcome_embed == 1 { "yes" } else { "no" },
                    true,
                )
                .field(
                    "Goodbye channel",
                    cfg.goodbye_channel
                        .map(|c| format!("<#{c}>"))
                        .unwrap_or_else(|| "not set".into()),
                    true,
                )
                .field("Message", text::truncate(&cfg.welcome_message, 500), false)
                .field("Goodbye", text::truncate(&cfg.goodbye_message, 300), false)
                .field("Auto-roles", roles(&cfg.autoroles), true)
                .field("Bot auto-roles", roles(&cfg.bot_autoroles), true)
                .field(
                    "Welcome DM",
                    if cfg.welcome_dm.as_deref().unwrap_or("").is_empty() {
                        "off".to_string()
                    } else {
                        "on".to_string()
                    },
                    true,
                )
                .footer(embed::footer(PLACEHOLDER_HELP)),
        ),
    )
    .await?;
    Ok(())
}

/// Turn welcome messages off.
#[poise::command(slash_command, rename = "off", guild_only)]
pub async fn welcome_off(ctx: Ctx<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    db::set_opt_i64(
        &ctx.data().db,
        "welcome_config",
        "welcome_channel",
        guild_id,
        None,
    )
    .await?;
    ctx.send(poise::CreateReply::default().embed(embed::ok("Welcome messages are off.")))
        .await?;
    Ok(())
}

// ===========================================================================
// Verification
// ===========================================================================

/// New-member verification gate.
#[poise::command(
    slash_command,
    guild_only,
    category = "Welcome",
    subcommands("verify_setup", "verify_post", "verify_off", "verify_manual"),
    check = "perms::admin_only"
)]
pub async fn verification(_ctx: Ctx<'_>) -> Result<(), Error> {
    Ok(())
}

/// Choose the verification channel and the role it grants.
#[poise::command(slash_command, rename = "setup", guild_only)]
pub async fn verify_setup(
    ctx: Ctx<'_>,
    #[description = "Channel where the verify panel lives"] channel: GuildChannel,
    #[description = "Role granted on success"] role: serenity::Role,
    #[description = "Minimum account age before they may verify, e.g. 1d"] min_age: Option<String>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let min_secs = min_age
        .as_deref()
        .and_then(time::parse_duration)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    db::set_opt_i64(
        &ctx.data().db,
        "welcome_config",
        "verify_channel",
        guild_id,
        Some(channel.id.get() as i64),
    )
    .await?;
    db::set_opt_i64(
        &ctx.data().db,
        "welcome_config",
        "verify_role",
        guild_id,
        Some(role.id.get() as i64),
    )
    .await?;
    db::set_i64(
        &ctx.data().db,
        "welcome_config",
        "verify_min_age",
        guild_id,
        min_secs,
    )
    .await?;

    ctx.send(
        poise::CreateReply::default().embed(
            embed::ok(format!(
                "Verification set up in <#{}>, granting <@&{}>.",
                channel.id.get(),
                role.id.get()
            ))
            .footer(embed::footer(
                "Now run /verification post to put the panel in the channel.",
            )),
        ),
    )
    .await?;
    Ok(())
}

/// Post the verification panel.
#[poise::command(slash_command, rename = "post", guild_only)]
pub async fn verify_post(
    ctx: Ctx<'_>,
    #[description = "Panel title"] title: Option<String>,
    #[description = "Panel body"] body: Option<String>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let cfg = db::welcome_config(&ctx.data().db, guild_id).await?;

    let Some(channel_id) = cfg.verify_channel else {
        ctx.send(
            poise::CreateReply::default()
                .embed(embed::err("Run `/verification setup` first.")),
        )
        .await?;
        return Ok(());
    };

    let panel = embed::titled(
        title.unwrap_or_else(|| "✅ Verification".to_string()),
        embed::SUCCESS,
    )
    .description(body.unwrap_or_else(|| {
        "Press the button below to confirm you have read the rules and get access to the server."
            .to_string()
    }));

    ChannelId::new(channel_id as u64)
        .send_message(
            ctx.http(),
            CreateMessage::new()
                .embed(panel)
                .components(vec![CreateActionRow::Buttons(vec![CreateButton::new(
                    "verify:start",
                )
                .label("Verify me")
                .style(serenity::ButtonStyle::Success)
                .emoji('✅')])]),
        )
        .await?;

    ctx.send(
        poise::CreateReply::default()
            .embed(embed::ok("Verification panel posted."))
            .ephemeral(true),
    )
    .await?;
    Ok(())
}

/// Switch verification off.
#[poise::command(slash_command, rename = "off", guild_only)]
pub async fn verify_off(ctx: Ctx<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    db::set_opt_i64(
        &ctx.data().db,
        "welcome_config",
        "verify_role",
        guild_id,
        None,
    )
    .await?;
    ctx.send(poise::CreateReply::default().embed(embed::ok("Verification is off.")))
        .await?;
    Ok(())
}

/// Verify someone by hand.
#[poise::command(slash_command, rename = "manual", guild_only)]
pub async fn verify_manual(
    ctx: Ctx<'_>,
    #[description = "Member to verify"] user: User,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap();
    let cfg = db::welcome_config(&ctx.data().db, guild_id.get() as i64).await?;

    let Some(role_id) = cfg.verify_role else {
        ctx.send(poise::CreateReply::default().embed(embed::err("No verification role is set.")))
            .await?;
        return Ok(());
    };

    grant_verification(ctx.serenity_context(), ctx.data(), guild_id, user.id, role_id).await?;
    ctx.send(
        poise::CreateReply::default()
            .embed(embed::ok(format!("{} is verified.", user.tag()))),
    )
    .await?;
    Ok(())
}

/// Shared by the button handler and `/verification manual`.
pub async fn grant_verification(
    sctx: &serenity::Context,
    data: &Data,
    guild_id: GuildId,
    user_id: serenity::UserId,
    role_id: i64,
) -> Result<(), Error> {
    compat::add_role(
        &sctx.http,
        guild_id,
        user_id,
        RoleId::new(role_id as u64),
    )
    .await?;

    let guild_i64 = guild_id.get() as i64;
    sqlx::query("UPDATE members SET verified = 1 WHERE guild_id = ? AND user_id = ?")
        .bind(guild_i64)
        .bind(user_id.get() as i64)
        .execute(&data.db)
        .await?;

    // Hand out the normal auto-roles now that they are through the gate.
    let cfg = db::welcome_config(&data.db, guild_i64).await?;
    for extra in db::csv_ids(&cfg.autoroles) {
        let _ = compat::add_role(&sctx.http, guild_id, user_id, RoleId::new(extra as u64)).await;
    }

    logs::send(
        sctx,
        &data.db,
        guild_i64,
        Log::Join,
        embed::titled("✅ Member verified", embed::SUCCESS)
            .description(format!("<@{}>", user_id.get())),
    )
    .await;
    Ok(())
}

pub fn commands() -> Vec<poise::Command<Data, Error>> {
    let mut all = vec![welcome(), verification()];
    all.extend(roles::commands());
    all.extend(invites::commands());
    all.extend(profiles::commands());
    all
}
