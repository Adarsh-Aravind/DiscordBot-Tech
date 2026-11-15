//! Utility commands: information lookups, small tools and conveniences.

use chrono::Utc;
use poise::serenity_prelude as serenity;
use serenity::{GuildChannel, Role, User};

use crate::state::{Ctx, Data, Error};
use crate::util::rankcard::thousands;
use crate::util::{embed, perms, text, time};

// ===========================================================================
// Information
// ===========================================================================

/// Someone's avatar, full size.
#[poise::command(slash_command, prefix_command, category = "Utility")]
pub async fn avatar(
    ctx: Ctx<'_>,
    #[description = "Whose avatar, defaults to you"] user: Option<User>,
    #[description = "Show the server-specific avatar if they have one"] server: Option<bool>,
) -> Result<(), Error> {
    let target = user.unwrap_or_else(|| ctx.author().clone());

    // Prefer the per-guild avatar when asked for and available.
    let url = if server.unwrap_or(false) {
        ctx.guild_id()
            .and_then(|g| {
                g.to_guild_cached(&ctx.serenity_context().cache)
                    .and_then(|guild| guild.members.get(&target.id).and_then(|m| m.avatar_url()))
            })
            .unwrap_or_else(|| target.face())
    } else {
        target.face()
    };

    ctx.send(
        poise::CreateReply::default().embed(
            embed::titled(format!("{}'s avatar", target.name), embed::BRAND)
                .image(url.clone())
                .description(format!("[open original]({url})")),
        ),
    )
    .await?;
    Ok(())
}

/// Someone's profile banner.
#[poise::command(slash_command, prefix_command, category = "Utility")]
pub async fn banner(
    ctx: Ctx<'_>,
    #[description = "Whose banner, defaults to you"] user: Option<User>,
) -> Result<(), Error> {
    let target = user.unwrap_or_else(|| ctx.author().clone());

    // Banners are not in the gateway payload, so fetch the full user object.
    let fetched = ctx.http().get_user(target.id).await.unwrap_or(target.clone());

    match fetched.banner_url() {
        Some(url) => {
            ctx.send(
                poise::CreateReply::default().embed(
                    embed::titled(format!("{}'s banner", fetched.name), embed::BRAND)
                        .image(url.clone())
                        .description(format!("[open original]({url})")),
                ),
            )
            .await?;
        }
        None => {
            ctx.send(
                poise::CreateReply::default()
                    .embed(embed::warn("They do not have a banner set.")),
            )
            .await?;
        }
    }
    Ok(())
}

/// Details about a member.
#[poise::command(slash_command, prefix_command, guild_only, category = "Utility")]
pub async fn userinfo(
    ctx: Ctx<'_>,
    #[description = "Who to look up, defaults to you"] user: Option<User>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap();
    let target = user.unwrap_or_else(|| ctx.author().clone());
    let user_i64 = target.id.get() as i64;

    let member = guild_id.member(ctx.serenity_context(), target.id).await.ok();

    let roles = member
        .as_ref()
        .map(|m| {
            let mut ids: Vec<String> =
                m.roles.iter().map(|r| format!("<@&{}>", r.get())).collect();
            ids.reverse();
            if ids.is_empty() {
                "none".to_string()
            } else {
                text::truncate(&ids.join(" "), 900)
            }
        })
        .unwrap_or_else(|| "not in this server".to_string());

    let joined = member
        .as_ref()
        .and_then(|m| m.joined_at)
        .map(|t| format!("<t:{}:F> (<t:{}:R>)", t.unix_timestamp(), t.unix_timestamp()))
        .unwrap_or_else(|| "unknown".to_string());

    let nickname = member
        .as_ref()
        .and_then(|m| m.nick.clone())
        .unwrap_or_else(|| "none".to_string());

    let boosting = member
        .as_ref()
        .and_then(|m| m.premium_since)
        .map(|t| format!("since <t:{}:R>", t.unix_timestamp()))
        .unwrap_or_else(|| "no".to_string());

    let timed_out = member
        .as_ref()
        .and_then(|m| m.communication_disabled_until)
        .filter(|t| t.unix_timestamp() > Utc::now().timestamp())
        .map(|t| format!("until <t:{}:R>", t.unix_timestamp()));

    // Our own records.
    let level: Option<(i64, i64, i64)> = sqlx::query_as(
        "SELECT level, total_xp, messages FROM levels WHERE guild_id = ? AND user_id = ?",
    )
    .bind(guild_id.get() as i64)
    .bind(user_i64)
    .fetch_optional(&ctx.data().db)
    .await?;

    let cases: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM mod_cases WHERE guild_id = ? AND target_id = ?")
            .bind(guild_id.get() as i64)
            .bind(user_i64)
            .fetch_one(&ctx.data().db)
            .await?;

    let mut e = embed::titled(target.tag(), embed::INFO)
        .thumbnail(target.face())
        .field("ID", format!("`{}`", target.id.get()), true)
        .field("Bot", if target.bot { "yes" } else { "no" }, true)
        .field("Nickname", nickname, true)
        .field(
            "Account created",
            format!(
                "<t:{}:F> (<t:{}:R>)",
                target.id.created_at().unix_timestamp(),
                target.id.created_at().unix_timestamp()
            ),
            false,
        )
        .field("Joined server", joined, false)
        .field("Boosting", boosting, true);

    if let Some((level, total_xp, messages)) = level {
        e = e.field(
            "Level",
            format!("{level} ({} XP, {} messages)", thousands(total_xp), thousands(messages)),
            true,
        );
    }
    if cases > 0 {
        e = e.field("Moderation cases", cases.to_string(), true);
    }
    if let Some(until) = timed_out {
        e = e.field("⚠️ Timed out", until, false);
    }
    e = e.field("Roles", roles, false);

    ctx.send(poise::CreateReply::default().embed(e)).await?;
    Ok(())
}

/// Details about this server.
#[poise::command(slash_command, prefix_command, guild_only, category = "Utility")]
pub async fn serverinfo(ctx: Ctx<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap();

    // One cache read, everything copied out.
    struct Snapshot {
        name: String,
        owner: u64,
        members: i64,
        bots: i64,
        text: i64,
        voice: i64,
        categories: i64,
        roles: i64,
        emojis: i64,
        stickers: i64,
        boosts: i64,
        tier: String,
        icon: Option<String>,
        banner: Option<String>,
        description: Option<String>,
        verification: String,
    }

    // Build the snapshot in its own statement so the cache guard (which is not
    // `Send`) is dropped before any `.await` below.
    let snapshot = guild_id
        .to_guild_cached(&ctx.serenity_context().cache)
        .map(|guild| Snapshot {
            name: guild.name.clone(),
            owner: guild.owner_id.get(),
            members: guild.member_count as i64,
            bots: guild.members.values().filter(|m| m.user.bot).count() as i64,
            text: guild
                .channels
                .values()
                .filter(|c| c.kind == serenity::ChannelType::Text)
                .count() as i64,
            voice: guild
                .channels
                .values()
                .filter(|c| {
                    matches!(c.kind, serenity::ChannelType::Voice | serenity::ChannelType::Stage)
                })
                .count() as i64,
            categories: guild
                .channels
                .values()
                .filter(|c| c.kind == serenity::ChannelType::Category)
                .count() as i64,
            roles: guild.roles.len() as i64,
            emojis: guild.emojis.len() as i64,
            stickers: guild.stickers.len() as i64,
            boosts: guild.premium_subscription_count.unwrap_or(0) as i64,
            tier: format!("{:?}", guild.premium_tier),
            icon: guild.icon_url(),
            banner: guild.banner_url(),
            description: guild.description.clone(),
            verification: format!("{:?}", guild.verification_level),
        });
    let Some(snapshot) = snapshot else {
        ctx.send(poise::CreateReply::default().embed(embed::err(
            "I do not have this server cached yet. Try again in a moment.",
        )))
        .await?;
        return Ok(());
    };

    let created = guild_id.created_at().unix_timestamp();
    let tracked: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM members WHERE guild_id = ?")
        .bind(guild_id.get() as i64)
        .fetch_one(&ctx.data().db)
        .await?;

    let mut e = embed::titled(snapshot.name.clone(), embed::INFO)
        .field("Owner", format!("<@{}>", snapshot.owner), true)
        .field("Server ID", format!("`{}`", guild_id.get()), true)
        .field(
            "Created",
            format!("<t:{created}:D> (<t:{created}:R>)"),
            true,
        )
        .field(
            "Members",
            format!(
                "**{}** total\n{} people · {} bots",
                thousands(snapshot.members),
                thousands(snapshot.members - snapshot.bots),
                snapshot.bots
            ),
            true,
        )
        .field(
            "Channels",
            format!(
                "{} text · {} voice\n{} categories",
                snapshot.text, snapshot.voice, snapshot.categories
            ),
            true,
        )
        .field(
            "Other",
            format!(
                "{} roles · {} emojis\n{} stickers",
                snapshot.roles, snapshot.emojis, snapshot.stickers
            ),
            true,
        )
        .field(
            "Boosts",
            format!("{} ({})", snapshot.boosts, snapshot.tier),
            true,
        )
        .field("Verification", snapshot.verification, true)
        .field("Members I track", thousands(tracked), true);

    if let Some(description) = snapshot.description {
        e = e.description(description);
    }
    if let Some(icon) = snapshot.icon {
        e = e.thumbnail(icon);
    }
    if let Some(banner) = snapshot.banner {
        e = e.image(banner);
    }

    ctx.send(poise::CreateReply::default().embed(e)).await?;
    Ok(())
}

/// This server's icon.
#[poise::command(slash_command, prefix_command, guild_only, category = "Utility")]
pub async fn servericon(ctx: Ctx<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap();
    let icon = guild_id
        .to_guild_cached(&ctx.serenity_context().cache)
        .and_then(|g| g.icon_url());

    match icon {
        Some(url) => {
            ctx.send(
                poise::CreateReply::default().embed(
                    embed::titled("Server icon", embed::BRAND)
                        .image(url.clone())
                        .description(format!("[open original]({url})")),
                ),
            )
            .await?;
        }
        None => {
            ctx.send(poise::CreateReply::default().embed(embed::warn("This server has no icon.")))
                .await?;
        }
    }
    Ok(())
}

/// Details about a role.
#[poise::command(slash_command, guild_only, category = "Utility")]
pub async fn roleinfo(
    ctx: Ctx<'_>,
    #[description = "Role to look up"] role: Role,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap();

    let holders = guild_id
        .to_guild_cached(&ctx.serenity_context().cache)
        .map(|guild| {
            guild
                .members
                .values()
                .filter(|m| m.roles.contains(&role.id))
                .count() as i64
        })
        .unwrap_or(0);

    let key_perms: Vec<&str> = [
        ("Administrator", role.permissions.administrator()),
        ("Manage Server", role.permissions.manage_guild()),
        ("Manage Roles", role.permissions.manage_roles()),
        ("Manage Channels", role.permissions.manage_channels()),
        ("Ban Members", role.permissions.ban_members()),
        ("Kick Members", role.permissions.kick_members()),
        ("Manage Messages", role.permissions.manage_messages()),
        ("Mention Everyone", role.permissions.mention_everyone()),
        ("Timeout Members", role.permissions.moderate_members()),
    ]
    .iter()
    .filter(|(_, has)| *has)
    .map(|(name, _)| *name)
    .collect();

    ctx.send(
        poise::CreateReply::default().embed(
            serenity::CreateEmbed::new()
                .colour(role.colour)
                .title(format!("Role · {}", role.name))
                .field("ID", format!("`{}`", role.id.get()), true)
                .field("Colour", format!("`#{:06X}`", role.colour.0), true)
                .field("Position", role.position.to_string(), true)
                .field("Members with it", thousands(holders), true)
                .field("Mentionable", if role.mentionable { "yes" } else { "no" }, true)
                .field("Shown separately", if role.hoist { "yes" } else { "no" }, true)
                .field("Managed by an app", if role.managed { "yes" } else { "no" }, true)
                .field(
                    "Key permissions",
                    if key_perms.is_empty() {
                        "none of note".to_string()
                    } else {
                        key_perms.join(", ")
                    },
                    false,
                )
                .field(
                    "Created",
                    format!("<t:{}:R>", role.id.created_at().unix_timestamp()),
                    true,
                ),
        ),
    )
    .await?;
    Ok(())
}

/// Details about a channel.
#[poise::command(slash_command, guild_only, category = "Utility")]
pub async fn channelinfo(
    ctx: Ctx<'_>,
    #[description = "Channel, defaults to this one"] channel: Option<GuildChannel>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap();

    let channel = match channel {
        Some(channel) => channel,
        None => {
            let found = guild_id
                .to_guild_cached(&ctx.serenity_context().cache)
                .and_then(|g| g.channels.get(&ctx.channel_id()).cloned());
            match found {
                Some(channel) => channel,
                None => {
                    ctx.send(
                        poise::CreateReply::default()
                            .embed(embed::err("I could not read this channel from my cache.")),
                    )
                    .await?;
                    return Ok(());
                }
            }
        }
    };

    let mut e = embed::titled(format!("Channel · #{}", channel.name), embed::INFO)
        .field("ID", format!("`{}`", channel.id.get()), true)
        .field("Type", format!("{:?}", channel.kind), true)
        .field("Position", channel.position.to_string(), true)
        .field("NSFW", if channel.nsfw { "yes" } else { "no" }, true)
        .field(
            "Created",
            format!("<t:{}:R>", channel.id.created_at().unix_timestamp()),
            true,
        )
        .field(
            "Permission overwrites",
            channel.permission_overwrites.len().to_string(),
            true,
        );

    if let Some(topic) = &channel.topic {
        if !topic.is_empty() {
            e = e.description(text::truncate(topic, 1000));
        }
    }
    if channel.rate_limit_per_user.unwrap_or(0) > 0 {
        e = e.field(
            "Slowmode",
            time::humanize(channel.rate_limit_per_user.unwrap_or(0) as i64),
            true,
        );
    }
    if let Some(parent) = channel.parent_id {
        e = e.field("Category", format!("<#{}>", parent.get()), true);
    }
    if let Some(limit) = channel.user_limit {
        if limit > 0 {
            e = e.field("User limit", limit.to_string(), true);
        }
    }

    ctx.send(poise::CreateReply::default().embed(e)).await?;
    Ok(())
}

/// What the bot can do here.
#[poise::command(slash_command, guild_only, category = "Utility")]
pub async fn permissions(
    ctx: Ctx<'_>,
    #[description = "Check a member instead of me"] user: Option<User>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap();
    let target = match &user {
        Some(user) => user.id,
        None => perms::bot_id(ctx.serenity_context()),
    };

    let member = match guild_id.member(ctx.serenity_context(), target).await {
        Ok(member) => member,
        Err(_) => {
            ctx.send(
                poise::CreateReply::default()
                    .embed(embed::err("I could not find that member here.")),
            )
            .await?;
            return Ok(());
        }
    };

    let resolved = member.permissions(ctx.serenity_context()).ok();
    let Some(permissions) = resolved else {
        ctx.send(poise::CreateReply::default().embed(embed::err(
            "I could not resolve permissions. My cache may not be warm yet.",
        )))
        .await?;
        return Ok(());
    };

    let checks: Vec<(&str, bool)> = vec![
        ("Administrator", permissions.administrator()),
        ("Manage Server", permissions.manage_guild()),
        ("Manage Roles", permissions.manage_roles()),
        ("Manage Channels", permissions.manage_channels()),
        ("Manage Messages", permissions.manage_messages()),
        ("Ban Members", permissions.ban_members()),
        ("Kick Members", permissions.kick_members()),
        ("Timeout Members", permissions.moderate_members()),
        ("Manage Nicknames", permissions.manage_nicknames()),
        ("Manage Webhooks", permissions.manage_webhooks()),
        ("View Audit Log", permissions.view_audit_log()),
        ("Send Messages", permissions.send_messages()),
        ("Embed Links", permissions.embed_links()),
        ("Attach Files", permissions.attach_files()),
        ("Add Reactions", permissions.add_reactions()),
        ("Move Members", permissions.move_members()),
        ("Mute Members", permissions.mute_members()),
        ("Create Invites", permissions.create_instant_invite()),
    ];

    let body = checks
        .iter()
        .map(|(name, has)| format!("{} {name}", if *has { "✅" } else { "❌" }))
        .collect::<Vec<_>>()
        .join("\n");

    // Flag the ones this bot genuinely needs.
    let missing: Vec<&str> = [
        ("Manage Roles", permissions.manage_roles()),
        ("Manage Channels", permissions.manage_channels()),
        ("Ban Members", permissions.ban_members()),
        ("Kick Members", permissions.kick_members()),
        ("Timeout Members", permissions.moderate_members()),
        ("Manage Messages", permissions.manage_messages()),
        ("Manage Server", permissions.manage_guild()),
        ("View Audit Log", permissions.view_audit_log()),
    ]
    .iter()
    .filter(|(_, has)| !*has)
    .map(|(name, _)| *name)
    .collect();

    let mut e = embed::titled(
        format!("Permissions · {}", member.user.tag()),
        embed::INFO,
    )
    .description(body);

    if user.is_none() && !missing.is_empty() {
        e = e.field(
            "⚠️ Missing for full functionality",
            missing.join(", "),
            false,
        );
    }

    ctx.send(poise::CreateReply::default().embed(e)).await?;
    Ok(())
}

/// About the bot.
#[poise::command(slash_command, prefix_command, category = "Utility")]
pub async fn botinfo(ctx: Ctx<'_>) -> Result<(), Error> {
    let data = ctx.data();
    let uptime = (Utc::now() - data.started_at).num_seconds();

    let guilds = ctx.serenity_context().cache.guild_count();
    let users = ctx.serenity_context().cache.user_count();
    let commands = ctx.framework().options().commands.len();

    let total_members: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM members")
        .fetch_one(&data.db)
        .await
        .unwrap_or(0);
    let total_cases: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM mod_cases")
        .fetch_one(&data.db)
        .await
        .unwrap_or(0);
    let total_xp: i64 = sqlx::query_scalar("SELECT COALESCE(SUM(total_xp), 0) FROM levels")
        .fetch_one(&data.db)
        .await
        .unwrap_or(0);
    let commands_run = data
        .commands_run
        .load(std::sync::atomic::Ordering::Relaxed);

    ctx.send(
        poise::CreateReply::default().embed(
            embed::titled("🤖 MrTechTrivia bot", embed::BRAND)
                .description(
                    "A community bot for the MrTechTrivia Discord, written in Rust with \
                     serenity and poise.",
                )
                .field("Uptime", time::humanize_long(uptime), true)
                .field("Servers", guilds.to_string(), true)
                .field("Cached users", thousands(users as i64), true)
                .field("Commands", commands.to_string(), true)
                .field("Commands run this session", thousands(commands_run as i64), true)
                .field("Members tracked", thousands(total_members), true)
                .field("Moderation cases", thousands(total_cases), true)
                .field("XP awarded", thousands(total_xp), true)
                .field(
                    "Built with",
                    "Rust · serenity 0.12 · poise 0.6 · SQLite",
                    true,
                )
                .footer(embed::footer("/help lists every command")),
        ),
    )
    .await?;
    Ok(())
}

/// Round-trip latency.
#[poise::command(slash_command, prefix_command, category = "Utility")]
pub async fn ping(ctx: Ctx<'_>) -> Result<(), Error> {
    let start = std::time::Instant::now();
    let reply = ctx
        .send(poise::CreateReply::default().embed(embed::titled("🏓 Pinging...", embed::MUTED)))
        .await?;
    let elapsed = start.elapsed().as_millis();

    let gateway = ctx.ping().await.as_millis();
    let gateway_text = if gateway == 0 {
        "measuring".to_string()
    } else {
        format!("{gateway} ms")
    };

    let db_start = std::time::Instant::now();
    let _: i64 = sqlx::query_scalar("SELECT 1")
        .fetch_one(&ctx.data().db)
        .await
        .unwrap_or(0);
    let db_elapsed = db_start.elapsed().as_micros();

    reply
        .edit(
            ctx,
            poise::CreateReply::default().embed(
                embed::titled("🏓 Pong", embed::SUCCESS)
                    .field("REST round trip", format!("{elapsed} ms"), true)
                    .field("Gateway", gateway_text, true)
                    .field("Database", format!("{:.1} ms", db_elapsed as f64 / 1000.0), true),
            ),
        )
        .await?;
    Ok(())
}

/// How long the bot has been running.
#[poise::command(slash_command, prefix_command, category = "Utility")]
pub async fn uptime(ctx: Ctx<'_>) -> Result<(), Error> {
    let started = ctx.data().started_at;
    let seconds = (Utc::now() - started).num_seconds();

    ctx.send(
        poise::CreateReply::default().embed(
            embed::titled("⏱️ Uptime", embed::INFO)
                .description(format!("**{}**", time::humanize_long(seconds)))
                .field("Started", time::full_and_relative(started), false),
        ),
    )
    .await?;
    Ok(())
}

/// Turn a date and time into a Discord timestamp.
#[poise::command(slash_command, category = "Utility")]
pub async fn timestamp(
    ctx: Ctx<'_>,
    #[description = "A delay like 2h, or a UTC time like 2026-03-01 18:00"] when: String,
) -> Result<(), Error> {
    let Some(moment) = time::parse_when(&when) else {
        ctx.send(poise::CreateReply::default().embed(embed::err(
            "I could not read that. Try `2h`, or `2026-03-01 18:00`.",
        )))
        .await?;
        return Ok(());
    };

    let unix = moment.timestamp();
    let styles = [
        ('f', "short date and time"),
        ('F', "long date and time"),
        ('d', "short date"),
        ('D', "long date"),
        ('t', "short time"),
        ('T', "long time"),
        ('R', "relative"),
    ];

    let body = styles
        .iter()
        .map(|(style, label)| {
            format!("`<t:{unix}:{style}>` → <t:{unix}:{style}>  *({label})*")
        })
        .collect::<Vec<_>>()
        .join("\n");

    ctx.send(
        poise::CreateReply::default().embed(
            embed::titled("🕐 Discord timestamps", embed::INFO)
                .description(body)
                .footer(embed::footer(
                    "Copy a code block; everyone sees it in their own timezone.",
                )),
        ),
    )
    .await?;
    Ok(())
}

// ===========================================================================
// Tools
// ===========================================================================

/// Work out a sum.
#[poise::command(slash_command, prefix_command, category = "Utility", aliases("calc"))]
pub async fn calculator(
    ctx: Ctx<'_>,
    #[description = "For example: (12 + 5) * 3 / 2 ^ 2"] expression: String,
) -> Result<(), Error> {
    match calc::evaluate(&expression) {
        Ok(value) => {
            let rendered = if value.fract().abs() < 1e-9 {
                thousands(value as i64)
            } else {
                format!("{value}")
            };
            ctx.send(
                poise::CreateReply::default().embed(
                    embed::titled("🧮 Calculator", embed::INFO)
                        .field("Expression", format!("`{}`", text::truncate(&expression, 200)), false)
                        .field("Result", format!("**{rendered}**"), false),
                ),
            )
            .await?;
        }
        Err(why) => {
            ctx.send(
                poise::CreateReply::default()
                    .embed(embed::err(format!("I could not work that out: {why}"))),
            )
            .await?;
        }
    }
    Ok(())
}

/// A small, dependency-free expression evaluator (shunting-yard).
mod calc {
    #[derive(Debug, Clone, Copy, PartialEq)]
    enum Token {
        Number(f64),
        Op(char),
        Open,
        Close,
    }

    fn tokenize(input: &str) -> Result<Vec<Token>, String> {
        let mut tokens = Vec::new();
        let chars: Vec<char> = input.chars().collect();
        let mut i = 0;

        while i < chars.len() {
            let ch = chars[i];
            match ch {
                ' ' | ',' | '_' => i += 1,
                '0'..='9' | '.' => {
                    let start = i;
                    while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                        i += 1;
                    }
                    let literal: String = chars[start..i].iter().collect();
                    let value: f64 = literal
                        .parse()
                        .map_err(|_| format!("`{literal}` is not a number"))?;
                    tokens.push(Token::Number(value));
                }
                '+' | '-' | '*' | '/' | '^' | '%' => {
                    // A leading minus (or one after an operator) is a sign, not a subtraction.
                    let unary = ch == '-'
                        && matches!(
                            tokens.last(),
                            None | Some(Token::Op(_)) | Some(Token::Open)
                        );
                    if unary {
                        tokens.push(Token::Number(0.0));
                    }
                    tokens.push(Token::Op(ch));
                    i += 1;
                }
                'x' | 'X' => {
                    tokens.push(Token::Op('*'));
                    i += 1;
                }
                '(' | '[' => {
                    tokens.push(Token::Open);
                    i += 1;
                }
                ')' | ']' => {
                    tokens.push(Token::Close);
                    i += 1;
                }
                other => return Err(format!("I do not understand `{other}`")),
            }
        }
        if tokens.is_empty() {
            return Err("there is nothing to calculate".into());
        }
        Ok(tokens)
    }

    fn precedence(op: char) -> u8 {
        match op {
            '+' | '-' => 1,
            '*' | '/' | '%' => 2,
            '^' => 3,
            _ => 0,
        }
    }

    fn apply(op: char, a: f64, b: f64) -> Result<f64, String> {
        Ok(match op {
            '+' => a + b,
            '-' => a - b,
            '*' => a * b,
            '/' => {
                if b == 0.0 {
                    return Err("division by zero".into());
                }
                a / b
            }
            '%' => {
                if b == 0.0 {
                    return Err("division by zero".into());
                }
                a % b
            }
            '^' => a.powf(b),
            _ => return Err(format!("unknown operator `{op}`")),
        })
    }

    pub fn evaluate(input: &str) -> Result<f64, String> {
        if input.len() > 200 {
            return Err("that expression is too long".into());
        }
        let tokens = tokenize(input)?;

        let mut output: Vec<f64> = Vec::new();
        let mut operators: Vec<Token> = Vec::new();

        fn pop_apply(output: &mut Vec<f64>, op: char) -> Result<(), String> {
            let b = output.pop().ok_or("the expression is incomplete")?;
            let a = output.pop().ok_or("the expression is incomplete")?;
            output.push(apply(op, a, b)?);
            Ok(())
        }

        for token in tokens {
            match token {
                Token::Number(value) => output.push(value),
                Token::Op(op) => {
                    while let Some(Token::Op(top)) = operators.last().copied() {
                        // `^` is right associative, the rest are left associative.
                        let takes_precedence = if op == '^' {
                            precedence(top) > precedence(op)
                        } else {
                            precedence(top) >= precedence(op)
                        };
                        if takes_precedence {
                            operators.pop();
                            pop_apply(&mut output, top)?;
                        } else {
                            break;
                        }
                    }
                    operators.push(Token::Op(op));
                }
                Token::Open => operators.push(Token::Open),
                Token::Close => {
                    loop {
                        match operators.pop() {
                            Some(Token::Op(op)) => pop_apply(&mut output, op)?,
                            Some(Token::Open) => break,
                            _ => return Err("unbalanced brackets".into()),
                        }
                    }
                }
            }
        }

        while let Some(token) = operators.pop() {
            match token {
                Token::Op(op) => pop_apply(&mut output, op)?,
                _ => return Err("unbalanced brackets".into()),
            }
        }

        if output.len() != 1 {
            return Err("the expression is incomplete".into());
        }
        let result = output[0];
        if !result.is_finite() {
            return Err("the result is not a finite number".into());
        }
        Ok(result)
    }
}

/// Translate text. Needs a LibreTranslate-compatible endpoint configured.
#[poise::command(slash_command, category = "Utility")]
pub async fn translate(
    ctx: Ctx<'_>,
    #[description = "Text to translate"] text_to_translate: String,
    #[description = "Target language code, e.g. en, es, hi, de"] to: String,
    #[description = "Source language code, or leave empty to auto-detect"] from: Option<String>,
) -> Result<(), Error> {
    let Some(endpoint) = ctx.data().config.translate_url.clone() else {
        ctx.send(
            poise::CreateReply::default().embed(
                embed::err(
                    "Translation is not configured. Set `TRANSLATE_API_URL` to a \
                     LibreTranslate-compatible endpoint and restart the bot.",
                )
                .footer(embed::footer("For example https://libretranslate.com/translate")),
            ),
        )
        .await?;
        return Ok(());
    };

    ctx.defer().await?;

    let mut body = serde_json::json!({
        "q": text_to_translate,
        "source": from.clone().unwrap_or_else(|| "auto".to_string()),
        "target": to.trim().to_lowercase(),
        "format": "text",
    });
    if let Some(key) = &ctx.data().config.translate_key {
        body["api_key"] = serde_json::Value::String(key.clone());
    }

    let response = ctx.data().http.post(&endpoint).json(&body).send().await;

    let reply = match response {
        Ok(response) if response.status().is_success() => {
            match response.json::<serde_json::Value>().await {
                Ok(json) => {
                    let translated = json
                        .get("translatedText")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    if translated.is_empty() {
                        embed::err("The endpoint returned nothing useful.")
                    } else {
                        embed::titled("🌐 Translation", embed::INFO)
                            .field(
                                format!("From ({})", from.unwrap_or_else(|| "auto".into())),
                                text::truncate(&text_to_translate, 1000),
                                false,
                            )
                            .field(
                                format!("To ({to})"),
                                text::truncate(&translated, 1000),
                                false,
                            )
                    }
                }
                Err(why) => embed::err(format!("I could not read the reply: {why}")),
            }
        }
        Ok(response) => embed::err(format!(
            "The translation service replied {}.",
            response.status()
        )),
        Err(why) => embed::err(format!("The translation service is unreachable: {why}")),
    };

    ctx.send(poise::CreateReply::default().embed(reply)).await?;
    Ok(())
}

/// Current weather for a place. Uses Open-Meteo, no API key needed.
#[poise::command(slash_command, category = "Utility")]
pub async fn weather(
    ctx: Ctx<'_>,
    #[description = "Town or city"] location: String,
) -> Result<(), Error> {
    ctx.defer().await?;
    let http = &ctx.data().http;

    let geo_url = format!(
        "https://geocoding-api.open-meteo.com/v1/search?name={}&count=1&language=en&format=json",
        urlencoding::encode(location.trim())
    );

    let geo: serde_json::Value = match http.get(&geo_url).send().await {
        Ok(response) => match response.json().await {
            Ok(json) => json,
            Err(why) => {
                ctx.send(
                    poise::CreateReply::default()
                        .embed(embed::err(format!("I could not read the location reply: {why}"))),
                )
                .await?;
                return Ok(());
            }
        },
        Err(why) => {
            ctx.send(
                poise::CreateReply::default()
                    .embed(embed::err(format!("The weather service is unreachable: {why}"))),
            )
            .await?;
            return Ok(());
        }
    };

    let Some(place) = geo.get("results").and_then(|r| r.get(0)) else {
        ctx.send(
            poise::CreateReply::default()
                .embed(embed::err(format!("I could not find **{location}**."))),
        )
        .await?;
        return Ok(());
    };

    let latitude = place.get("latitude").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let longitude = place.get("longitude").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let name = place.get("name").and_then(|v| v.as_str()).unwrap_or("there");
    let country = place
        .get("country")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let forecast_url = format!(
        "https://api.open-meteo.com/v1/forecast?latitude={latitude}&longitude={longitude}\
         &current=temperature_2m,relative_humidity_2m,apparent_temperature,precipitation,weather_code,wind_speed_10m\
         &daily=temperature_2m_max,temperature_2m_min&timezone=auto&forecast_days=1"
    );

    let forecast: serde_json::Value = match http.get(&forecast_url).send().await {
        Ok(response) => response.json().await.unwrap_or_default(),
        Err(why) => {
            ctx.send(
                poise::CreateReply::default()
                    .embed(embed::err(format!("The weather service is unreachable: {why}"))),
            )
            .await?;
            return Ok(());
        }
    };

    let current = forecast.get("current").cloned().unwrap_or_default();
    let number = |key: &str| current.get(key).and_then(|v| v.as_f64());

    let code = number("weather_code").unwrap_or(-1.0) as i64;
    let (icon, description) = weather_code(code);

    let daily = forecast.get("daily").cloned().unwrap_or_default();
    let high = daily
        .get("temperature_2m_max")
        .and_then(|v| v.get(0))
        .and_then(|v| v.as_f64());
    let low = daily
        .get("temperature_2m_min")
        .and_then(|v| v.get(0))
        .and_then(|v| v.as_f64());

    let mut e = embed::titled(
        format!("{icon} Weather in {name}{}", if country.is_empty() { String::new() } else { format!(", {country}") }),
        embed::INFO,
    )
    .description(format!("**{description}**"));

    if let Some(temperature) = number("temperature_2m") {
        e = e.field("Temperature", format!("{temperature:.1} °C"), true);
    }
    if let Some(feels) = number("apparent_temperature") {
        e = e.field("Feels like", format!("{feels:.1} °C"), true);
    }
    if let (Some(high), Some(low)) = (high, low) {
        e = e.field("Today", format!("{low:.0} °C to {high:.0} °C"), true);
    }
    if let Some(humidity) = number("relative_humidity_2m") {
        e = e.field("Humidity", format!("{humidity:.0}%"), true);
    }
    if let Some(wind) = number("wind_speed_10m") {
        e = e.field("Wind", format!("{wind:.1} km/h"), true);
    }
    if let Some(rain) = number("precipitation") {
        e = e.field("Precipitation", format!("{rain:.1} mm"), true);
    }

    ctx.send(
        poise::CreateReply::default()
            .embed(e.footer(embed::footer("Data from Open-Meteo"))),
    )
    .await?;
    Ok(())
}

/// WMO weather codes, as used by Open-Meteo.
fn weather_code(code: i64) -> (&'static str, &'static str) {
    match code {
        0 => ("☀️", "Clear sky"),
        1 => ("🌤️", "Mainly clear"),
        2 => ("⛅", "Partly cloudy"),
        3 => ("☁️", "Overcast"),
        45 | 48 => ("🌫️", "Fog"),
        51 | 53 | 55 => ("🌦️", "Drizzle"),
        56 | 57 => ("🌧️", "Freezing drizzle"),
        61 => ("🌦️", "Light rain"),
        63 => ("🌧️", "Rain"),
        65 => ("🌧️", "Heavy rain"),
        66 | 67 => ("🌧️", "Freezing rain"),
        71 => ("🌨️", "Light snow"),
        73 => ("🌨️", "Snow"),
        75 => ("❄️", "Heavy snow"),
        77 => ("❄️", "Snow grains"),
        80 | 81 | 82 => ("🌦️", "Rain showers"),
        85 | 86 => ("🌨️", "Snow showers"),
        95 => ("⛈️", "Thunderstorm"),
        96 | 99 => ("⛈️", "Thunderstorm with hail"),
        _ => ("🌡️", "Unknown conditions"),
    }
}

/// Build an embed from parameters.
#[poise::command(
    slash_command,
    rename = "embed",
    guild_only,
    category = "Utility",
    check = "perms::staff_only"
)]
pub async fn embed_builder(
    ctx: Ctx<'_>,
    #[rename = "title"]
    #[description = "Embed title"]
    embed_title: Option<String>,
    #[description = "Embed body. Use \\n for new lines."] description: Option<String>,
    #[description = "Hex colour, e.g. #5865F2"] colour: Option<String>,
    #[description = "Image URL"] image: Option<String>,
    #[description = "Thumbnail URL"] thumbnail: Option<String>,
    #[description = "Footer text"] footer: Option<String>,
    #[description = "Channel to post in, defaults to this one"] channel: Option<GuildChannel>,
) -> Result<(), Error> {
    if embed_title.is_none() && description.is_none() {
        ctx.send(
            poise::CreateReply::default()
                .embed(embed::err("Give me at least a title or a description.")),
        )
        .await?;
        return Ok(());
    }

    let mut built = serenity::CreateEmbed::new().colour(
        colour
            .as_deref()
            .and_then(embed::parse_colour)
            .unwrap_or_else(|| embed::colour(embed::BRAND)),
    );
    if let Some(title) = embed_title {
        built = built.title(title);
    }
    if let Some(description) = description {
        built = built.description(description.replace("\\n", "\n"));
    }
    if let Some(image) = image {
        built = built.image(image);
    }
    if let Some(thumbnail) = thumbnail {
        built = built.thumbnail(thumbnail);
    }
    if let Some(footer) = footer {
        built = built.footer(embed::footer(footer));
    }

    let target = channel.map(|c| c.id).unwrap_or_else(|| ctx.channel_id());
    target
        .send_message(ctx.http(), serenity::CreateMessage::new().embed(built))
        .await?;

    ctx.send(
        poise::CreateReply::default()
            .embed(embed::ok(format!("Posted in <#{}>.", target.get())))
            .ephemeral(true),
    )
    .await?;
    Ok(())
}

// ===========================================================================
// AFK and snipe
// ===========================================================================

/// Mark yourself as away. The bot tells anyone who pings you.
#[poise::command(slash_command, prefix_command, guild_only, category = "Utility")]
pub async fn afk(
    ctx: Ctx<'_>,
    #[description = "Why are you away?"] reason: Option<String>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let reason = reason.unwrap_or_else(|| "AFK".to_string());

    sqlx::query(
        "INSERT INTO afk (guild_id, user_id, reason, since, pings) VALUES (?, ?, ?, ?, 0)
         ON CONFLICT(guild_id, user_id) DO UPDATE SET
            reason = excluded.reason, since = excluded.since, pings = 0",
    )
    .bind(guild_id)
    .bind(ctx.author().id.get() as i64)
    .bind(text::truncate(&reason, 200))
    .bind(time::sql_now())
    .execute(&ctx.data().db)
    .await?;

    ctx.send(
        poise::CreateReply::default().embed(embed::ok(format!(
            "You are AFK: **{}**\nI will let people know when they mention you.",
            text::escape_md(&reason)
        ))),
    )
    .await?;
    Ok(())
}

/// Message handler for AFK: clear the author's status, answer mentions.
pub async fn handle_afk(
    sctx: &serenity::Context,
    data: &Data,
    message: &serenity::Message,
) -> Result<(), Error> {
    let Some(guild_id) = message.guild_id else {
        return Ok(());
    };
    let guild_i64 = guild_id.get() as i64;

    // The author is back.
    let own: Option<(String, i64)> = sqlx::query_as(
        "SELECT since, pings FROM afk WHERE guild_id = ? AND user_id = ?",
    )
    .bind(guild_i64)
    .bind(message.author.id.get() as i64)
    .fetch_optional(&data.db)
    .await?;

    if let Some((since, pings)) = own {
        sqlx::query("DELETE FROM afk WHERE guild_id = ? AND user_id = ?")
            .bind(guild_i64)
            .bind(message.author.id.get() as i64)
            .execute(&data.db)
            .await?;

        let away_for = time::from_sql(&since)
            .map(|t| time::humanize((Utc::now() - t).num_seconds()))
            .unwrap_or_else(|| "a while".to_string());

        let note = if pings > 0 {
            format!(
                "Welcome back, you were away for **{away_for}** and were mentioned **{pings}** time(s)."
            )
        } else {
            format!("Welcome back, you were away for **{away_for}**.")
        };
        let _ = message.reply(&sctx.http, note).await;
    }

    // Anyone mentioned who is AFK.
    for mentioned in &message.mentions {
        if mentioned.id == message.author.id {
            continue;
        }
        let found: Option<(String, String)> = sqlx::query_as(
            "SELECT reason, since FROM afk WHERE guild_id = ? AND user_id = ?",
        )
        .bind(guild_i64)
        .bind(mentioned.id.get() as i64)
        .fetch_optional(&data.db)
        .await?;

        if let Some((reason, since)) = found {
            let _ = sqlx::query(
                "UPDATE afk SET pings = pings + 1 WHERE guild_id = ? AND user_id = ?",
            )
            .bind(guild_i64)
            .bind(mentioned.id.get() as i64)
            .execute(&data.db)
            .await;

            let ago = time::from_sql(&since)
                .map(|t| time::discord_ts(t, 'R'))
                .unwrap_or_default();
            let _ = message
                .channel_id
                .send_message(
                    &sctx.http,
                    serenity::CreateMessage::new().embed(
                        embed::titled(format!("{} is AFK", mentioned.name), embed::MUTED)
                            .description(format!("{}\nAway since {ago}.", text::escape_md(&reason))),
                    ),
                )
                .await;
        }
    }

    Ok(())
}

/// Show the last deleted message in this channel.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    category = "Utility",
    check = "perms::staff_only"
)]
pub async fn snipe(ctx: Ctx<'_>) -> Result<(), Error> {
    let channel = ctx.channel_id().get();
    let snipe = ctx.data().snipes.get(&channel).map(|s| s.clone());

    match snipe {
        Some(snipe) => {
            let mut e = embed::titled("🗑️ Last deleted message", embed::MUTED)
                .description(if snipe.content.trim().is_empty() {
                    "*(no text)*".to_string()
                } else {
                    text::truncate(&snipe.content, 3000)
                })
                .field("Author", format!("<@{}>", snipe.author_id), true)
                .field("Deleted", time::discord_ts(snipe.at, 'R'), true);
            if !snipe.attachments.is_empty() {
                e = e.field("Attachments", snipe.attachments.join("\n"), false);
            }
            ctx.send(poise::CreateReply::default().embed(e)).await?;
        }
        None => {
            ctx.send(
                poise::CreateReply::default()
                    .embed(embed::warn("Nothing has been deleted here since I started.")),
            )
            .await?;
        }
    }
    Ok(())
}

/// Show the last edited message in this channel.
#[poise::command(
    slash_command,
    guild_only,
    category = "Utility",
    check = "perms::staff_only"
)]
pub async fn editsnipe(ctx: Ctx<'_>) -> Result<(), Error> {
    let channel = ctx.channel_id().get();
    let snipe = ctx.data().edit_snipes.get(&channel).map(|s| s.clone());

    match snipe {
        Some(snipe) => {
            ctx.send(
                poise::CreateReply::default().embed(
                    embed::titled("✏️ Last edited message", embed::MUTED)
                        .field("Author", format!("<@{}>", snipe.author_id), true)
                        .field("Edited", time::discord_ts(snipe.at, 'R'), true)
                        .field(
                            "Before",
                            text::truncate(snipe.before.as_deref().unwrap_or("*(unknown)*"), 1000),
                            false,
                        )
                        .field("After", text::truncate(&snipe.content, 1000), false),
                ),
            )
            .await?;
        }
        None => {
            ctx.send(
                poise::CreateReply::default()
                    .embed(embed::warn("Nothing has been edited here since I started.")),
            )
            .await?;
        }
    }
    Ok(())
}

// ===========================================================================
// Help
// ===========================================================================

/// The categories commands are filed under, for `/help category:`.
#[derive(Debug, poise::ChoiceParameter)]
pub enum HelpCategory {
    Moderation,
    Automod,
    Community,
    Games,
    Leveling,
    Profile,
    Voice,
    Welcome,
    Roles,
    Automation,
    Utility,
    Settings,
}

impl HelpCategory {
    /// The string the `category = "..."` attributes use.
    fn key(&self) -> &'static str {
        match self {
            Self::Moderation => "Moderation",
            Self::Automod => "Automod",
            Self::Community => "Community",
            Self::Games => "Games",
            Self::Leveling => "Leveling",
            Self::Profile => "Profile",
            Self::Voice => "Voice",
            Self::Welcome => "Welcome",
            Self::Roles => "Roles",
            Self::Automation => "Automation",
            Self::Utility => "Utility",
            Self::Settings => "Settings",
        }
    }
}

/// Who may run a command, taken from the same policy the gate enforces so this
/// cannot drift from what actually happens.
fn tier_badge(command: &poise::Command<Data, Error>) -> &'static str {
    if !command.required_permissions.is_empty() {
        return "🔑";
    }
    match crate::gate::required_access(&command.qualified_name) {
        crate::gate::Access::Admin => "🔴",
        crate::gate::Access::Staff => "🔵",
        crate::gate::Access::Everyone => "🟢",
    }
}

/// Every command that can actually be run, with the category it belongs to.
///
/// A command with subcommands is a group: Discord will not let anyone invoke it
/// on its own, and only the parent carries the `category`, so that is handed
/// down as we walk.
fn collect_runnable<'a>(
    command: &'a poise::Command<Data, Error>,
    inherited: Option<&'a str>,
    out: &mut Vec<(&'a str, &'a poise::Command<Data, Error>)>,
) {
    if command.hide_in_help {
        return;
    }
    let category = command.category.as_deref().or(inherited);

    if command.subcommands.is_empty() {
        out.push((category.unwrap_or("Other"), command));
        return;
    }
    for sub in &command.subcommands {
        collect_runnable(sub, category, out);
    }
}

/// `🟢 /ban — Ban a member.`, or just the name when a command has no
/// description, so the line never trails off with a dangling dash.
fn help_line(command: &poise::Command<Data, Error>) -> String {
    let badge = tier_badge(command);
    let name = &command.qualified_name;
    match command.description.as_deref().map(str::trim) {
        Some(description) if !description.is_empty() => {
            format!("{badge} `/{name}` — {description}")
        }
        _ => format!("{badge} `/{name}`"),
    }
}

/// Group commands by the path they hang off, so a whole family fits on one
/// line: `/quote add|get|random|list`.
fn compact_lines(commands: &[&poise::Command<Data, Error>]) -> Vec<String> {
    let mut standalone: Vec<String> = Vec::new();
    let mut families: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();

    for command in commands {
        match command.qualified_name.rsplit_once(' ') {
            Some((parent, child)) => families
                .entry(parent.to_string())
                .or_default()
                .push(child.to_string()),
            None => standalone.push(format!("`/{}`", command.qualified_name)),
        }
    }

    let mut lines: Vec<String> = families
        .into_iter()
        .map(|(parent, children)| format!("`/{parent} {}`", children.join("|")))
        .collect();
    if !standalone.is_empty() {
        lines.push(standalone.join(" "));
    }
    lines
}

/// Every command, the commands in one category, or one command in detail.
#[poise::command(slash_command, prefix_command, category = "Utility")]
pub async fn help(
    ctx: Ctx<'_>,
    #[description = "A command to read about, e.g. ban or remind me"] command: Option<String>,
    #[description = "Show every command in one category, with descriptions"] category: Option<
        HelpCategory,
    >,
) -> Result<(), Error> {
    const LEGEND: &str = "🟢 anyone · 🔵 staff · 🔴 admin · 🔑 needs a Discord permission";

    // Flatten the whole tree once: every runnable command with its category.
    let registered = &ctx.framework().options().commands;
    let mut runnable: Vec<(&str, &poise::Command<Data, Error>)> = Vec::new();
    for top in registered {
        collect_runnable(top, None, &mut runnable);
    }

    // ---- one command in detail -------------------------------------------
    if let Some(name) = command {
        // Accept "/ban", "ban", and a subcommand path like "remind me".
        let needle = name
            .trim()
            .trim_start_matches('/')
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase();

        // Groups are worth describing too, so search the tree, not just leaves.
        let mut every: Vec<&poise::Command<Data, Error>> = Vec::new();
        fn push_all<'a>(
            command: &'a poise::Command<Data, Error>,
            out: &mut Vec<&'a poise::Command<Data, Error>>,
        ) {
            out.push(command);
            for sub in &command.subcommands {
                push_all(sub, out);
            }
        }
        for top in registered {
            push_all(top, &mut every);
        }

        let found = every
            .iter()
            .find(|c| c.qualified_name.to_lowercase() == needle);

        let reply = match found {
            Some(command) => {
                let mut e = embed::titled(
                    format!("{} /{}", tier_badge(command), command.qualified_name),
                    embed::INFO,
                )
                .description(
                    command
                        .description
                        .clone()
                        .unwrap_or_else(|| "No description.".to_string()),
                );

                if !command.subcommands.is_empty() {
                    let body = command
                        .subcommands
                        .iter()
                        .filter(|s| !s.hide_in_help)
                        .map(help_line)
                        .collect::<Vec<_>>()
                        .join("\n");
                    e = e.field(
                        format!("Subcommands ({})", command.subcommands.len()),
                        text::truncate(&body, 1000),
                        false,
                    );
                }

                if !command.required_permissions.is_empty() {
                    e = e.field(
                        "Needs",
                        format!("**{}**", command.required_permissions),
                        true,
                    );
                }

                e.footer(embed::footer(LEGEND))
            }
            None => embed::err(format!(
                "There is no command called `{needle}`. Run `/help` for the list."
            )),
        };
        ctx.send(poise::CreateReply::default().embed(reply)).await?;
        return Ok(());
    }

    // ---- one category, with descriptions ---------------------------------
    if let Some(category) = category {
        let wanted = category.key();
        let mut matching: Vec<&poise::Command<Data, Error>> = runnable
            .iter()
            .filter(|(cat, _)| *cat == wanted)
            .map(|(_, command)| *command)
            .collect();
        matching.sort_by(|a, b| a.qualified_name.cmp(&b.qualified_name));

        if matching.is_empty() {
            ctx.send(
                poise::CreateReply::default()
                    .embed(embed::warn("Nothing is filed under that category.")),
            )
            .await?;
            return Ok(());
        }

        let body = matching
            .iter()
            .map(|command| help_line(command))
            .collect::<Vec<_>>()
            .join("\n");

        // An embed description caps out at 4096 characters, and the bigger
        // categories go past that, so send the pages as separate embeds.
        let pages = text::chunk(&body, 3800);
        let mut reply = poise::CreateReply::default();
        for (index, page) in pages.iter().take(3).enumerate() {
            let title = if index == 0 {
                format!("📖 {wanted} — {} commands", matching.len())
            } else {
                format!("📖 {wanted} (continued)")
            };
            let mut e = embed::titled(title, embed::BRAND).description(page.clone());
            if index + 1 == pages.len().min(3) {
                e = e.footer(embed::footer(LEGEND));
            }
            reply = reply.embed(e);
        }
        ctx.send(reply).await?;
        return Ok(());
    }

    // ---- everything, grouped -------------------------------------------
    let mut groups: std::collections::BTreeMap<&str, Vec<&poise::Command<Data, Error>>> =
        std::collections::BTreeMap::new();
    for (category, command) in &runnable {
        groups.entry(*category).or_default().push(*command);
    }

    let mut e = embed::titled(format!("📖 All {} commands", runnable.len()), embed::BRAND)
        .description(format!(
            "`/help category:<name>` lists one group with descriptions, and \
             `/help command:<name>` explains one command - subcommands included, \
             so `/help command:remind me` works.\n\n{LEGEND}"
        ));

    for (category, mut commands) in groups {
        commands.sort_by(|a, b| a.qualified_name.cmp(&b.qualified_name));
        let lines = compact_lines(&commands);
        e = e.field(
            format!("{category} ({})", commands.len()),
            text::truncate(&lines.join("\n"), 1000),
            false,
        );
    }

    ctx.send(poise::CreateReply::default().embed(e)).await?;
    Ok(())
}

pub fn commands() -> Vec<poise::Command<Data, Error>> {
    vec![
        avatar(),
        banner(),
        userinfo(),
        serverinfo(),
        servericon(),
        roleinfo(),
        channelinfo(),
        permissions(),
        botinfo(),
        ping(),
        uptime(),
        timestamp(),
        calculator(),
        translate(),
        weather(),
        embed_builder(),
        afk(),
        snipe(),
        editsnipe(),
        help(),
    ]
}
