//! Server-level configuration: log channels, staff roles, prefix and a setup
//! overview that shows what still needs doing.

use poise::serenity_prelude as serenity;
use serenity::{GuildChannel, Role};

use crate::db;
use crate::logs;
use crate::state::{Ctx, Data, Error};
use crate::util::{embed, perms};

#[derive(Debug, poise::ChoiceParameter)]
pub enum LogKind {
    #[name = "moderation (cases, bans, warns)"]
    Mod,
    #[name = "messages (edits, deletions, purges)"]
    Message,
    #[name = "joins and leaves"]
    Join,
    #[name = "voice activity"]
    Voice,
    #[name = "server changes, lockdowns, anti-nuke"]
    Server,
}

impl LogKind {
    fn key(&self) -> &'static str {
        match self {
            LogKind::Mod => "mod",
            LogKind::Message => "message",
            LogKind::Join => "join",
            LogKind::Voice => "voice",
            LogKind::Server => "server",
        }
    }
}

#[derive(Debug, poise::ChoiceParameter)]
pub enum RoleKind {
    #[name = "admin (full access to configuration)"]
    Admin,
    #[name = "moderator (bans, kicks, warns)"]
    Mod,
    #[name = "staff (soft moderation, announcements)"]
    Staff,
}

impl RoleKind {
    fn column(&self) -> &'static str {
        match self {
            RoleKind::Admin => "admin_roles",
            RoleKind::Mod => "mod_roles",
            RoleKind::Staff => "staff_roles",
        }
    }
}

/// Configure the bot for this server.
#[poise::command(
    slash_command,
    guild_only,
    category = "Settings",
    subcommands(
        "config_overview",
        "config_log",
        "config_role",
        "config_prefix",
        "config_appeals",
        "config_muterole",
        "config_timezone",
        "config_export"
    ),
    check = "perms::admin_only"
)]
pub async fn config(_ctx: Ctx<'_>) -> Result<(), Error> {
    Ok(())
}

/// What is set up, and what is still missing.
#[poise::command(slash_command, rename = "overview", guild_only)]
pub async fn config_overview(ctx: Ctx<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let dbh = &ctx.data().db;

    let guild = db::guild_config(dbh, guild_id).await?;
    let welcome = db::welcome_config(dbh, guild_id).await?;
    let voice = db::voice_config(dbh, guild_id).await?;
    let leveling = db::leveling_config(dbh, guild_id).await?;
    let automod = db::automod_config(dbh, guild_id).await?;

    let channel = |value: Option<i64>| {
        value
            .map(|c| format!("<#{c}>"))
            .unwrap_or_else(|| "❌ not set".to_string())
    };
    let roles = |csv: &str| {
        let ids = db::csv_ids(csv);
        if ids.is_empty() {
            "❌ none".to_string()
        } else {
            ids.iter()
                .map(|r| format!("<@&{r}>"))
                .collect::<Vec<_>>()
                .join(", ")
        }
    };

    // Collect the things an admin still needs to do.
    let mut todo: Vec<&str> = Vec::new();
    if guild.modlog_channel.is_none() {
        todo.push("`/config log mod` — set the moderation log");
    }
    if db::csv_ids(&guild.mod_roles).is_empty() && db::csv_ids(&guild.admin_roles).is_empty() {
        todo.push("`/config role` — tell me which roles are staff");
    }
    if welcome.welcome_channel.is_none() {
        todo.push("`/welcome channel` — set up welcome messages");
    }
    if voice.j2c_channel.is_none() {
        todo.push("`/voice setup` — set up join-to-create rooms");
    }
    if automod.alert_channel.is_none() {
        todo.push("`/automod alerts` — pick an automod alert channel");
    }

    let mut e = embed::titled("⚙️ Server configuration", embed::INFO)
        .field(
            "Log channels",
            format!(
                "Moderation: {}\nMessages: {}\nJoins: {}\nVoice: {}\nServer: {}",
                channel(guild.modlog_channel),
                channel(guild.msglog_channel),
                channel(guild.joinlog_channel),
                channel(guild.voicelog_channel),
                channel(guild.serverlog_channel)
            ),
            false,
        )
        .field(
            "Staff roles",
            format!(
                "Admin: {}\nModerator: {}\nStaff: {}",
                roles(&guild.admin_roles),
                roles(&guild.mod_roles),
                roles(&guild.staff_roles)
            ),
            false,
        )
        .field(
            "Features",
            format!(
                "Welcome: {}\nVerification: {}\nJoin-to-create: {}\nXP: {}",
                if welcome.welcome_channel.is_some() { "🟢" } else { "🔴" },
                if welcome.verify_role.is_some() { "🟢" } else { "🔴" },
                if voice.j2c_channel.is_some() { "🟢" } else { "🔴" },
                if leveling.enabled == 1 { "🟢" } else { "🔴" }
            ),
            true,
        )
        .field(
            "Other",
            format!(
                "Prefix: `{}`\nTimezone: `{}`\nLocked down: {}\nAppeals: {}",
                guild.prefix,
                guild.timezone,
                if guild.locked_down == 1 { "yes ⚠️" } else { "no" },
                channel(guild.appeal_channel)
            ),
            true,
        );

    if todo.is_empty() {
        e = e.field("Next steps", "Everything is configured. 🎉", false);
    } else {
        e = e.field(
            format!("Next steps ({})", todo.len()),
            todo.iter()
                .map(|line| format!("• {line}"))
                .collect::<Vec<_>>()
                .join("\n"),
            false,
        );
    }

    ctx.send(poise::CreateReply::default().embed(e)).await?;
    Ok(())
}

/// Point a log kind at a channel.
#[poise::command(slash_command, rename = "log", guild_only)]
pub async fn config_log(
    ctx: Ctx<'_>,
    #[description = "Which log"] kind: LogKind,
    #[description = "Channel, or leave empty to switch this log off"] channel: Option<GuildChannel>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let Some(column) = logs::column_for_name(kind.key()) else {
        return Ok(());
    };

    db::set_opt_i64(
        &ctx.data().db,
        "guild_config",
        column,
        guild_id,
        channel.as_ref().map(|c| c.id.get() as i64),
    )
    .await?;

    let body = match &channel {
        Some(channel) => format!(
            "The **{}** log will go to <#{}>.",
            kind.key(),
            channel.id.get()
        ),
        None => format!(
            "The **{}** log is off (it will fall back to the moderation log).",
            kind.key()
        ),
    };
    ctx.send(poise::CreateReply::default().embed(embed::ok(body)))
        .await?;
    Ok(())
}

/// Add or remove a staff role.
#[poise::command(slash_command, rename = "role", guild_only)]
pub async fn config_role(
    ctx: Ctx<'_>,
    #[description = "Which tier"] tier: RoleKind,
    #[description = "The role"] role: Role,
    #[description = "Add it? (false removes it)"] add: Option<bool>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let column = tier.column();

    let cfg = db::guild_config(&ctx.data().db, guild_id).await?;
    let current = match tier {
        RoleKind::Admin => &cfg.admin_roles,
        RoleKind::Mod => &cfg.mod_roles,
        RoleKind::Staff => &cfg.staff_roles,
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
        "guild_config",
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
        "`{column}` is now: {listed}"
    ))))
    .await?;
    Ok(())
}

/// Change the text prefix used for the few prefix commands.
#[poise::command(slash_command, rename = "prefix", guild_only)]
pub async fn config_prefix(
    ctx: Ctx<'_>,
    #[description = "New prefix, e.g. ! or ?"] prefix: String,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let prefix = prefix.trim().to_string();

    if prefix.is_empty() || prefix.len() > 5 {
        ctx.send(
            poise::CreateReply::default()
                .embed(embed::err("The prefix must be 1 to 5 characters.")),
        )
        .await?;
        return Ok(());
    }

    db::set_text(
        &ctx.data().db,
        "guild_config",
        "prefix",
        guild_id,
        prefix.clone(),
    )
    .await?;
    ctx.send(poise::CreateReply::default().embed(embed::ok(format!(
        "The prefix is now `{prefix}`."
    ))))
    .await?;
    Ok(())
}

/// Where moderation appeals are sent.
#[poise::command(slash_command, rename = "appeals", guild_only)]
pub async fn config_appeals(
    ctx: Ctx<'_>,
    #[description = "Appeals channel, or leave empty to use the mod log"] channel: Option<
        GuildChannel,
    >,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    db::set_opt_i64(
        &ctx.data().db,
        "guild_config",
        "appeal_channel",
        guild_id,
        channel.as_ref().map(|c| c.id.get() as i64),
    )
    .await?;

    let body = match channel {
        Some(channel) => format!("Appeals will go to <#{}>.", channel.id.get()),
        None => "Appeals will go to the moderation log.".to_string(),
    };
    ctx.send(poise::CreateReply::default().embed(embed::ok(body)))
        .await?;
    Ok(())
}

/// Set a mute role, for servers that prefer it over Discord timeouts.
#[poise::command(slash_command, rename = "muterole", guild_only)]
pub async fn config_muterole(
    ctx: Ctx<'_>,
    #[description = "The mute role"] role: Option<Role>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    db::set_opt_i64(
        &ctx.data().db,
        "guild_config",
        "mute_role",
        guild_id,
        role.as_ref().map(|r| r.id.get() as i64),
    )
    .await?;

    let body = match role {
        Some(role) => format!(
            "Mute role set to <@&{}>. Note that `/timeout` uses Discord's own timeout, which \
             does not need a role.",
            role.id.get()
        ),
        None => "Mute role cleared.".to_string(),
    };
    ctx.send(poise::CreateReply::default().embed(embed::ok(body)))
        .await?;
    Ok(())
}

/// The server's timezone, used for daily and weekly jobs.
#[poise::command(slash_command, rename = "timezone", guild_only)]
pub async fn config_timezone(
    ctx: Ctx<'_>,
    #[description = "IANA name, e.g. Europe/London"] timezone: String,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;

    if timezone.parse::<chrono_tz::Tz>().is_err() {
        ctx.send(poise::CreateReply::default().embed(embed::err(
            "I do not recognise that timezone. Use an IANA name such as `Europe/London`.",
        )))
        .await?;
        return Ok(());
    }

    db::set_text(
        &ctx.data().db,
        "guild_config",
        "timezone",
        guild_id,
        timezone.clone(),
    )
    .await?;
    ctx.send(poise::CreateReply::default().embed(embed::ok(format!(
        "Timezone set to `{timezone}`."
    ))))
    .await?;
    Ok(())
}

/// Export this server's settings as JSON, for backup.
#[poise::command(slash_command, rename = "export", guild_only, ephemeral)]
pub async fn config_export(ctx: Ctx<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let dbh = &ctx.data().db;

    let guild = db::guild_config(dbh, guild_id).await?;
    let welcome = db::welcome_config(dbh, guild_id).await?;
    let automod = db::automod_config(dbh, guild_id).await?;
    let leveling = db::leveling_config(dbh, guild_id).await?;
    let voice = db::voice_config(dbh, guild_id).await?;

    let badwords: Vec<String> =
        sqlx::query_scalar("SELECT word FROM badwords WHERE guild_id = ?")
            .bind(guild_id)
            .fetch_all(dbh)
            .await?;
    let rewards: Vec<(i64, i64)> =
        sqlx::query_as("SELECT level, role_id FROM level_rewards WHERE guild_id = ?")
            .bind(guild_id)
            .fetch_all(dbh)
            .await?;

    // Written by hand rather than derived, so the export stays readable and
    // never accidentally includes something sensitive.
    let export = serde_json::json!({
        "exported_at": crate::util::time::sql_now(),
        "guild_id": guild_id.to_string(),
        "guild": {
            "prefix": guild.prefix,
            "timezone": guild.timezone,
            "modlog_channel": guild.modlog_channel,
            "msglog_channel": guild.msglog_channel,
            "joinlog_channel": guild.joinlog_channel,
            "voicelog_channel": guild.voicelog_channel,
            "serverlog_channel": guild.serverlog_channel,
            "admin_roles": guild.admin_roles,
            "mod_roles": guild.mod_roles,
            "staff_roles": guild.staff_roles,
        },
        "welcome": {
            "channel": welcome.welcome_channel,
            "message": welcome.welcome_message,
            "goodbye_channel": welcome.goodbye_channel,
            "goodbye_message": welcome.goodbye_message,
            "autoroles": welcome.autoroles,
            "verify_role": welcome.verify_role,
        },
        "automod": {
            "antispam": automod.antispam,
            "antiflood": automod.antiflood,
            "antiraid": automod.antiraid,
            "antinuke": automod.antinuke,
            "antiphish": automod.antiphish,
            "antiinvite": automod.antiinvite,
            "mention_limit": automod.mention_limit,
            "alert_channel": automod.alert_channel,
            "badwords": badwords,
        },
        "leveling": {
            "enabled": leveling.enabled,
            "xp_min": leveling.xp_min,
            "xp_max": leveling.xp_max,
            "cooldown": leveling.cooldown,
            "voice_xp_per_min": leveling.voice_xp_per_min,
            "prestige_level": leveling.prestige_level,
            "rewards": rewards,
        },
        "voice": {
            "j2c_channel": voice.j2c_channel,
            "j2c_category": voice.j2c_category,
            "default_name": voice.default_name,
            "afk_channel": voice.afk_channel,
        },
    });

    let pretty = serde_json::to_string_pretty(&export)?;
    ctx.send(
        poise::CreateReply::default()
            .embed(embed::ok("Here is your configuration backup."))
            .attachment(serenity::CreateAttachment::bytes(
                pretty.into_bytes(),
                format!("config-{guild_id}.json"),
            ))
            .ephemeral(true),
    )
    .await?;
    Ok(())
}

/// Register or clear slash commands. Bot owner only.
///
/// Prefix-only on purpose: Discord allows at most 100 top-level slash commands,
/// and a slash version could not bootstrap registration anyway. Run it as
/// `!register` or `@bot register`.
#[poise::command(prefix_command, owners_only, hide_in_help)]
pub async fn register(ctx: Ctx<'_>) -> Result<(), Error> {
    poise::builtins::register_application_commands_buttons(ctx).await?;
    Ok(())
}

pub fn commands() -> Vec<poise::Command<Data, Error>> {
    vec![config(), register()]
}
