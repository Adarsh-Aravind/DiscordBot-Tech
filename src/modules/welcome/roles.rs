//! Self-service roles: button menus, dropdown menus, rules acceptance, plus
//! staff role and nickname management.

use poise::serenity_prelude as serenity;
use serde::{Deserialize, Serialize};
use serenity::{
    ComponentInteraction, CreateActionRow, CreateButton, CreateInteractionResponse,
    CreateInteractionResponseMessage, CreateMessage, CreateSelectMenu, CreateSelectMenuKind,
    CreateSelectMenuOption, GuildChannel, ReactionType, Role, RoleId, User,
};

use crate::compat;
use crate::db;
use crate::state::{Ctx, Data, Error};
use crate::util::{embed, perms, text};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MenuRole {
    pub role_id: i64,
    pub label: String,
    #[serde(default)]
    pub emoji: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

fn parse_roles(raw: &str) -> Vec<MenuRole> {
    serde_json::from_str(raw).unwrap_or_default()
}

/// Build the message components for a menu.
pub fn build_components(menu_id: i64, style: &str, roles: &[MenuRole], max: i64) -> Vec<CreateActionRow> {
    if roles.is_empty() {
        return Vec::new();
    }

    if style == "dropdown" {
        let options: Vec<CreateSelectMenuOption> = roles
            .iter()
            .take(25)
            .map(|r| {
                let mut option =
                    CreateSelectMenuOption::new(text::truncate(&r.label, 90), r.role_id.to_string());
                if let Some(description) = &r.description {
                    option = option.description(text::truncate(description, 90));
                }
                if let Some(emoji) = &r.emoji {
                    if let Ok(reaction) = emoji.parse::<ReactionType>() {
                        option = option.emoji(reaction);
                    }
                }
                option
            })
            .collect();

        let count = options.len() as u8;
        let menu = CreateSelectMenu::new(
            format!("rolemenu_select:{menu_id}"),
            CreateSelectMenuKind::String { options },
        )
        .placeholder("Pick your roles")
        .min_values(0)
        .max_values(count.min(max.clamp(1, 25) as u8));

        return vec![CreateActionRow::SelectMenu(menu)];
    }

    // Buttons: five per row, up to five rows.
    let mut rows = Vec::new();
    for chunk in roles.iter().take(25).collect::<Vec<_>>().chunks(5) {
        let buttons: Vec<CreateButton> = chunk
            .iter()
            .map(|r| {
                let mut button = CreateButton::new(format!("rolemenu:{menu_id}:{}", r.role_id))
                    .label(text::truncate(&r.label, 80))
                    .style(serenity::ButtonStyle::Secondary);
                if let Some(emoji) = &r.emoji {
                    if let Ok(reaction) = emoji.parse::<ReactionType>() {
                        button = button.emoji(reaction);
                    }
                }
                button
            })
            .collect();
        rows.push(CreateActionRow::Buttons(buttons));
    }
    rows
}

// ===========================================================================
// Commands
// ===========================================================================

#[derive(Debug, poise::ChoiceParameter)]
pub enum MenuStyle {
    #[name = "buttons"]
    Buttons,
    #[name = "dropdown"]
    Dropdown,
}

/// Self-assignable role menus.
#[poise::command(
    slash_command,
    guild_only,
    category = "Roles",
    subcommands(
        "rolemenu_create",
        "rolemenu_addrole",
        "rolemenu_removerole",
        "rolemenu_post",
        "rolemenu_list",
        "rolemenu_delete"
    ),
    check = "perms::admin_only"
)]
pub async fn rolemenu(_ctx: Ctx<'_>) -> Result<(), Error> {
    Ok(())
}

/// Create an empty menu.
#[poise::command(slash_command, rename = "create", guild_only)]
pub async fn rolemenu_create(
    ctx: Ctx<'_>,
    #[description = "Short name used in the other commands"] name: String,
    #[description = "Embed title"] title: String,
    #[description = "Embed description"] description: Option<String>,
    #[description = "Buttons or dropdown"] style: Option<MenuStyle>,
    #[description = "Maximum roles a member may pick (dropdown only)"] max_choices: Option<i64>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let style_key = match style.unwrap_or(MenuStyle::Buttons) {
        MenuStyle::Buttons => "button",
        MenuStyle::Dropdown => "dropdown",
    };

    let result = sqlx::query(
        "INSERT INTO role_menus (guild_id, channel_id, name, title, description, style, max_choices)
         VALUES (?, 0, ?, ?, ?, ?, ?)
         ON CONFLICT(guild_id, name) DO UPDATE SET
            title = excluded.title,
            description = excluded.description,
            style = excluded.style,
            max_choices = excluded.max_choices",
    )
    .bind(guild_id)
    .bind(name.trim())
    .bind(&title)
    .bind(description.unwrap_or_default())
    .bind(style_key)
    .bind(max_choices.unwrap_or(25).clamp(1, 25))
    .execute(&ctx.data().db)
    .await;

    match result {
        Ok(_) => {
            ctx.send(poise::CreateReply::default().embed(
                embed::ok(format!("Menu **{}** created.", name.trim())).footer(embed::footer(
                    "Add roles with /rolemenu addrole, then publish it with /rolemenu post.",
                )),
            ))
            .await?;
        }
        Err(why) => {
            ctx.send(
                poise::CreateReply::default()
                    .embed(embed::err(format!("Could not create that menu: {why}"))),
            )
            .await?;
        }
    }
    Ok(())
}

/// Add a role to a menu.
#[poise::command(slash_command, rename = "addrole", guild_only)]
pub async fn rolemenu_addrole(
    ctx: Ctx<'_>,
    #[description = "Menu name"] menu: String,
    #[description = "Role to offer"] role: Role,
    #[description = "Button or option label"] label: Option<String>,
    #[description = "Emoji"] emoji: Option<String>,
    #[description = "Short description (dropdown only)"] description: Option<String>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;

    let found: Option<(i64, String)> =
        sqlx::query_as("SELECT id, roles FROM role_menus WHERE guild_id = ? AND name = ?")
            .bind(guild_id)
            .bind(menu.trim())
            .fetch_optional(&ctx.data().db)
            .await?;

    let Some((menu_id, raw)) = found else {
        ctx.send(poise::CreateReply::default().embed(embed::err("No menu with that name.")))
            .await?;
        return Ok(());
    };

    // Refuse roles the bot cannot actually assign - better now than on a click.
    if let Err(why) = can_assign(ctx.serenity_context(), ctx.guild_id().unwrap(), role.id) {
        ctx.send(poise::CreateReply::default().embed(embed::err(why)))
            .await?;
        return Ok(());
    }

    let mut roles = parse_roles(&raw);
    roles.retain(|r| r.role_id != role.id.get() as i64);
    if roles.len() >= 25 {
        ctx.send(
            poise::CreateReply::default()
                .embed(embed::err("A menu can hold at most 25 roles.")),
        )
        .await?;
        return Ok(());
    }
    roles.push(MenuRole {
        role_id: role.id.get() as i64,
        label: label.unwrap_or_else(|| role.name.clone()),
        emoji,
        description,
    });

    sqlx::query("UPDATE role_menus SET roles = ? WHERE id = ?")
        .bind(serde_json::to_string(&roles)?)
        .bind(menu_id)
        .execute(&ctx.data().db)
        .await?;

    ctx.send(poise::CreateReply::default().embed(embed::ok(format!(
        "Added <@&{}> to **{}** ({} role(s) total). Re-run `/rolemenu post` to refresh the message.",
        role.id.get(),
        menu.trim(),
        roles.len()
    ))))
    .await?;
    Ok(())
}

/// Remove a role from a menu.
#[poise::command(slash_command, rename = "removerole", guild_only)]
pub async fn rolemenu_removerole(
    ctx: Ctx<'_>,
    #[description = "Menu name"] menu: String,
    #[description = "Role to drop"] role: Role,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let found: Option<(i64, String)> =
        sqlx::query_as("SELECT id, roles FROM role_menus WHERE guild_id = ? AND name = ?")
            .bind(guild_id)
            .bind(menu.trim())
            .fetch_optional(&ctx.data().db)
            .await?;

    let Some((menu_id, raw)) = found else {
        ctx.send(poise::CreateReply::default().embed(embed::err("No menu with that name.")))
            .await?;
        return Ok(());
    };

    let mut roles = parse_roles(&raw);
    let before = roles.len();
    roles.retain(|r| r.role_id != role.id.get() as i64);

    sqlx::query("UPDATE role_menus SET roles = ? WHERE id = ?")
        .bind(serde_json::to_string(&roles)?)
        .bind(menu_id)
        .execute(&ctx.data().db)
        .await?;

    let body = if before == roles.len() {
        embed::warn("That role was not in the menu.")
    } else {
        embed::ok("Removed. Re-run `/rolemenu post` to refresh the message.")
    };
    ctx.send(poise::CreateReply::default().embed(body)).await?;
    Ok(())
}

/// Publish (or re-publish) a menu.
#[poise::command(slash_command, rename = "post", guild_only)]
pub async fn rolemenu_post(
    ctx: Ctx<'_>,
    #[description = "Menu name"] menu: String,
    #[description = "Channel, defaults to this one"] channel: Option<GuildChannel>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let target = channel.map(|c| c.id).unwrap_or_else(|| ctx.channel_id());

    let found: Option<(i64, String, String, String, i64, String)> = sqlx::query_as(
        "SELECT id, title, description, style, max_choices, roles FROM role_menus
         WHERE guild_id = ? AND name = ?",
    )
    .bind(guild_id)
    .bind(menu.trim())
    .fetch_optional(&ctx.data().db)
    .await?;

    let Some((menu_id, title, description, style, max_choices, raw)) = found else {
        ctx.send(poise::CreateReply::default().embed(embed::err("No menu with that name.")))
            .await?;
        return Ok(());
    };

    let roles = parse_roles(&raw);
    if roles.is_empty() {
        ctx.send(
            poise::CreateReply::default()
                .embed(embed::err("Add some roles first with `/rolemenu addrole`.")),
        )
        .await?;
        return Ok(());
    }

    let listing = roles
        .iter()
        .map(|r| {
            let emoji = r.emoji.clone().unwrap_or_default();
            match &r.description {
                Some(d) if !d.is_empty() => {
                    format!("{emoji} <@&{}> — {}", r.role_id, text::truncate(d, 80))
                }
                _ => format!("{emoji} <@&{}>", r.role_id),
            }
        })
        .collect::<Vec<_>>()
        .join("\n");

    let body = if description.trim().is_empty() {
        listing
    } else {
        format!("{description}\n\n{listing}")
    };

    let message = target
        .send_message(
            ctx.http(),
            CreateMessage::new()
                .embed(embed::titled(title, embed::BRAND).description(body))
                .components(build_components(menu_id, &style, &roles, max_choices)),
        )
        .await?;

    sqlx::query("UPDATE role_menus SET channel_id = ?, message_id = ? WHERE id = ?")
        .bind(target.get() as i64)
        .bind(message.id.get() as i64)
        .bind(menu_id)
        .execute(&ctx.data().db)
        .await?;

    ctx.send(
        poise::CreateReply::default()
            .embed(embed::ok(format!("Posted in <#{}>.", target.get())))
            .ephemeral(true),
    )
    .await?;
    Ok(())
}

/// List the menus in this server.
#[poise::command(slash_command, rename = "list", guild_only)]
pub async fn rolemenu_list(ctx: Ctx<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let rows: Vec<(String, String, String, Option<i64>)> = sqlx::query_as(
        "SELECT name, style, roles, message_id FROM role_menus WHERE guild_id = ? ORDER BY name",
    )
    .bind(guild_id)
    .fetch_all(&ctx.data().db)
    .await?;

    let body = if rows.is_empty() {
        "No role menus yet.".to_string()
    } else {
        rows.iter()
            .map(|(name, style, raw, message_id)| {
                format!(
                    "**{name}** · `{style}` · {} role(s) · {}",
                    parse_roles(raw).len(),
                    if message_id.unwrap_or(0) > 0 {
                        "published"
                    } else {
                        "not posted"
                    }
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    ctx.send(
        poise::CreateReply::default()
            .embed(embed::titled("Role menus", embed::INFO).description(body)),
    )
    .await?;
    Ok(())
}

/// Delete a menu. The posted message stays but stops working.
#[poise::command(slash_command, rename = "delete", guild_only)]
pub async fn rolemenu_delete(
    ctx: Ctx<'_>,
    #[description = "Menu name"] menu: String,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let result = sqlx::query("DELETE FROM role_menus WHERE guild_id = ? AND name = ?")
        .bind(guild_id)
        .bind(menu.trim())
        .execute(&ctx.data().db)
        .await?;
    let body = if result.rows_affected() == 0 {
        embed::err("No menu with that name.")
    } else {
        embed::ok("Menu deleted.")
    };
    ctx.send(poise::CreateReply::default().embed(body)).await?;
    Ok(())
}

/// Can the bot hand out this role?
fn can_assign(
    sctx: &serenity::Context,
    guild_id: serenity::GuildId,
    role_id: RoleId,
) -> Result<(), String> {
    let positions = perms::role_positions(sctx, guild_id);
    let Some(role_pos) = positions.get(&role_id).copied() else {
        return Ok(());
    };

    let bot = perms::bot_id(sctx);
    let Some(guild) = guild_id.to_guild_cached(&sctx.cache) else {
        return Ok(());
    };
    let Some(member) = guild.members.get(&bot) else {
        return Ok(());
    };
    let bot_top = member
        .roles
        .iter()
        .filter_map(|r| positions.get(r))
        .copied()
        .max()
        .unwrap_or(-1);

    if role_pos >= bot_top {
        return Err(
            "That role is above mine, so I cannot assign it. Move my role higher in Server Settings → Roles."
                .to_string(),
        );
    }
    Ok(())
}

// ===========================================================================
// Interaction handling
// ===========================================================================

/// Handle a role-menu button or dropdown. Returns true when it was ours.
pub async fn handle_component(
    sctx: &serenity::Context,
    data: &Data,
    interaction: &ComponentInteraction,
) -> Result<bool, Error> {
    let custom_id = interaction.data.custom_id.clone();
    let Some(guild_id) = interaction.guild_id else {
        return Ok(false);
    };

    // ---- single button ---------------------------------------------------
    if let Some(rest) = custom_id.strip_prefix("rolemenu:") {
        let mut parts = rest.split(':');
        let _menu_id = parts.next().unwrap_or("");
        let Some(role_id) = parts.next().and_then(|s| s.parse::<u64>().ok()) else {
            return Ok(true);
        };

        let role = RoleId::new(role_id);
        let has = interaction
            .member
            .as_ref()
            .map(|m| m.roles.contains(&role))
            .unwrap_or(false);

        let message = if has {
            match compat::remove_role(&sctx.http, guild_id, interaction.user.id, role).await {
                Ok(()) => format!("Removed <@&{role_id}>."),
                Err(_) => "I could not remove that role - my role may be too low.".to_string(),
            }
        } else {
            match compat::add_role(&sctx.http, guild_id, interaction.user.id, role).await {
                Ok(()) => format!("Added <@&{role_id}>."),
                Err(_) => "I could not add that role - my role may be too low.".to_string(),
            }
        };

        interaction
            .create_response(
                &sctx.http,
                CreateInteractionResponse::Message(
                    CreateInteractionResponseMessage::new()
                        .content(message)
                        .ephemeral(true),
                ),
            )
            .await?;
        return Ok(true);
    }

    // ---- dropdown --------------------------------------------------------
    if let Some(menu_id) = custom_id.strip_prefix("rolemenu_select:") {
        let menu_id: i64 = menu_id.parse().unwrap_or(0);
        let chosen: Vec<u64> = match &interaction.data.kind {
            serenity::ComponentInteractionDataKind::StringSelect { values } => {
                values.iter().filter_map(|v| v.parse().ok()).collect()
            }
            _ => Vec::new(),
        };

        let raw: Option<String> = sqlx::query_scalar("SELECT roles FROM role_menus WHERE id = ?")
            .bind(menu_id)
            .fetch_optional(&data.db)
            .await?;
        let all: Vec<u64> = parse_roles(raw.as_deref().unwrap_or("[]"))
            .iter()
            .map(|r| r.role_id as u64)
            .collect();

        let current: Vec<u64> = interaction
            .member
            .as_ref()
            .map(|m| m.roles.iter().map(|r| r.get()).collect())
            .unwrap_or_default();

        let mut added = Vec::new();
        let mut removed = Vec::new();

        for role_id in &all {
            let should_have = chosen.contains(role_id);
            let has = current.contains(role_id);
            let role = RoleId::new(*role_id);
            if should_have && !has {
                if compat::add_role(&sctx.http, guild_id, interaction.user.id, role)
                    .await
                    .is_ok()
                {
                    added.push(format!("<@&{role_id}>"));
                }
            } else if !should_have && has {
                if compat::remove_role(&sctx.http, guild_id, interaction.user.id, role)
                    .await
                    .is_ok()
                {
                    removed.push(format!("<@&{role_id}>"));
                }
            }
        }

        let mut body = String::new();
        if !added.is_empty() {
            body.push_str(&format!("Added: {}\n", added.join(", ")));
        }
        if !removed.is_empty() {
            body.push_str(&format!("Removed: {}", removed.join(", ")));
        }
        if body.is_empty() {
            body.push_str("Nothing changed.");
        }

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

    // ---- verification ----------------------------------------------------
    if custom_id == "verify:start" {
        let guild_i64 = guild_id.get() as i64;
        let cfg = db::welcome_config(&data.db, guild_i64).await?;

        let Some(role_id) = cfg.verify_role else {
            interaction
                .create_response(
                    &sctx.http,
                    CreateInteractionResponse::Message(
                        CreateInteractionResponseMessage::new()
                            .content("Verification is not configured here yet.")
                            .ephemeral(true),
                    ),
                )
                .await?;
            return Ok(true);
        };

        // Account-age gate.
        if cfg.verify_min_age > 0 {
            let age = chrono::Utc::now().timestamp() - interaction.user.id.created_at().unix_timestamp();
            if age < cfg.verify_min_age {
                let wait = cfg.verify_min_age - age;
                interaction
                    .create_response(
                        &sctx.http,
                        CreateInteractionResponse::Message(
                            CreateInteractionResponseMessage::new()
                                .content(format!(
                                    "Your account is too new to verify here. Try again in about **{}**.",
                                    crate::util::time::humanize(wait)
                                ))
                                .ephemeral(true),
                        ),
                    )
                    .await?;
                return Ok(true);
            }
        }

        let already = interaction
            .member
            .as_ref()
            .map(|m| m.roles.contains(&RoleId::new(role_id as u64)))
            .unwrap_or(false);

        let body = if already {
            "You are already verified.".to_string()
        } else {
            match super::grant_verification(sctx, data, guild_id, interaction.user.id, role_id)
                .await
            {
                Ok(()) => "✅ You are verified. Welcome in!".to_string(),
                Err(_) => "Something went wrong. Please ping a moderator.".to_string(),
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

    // ---- rules acceptance ------------------------------------------------
    if custom_id == "rules:accept" {
        let guild_i64 = guild_id.get() as i64;
        let cfg = db::welcome_config(&data.db, guild_i64).await?;

        let body = match cfg.rules_role {
            Some(role_id) => {
                let role = RoleId::new(role_id as u64);
                let has = interaction
                    .member
                    .as_ref()
                    .map(|m| m.roles.contains(&role))
                    .unwrap_or(false);
                if has {
                    "You have already accepted the rules.".to_string()
                } else if compat::add_role(&sctx.http, guild_id, interaction.user.id, role)
                    .await
                    .is_ok()
                {
                    "Thanks for accepting the rules. Enjoy the server!".to_string()
                } else {
                    "I could not give you the role - please ping a moderator.".to_string()
                }
            }
            None => "No rules role is configured here.".to_string(),
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

    Ok(false)
}

// ===========================================================================
// Rules acceptance setup
// ===========================================================================

/// Rules message with an accept button.
#[poise::command(
    slash_command,
    guild_only,
    category = "Roles",
    subcommands("rules_setup", "rules_post"),
    check = "perms::admin_only"
)]
pub async fn rules(_ctx: Ctx<'_>) -> Result<(), Error> {
    Ok(())
}

/// Pick the channel and the role that accepting the rules grants.
#[poise::command(slash_command, rename = "setup", guild_only)]
pub async fn rules_setup(
    ctx: Ctx<'_>,
    #[description = "Rules channel"] channel: GuildChannel,
    #[description = "Role granted on accept"] role: Role,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    db::set_opt_i64(
        &ctx.data().db,
        "welcome_config",
        "rules_channel",
        guild_id,
        Some(channel.id.get() as i64),
    )
    .await?;
    db::set_opt_i64(
        &ctx.data().db,
        "welcome_config",
        "rules_role",
        guild_id,
        Some(role.id.get() as i64),
    )
    .await?;
    ctx.send(
        poise::CreateReply::default().embed(embed::ok(format!(
            "Rules acceptance set up in <#{}>, granting <@&{}>. Post the panel with `/rules post`.",
            channel.id.get(),
            role.id.get()
        ))),
    )
    .await?;
    Ok(())
}

/// Post the rules panel with its accept button.
#[poise::command(slash_command, rename = "post", guild_only)]
pub async fn rules_post(
    ctx: Ctx<'_>,
    #[description = "The rules themselves. Use \\n for new lines."] body: String,
    #[description = "Title"] title: Option<String>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let cfg = db::welcome_config(&ctx.data().db, guild_id).await?;
    let channel = cfg
        .rules_channel
        .map(|c| serenity::ChannelId::new(c as u64))
        .unwrap_or_else(|| ctx.channel_id());

    let message = channel
        .send_message(
            ctx.http(),
            CreateMessage::new()
                .embed(
                    embed::titled(
                        title.unwrap_or_else(|| "📜 Server rules".to_string()),
                        embed::BRAND,
                    )
                    .description(body.replace("\\n", "\n")),
                )
                .components(vec![CreateActionRow::Buttons(vec![CreateButton::new(
                    "rules:accept",
                )
                .label("I accept the rules")
                .style(serenity::ButtonStyle::Success)
                .emoji('📜')])]),
        )
        .await?;

    db::set_opt_i64(
        &ctx.data().db,
        "welcome_config",
        "rules_message",
        guild_id,
        Some(message.id.get() as i64),
    )
    .await?;

    ctx.send(
        poise::CreateReply::default()
            .embed(embed::ok("Rules posted."))
            .ephemeral(true),
    )
    .await?;
    Ok(())
}

// ===========================================================================
// Staff role + nickname tools
// ===========================================================================

/// Add or remove a role from a member.
#[poise::command(
    slash_command,
    guild_only,
    category = "Roles",
    subcommands("role_add", "role_remove"),
    required_permissions = "MANAGE_ROLES",
    default_member_permissions = "MANAGE_ROLES"
)]
pub async fn role(_ctx: Ctx<'_>) -> Result<(), Error> {
    Ok(())
}

#[poise::command(slash_command, rename = "add", guild_only)]
pub async fn role_add(
    ctx: Ctx<'_>,
    #[description = "Member"] user: User,
    #[description = "Role to add"] role: Role,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap();
    if let Err(why) = can_assign(ctx.serenity_context(), guild_id, role.id) {
        ctx.send(poise::CreateReply::default().embed(embed::err(why)))
            .await?;
        return Ok(());
    }
    compat::add_role(&ctx.serenity_context().http, guild_id, user.id, role.id).await?;
    ctx.send(poise::CreateReply::default().embed(embed::ok(format!(
        "Gave <@&{}> to {}.",
        role.id.get(),
        user.tag()
    ))))
    .await?;
    Ok(())
}

#[poise::command(slash_command, rename = "remove", guild_only)]
pub async fn role_remove(
    ctx: Ctx<'_>,
    #[description = "Member"] user: User,
    #[description = "Role to remove"] role: Role,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap();
    compat::remove_role(&ctx.serenity_context().http, guild_id, user.id, role.id).await?;
    ctx.send(poise::CreateReply::default().embed(embed::ok(format!(
        "Removed <@&{}> from {}.",
        role.id.get(),
        user.tag()
    ))))
    .await?;
    Ok(())
}

/// Change or clear a member's nickname.
#[poise::command(
    slash_command,
    guild_only,
    category = "Roles",
    required_permissions = "MANAGE_NICKNAMES",
    default_member_permissions = "MANAGE_NICKNAMES"
)]
pub async fn nick(
    ctx: Ctx<'_>,
    #[description = "Member"] user: User,
    #[description = "New nickname, leave empty to reset"] nickname: Option<String>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap();
    let value = nickname.clone().unwrap_or_default();

    guild_id
        .edit_member(
            ctx.http(),
            user.id,
            serenity::EditMember::new()
                .nickname(value.as_str())
                .audit_log_reason(&format!("Set by {}", ctx.author().tag())),
        )
        .await?;

    // Keep a record so staff can see who renamed whom.
    sqlx::query(
        "INSERT INTO kv_settings (guild_id, key, value) VALUES (?, ?, ?)
         ON CONFLICT(guild_id, key) DO UPDATE SET value = excluded.value",
    )
    .bind(guild_id.get() as i64)
    .bind(format!("nick.{}", user.id.get()))
    .bind(format!("{}|{}", value, ctx.author().id.get()))
    .execute(&ctx.data().db)
    .await?;

    let body = match nickname {
        Some(name) => format!("{} is now **{}**.", user.tag(), text::escape_md(&name)),
        None => format!("Reset the nickname for {}.", user.tag()),
    };
    ctx.send(poise::CreateReply::default().embed(embed::ok(body)))
        .await?;
    Ok(())
}

pub fn commands() -> Vec<poise::Command<Data, Error>> {
    vec![rolemenu(), rules(), role(), nick()]
}
