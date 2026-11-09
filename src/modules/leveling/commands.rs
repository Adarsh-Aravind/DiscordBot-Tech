//! Leveling commands: rank cards, leaderboard, XP administration, rewards,
//! multipliers, prestige and achievements.

use poise::serenity_prelude as serenity;
use serenity::{GuildChannel, Role, User};

use super::{
    achievement_label, add_xp, ensure_row, level_from_total, total_xp_for_level, xp_to_next,
    ACHIEVEMENTS,
};
use crate::db;
use crate::state::{Ctx, Data, Error};
use crate::util::rankcard::{self, thousands, RankInfo};
use crate::util::{embed, perms, text, time};

/// Your level, XP and rank in this server.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    category = "Leveling",
    aliases("level", "xp")
)]
pub async fn rank(
    ctx: Ctx<'_>,
    #[description = "Whose rank to show, defaults to you"] user: Option<User>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let target = user.unwrap_or_else(|| ctx.author().clone());
    let user_i64 = target.id.get() as i64;

    ctx.defer().await?;
    ensure_row(&ctx.data().db, guild_id, user_i64).await?;

    let row: (i64, i64, i64, i64, i64, i64, i64) = sqlx::query_as(
        "SELECT xp, level, total_xp, prestige, messages, voice_secs, streak
         FROM levels WHERE guild_id = ? AND user_id = ?",
    )
    .bind(guild_id)
    .bind(user_i64)
    .fetch_one(&ctx.data().db)
    .await?;
    let (xp, level, total_xp, prestige, messages, voice_secs, streak) = row;

    let rank_position: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) + 1 FROM levels
         WHERE guild_id = ? AND (prestige > ? OR (prestige = ? AND total_xp > ?))",
    )
    .bind(guild_id)
    .bind(prestige)
    .bind(prestige)
    .bind(total_xp)
    .fetch_one(&ctx.data().db)
    .await?;

    let accent: Option<String> =
        sqlx::query_scalar("SELECT color FROM profiles WHERE guild_id = ? AND user_id = ?")
            .bind(guild_id)
            .bind(user_i64)
            .fetch_optional(&ctx.data().db)
            .await?;
    let accent = accent.unwrap_or_else(|| "#5865F2".to_string());

    let needed = xp_to_next(level);

    // Try the image card first; fall back to an embed when no font is available.
    let info = RankInfo {
        name: target.name.clone(),
        discriminator: if target.discriminator.is_some() {
            format!("#{}", target.discriminator.map(|d| d.get()).unwrap_or(0))
        } else {
            String::new()
        },
        avatar_url: target.face(),
        level,
        prestige,
        rank: rank_position,
        xp_into_level: xp,
        xp_needed: needed,
        total_xp,
        messages,
        voice_secs,
        streak,
        accent: accent.clone(),
    };

    if let Some(png) = rankcard::render(
        &ctx.data().http,
        ctx.data().config.font_path.as_deref(),
        &info,
    )
    .await
    {
        ctx.send(
            poise::CreateReply::default().attachment(serenity::CreateAttachment::bytes(
                png,
                format!("rank-{user_i64}.png"),
            )),
        )
        .await?;
        return Ok(());
    }

    let bar = text::progress_bar(xp, needed, 20);
    let mut e = serenity::CreateEmbed::new()
        .colour(embed::parse_colour(&accent).unwrap_or_else(|| embed::colour(embed::XP)))
        .author(serenity::CreateEmbedAuthor::new(target.tag()).icon_url(target.face()))
        .thumbnail(target.face())
        .field("Level", level.to_string(), true)
        .field("Rank", format!("#{rank_position}"), true)
        .field("Total XP", thousands(total_xp), true)
        .field(
            "Progress",
            format!("`{bar}`\n**{}** / **{}** XP", thousands(xp), thousands(needed)),
            false,
        )
        .field("Messages", thousands(messages), true)
        .field("Voice", time::humanize(voice_secs), true)
        .field("Streak", format!("{streak} day(s)"), true);

    if prestige > 0 {
        e = e.footer(embed::footer(format!("Prestige {prestige}")));
    }

    ctx.send(poise::CreateReply::default().embed(e)).await?;
    Ok(())
}

/// The server XP leaderboard.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    category = "Leveling",
    aliases("lb", "top")
)]
pub async fn leaderboard(
    ctx: Ctx<'_>,
    #[description = "Page number"]
    #[min = 1]
    page: Option<u32>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let page = page.unwrap_or(1).max(1);
    let per_page = 10i64;
    let offset = (page as i64 - 1) * per_page;

    let rows: Vec<(i64, i64, i64, i64, i64)> = sqlx::query_as(
        "SELECT user_id, level, total_xp, prestige, messages FROM levels
         WHERE guild_id = ? AND total_xp > 0
         ORDER BY prestige DESC, total_xp DESC LIMIT ? OFFSET ?",
    )
    .bind(guild_id)
    .bind(per_page)
    .bind(offset)
    .fetch_all(&ctx.data().db)
    .await?;

    let total: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM levels WHERE guild_id = ? AND total_xp > 0")
            .bind(guild_id)
            .fetch_one(&ctx.data().db)
            .await?;

    if rows.is_empty() {
        ctx.send(poise::CreateReply::default().embed(embed::warn(
            "Nothing on this page yet. Chat a little and come back.",
        )))
        .await?;
        return Ok(());
    }

    let body = rows
        .iter()
        .enumerate()
        .map(|(i, (user_id, level, total_xp, prestige, messages))| {
            let position = offset + i as i64 + 1;
            let medal = match position {
                1 => "🥇",
                2 => "🥈",
                3 => "🥉",
                _ => "▫️",
            };
            let prestige_tag = if *prestige > 0 {
                format!(" `P{prestige}`")
            } else {
                String::new()
            };
            format!(
                "{medal} **{position}.** <@{user_id}>{prestige_tag}\n└ Level **{level}** · {} XP · {} messages",
                thousands(*total_xp),
                thousands(*messages)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    let pages = (total + per_page - 1) / per_page;
    ctx.send(
        poise::CreateReply::default().embed(
            embed::titled("🏆 Server leaderboard", embed::BRAND)
                .description(body)
                .footer(embed::footer(format!(
                    "Page {page} of {} · {total} ranked members",
                    pages.max(1)
                ))),
        ),
    )
    .await?;
    Ok(())
}

/// Where you sit on the leaderboard, as a single line.
#[poise::command(slash_command, guild_only, category = "Leveling")]
pub async fn mystats(ctx: Ctx<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let user_id = ctx.author().id.get() as i64;
    ensure_row(&ctx.data().db, guild_id, user_id).await?;

    let row: (i64, i64, i64, i64, i64, i64) = sqlx::query_as(
        "SELECT level, total_xp, messages, voice_secs, streak, best_streak
         FROM levels WHERE guild_id = ? AND user_id = ?",
    )
    .bind(guild_id)
    .bind(user_id)
    .fetch_one(&ctx.data().db)
    .await?;

    let achievements: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM achievements WHERE guild_id = ? AND user_id = ?",
    )
    .bind(guild_id)
    .bind(user_id)
    .fetch_one(&ctx.data().db)
    .await?;

    ctx.send(
        poise::CreateReply::default().embed(
            embed::titled("Your stats here", embed::XP)
                .field("Level", row.0.to_string(), true)
                .field("Total XP", thousands(row.1), true)
                .field("Messages", thousands(row.2), true)
                .field("Voice", time::humanize(row.3), true)
                .field("Current streak", format!("{} day(s)", row.4), true)
                .field("Best streak", format!("{} day(s)", row.5), true)
                .field(
                    "Achievements",
                    format!("{achievements} / {}", ACHIEVEMENTS.len()),
                    true,
                ),
        ),
    )
    .await?;
    Ok(())
}

/// Achievements you have earned, and the ones still to go.
#[poise::command(slash_command, guild_only, category = "Leveling")]
pub async fn achievements(
    ctx: Ctx<'_>,
    #[description = "Whose achievements, defaults to you"] user: Option<User>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let target = user.unwrap_or_else(|| ctx.author().clone());

    let earned: Vec<String> = sqlx::query_scalar(
        "SELECT key FROM achievements WHERE guild_id = ? AND user_id = ?",
    )
    .bind(guild_id)
    .bind(target.id.get() as i64)
    .fetch_all(&ctx.data().db)
    .await?;

    let have = ACHIEVEMENTS
        .iter()
        .filter(|(k, _)| earned.iter().any(|e| e == k))
        .map(|(_, label)| format!("✅ {label}"))
        .collect::<Vec<_>>();
    let missing = ACHIEVEMENTS
        .iter()
        .filter(|(k, _)| !earned.iter().any(|e| e == k))
        .map(|(_, label)| format!("🔒 {label}"))
        .collect::<Vec<_>>();

    ctx.send(
        poise::CreateReply::default().embed(
            embed::titled(format!("Achievements · {}", target.tag()), embed::BRAND)
                .description(format!(
                    "**{}** of **{}** earned",
                    have.len(),
                    ACHIEVEMENTS.len()
                ))
                .field(
                    "Earned",
                    if have.is_empty() {
                        "-".to_string()
                    } else {
                        have.join("\n")
                    },
                    true,
                )
                .field(
                    "Locked",
                    if missing.is_empty() {
                        "All done!".to_string()
                    } else {
                        missing.join("\n")
                    },
                    true,
                )
                .thumbnail(target.face()),
        ),
    )
    .await?;
    Ok(())
}

/// Trade a full level of XP to reset to level 0 with a prestige star.
#[poise::command(slash_command, guild_only, category = "Leveling")]
pub async fn prestige(ctx: Ctx<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let user_id = ctx.author().id.get() as i64;
    let cfg = db::leveling_config(&ctx.data().db, guild_id).await?;
    ensure_row(&ctx.data().db, guild_id, user_id).await?;

    let (level, prestige): (i64, i64) =
        sqlx::query_as("SELECT level, prestige FROM levels WHERE guild_id = ? AND user_id = ?")
            .bind(guild_id)
            .bind(user_id)
            .fetch_one(&ctx.data().db)
            .await?;

    if level < cfg.prestige_level {
        ctx.send(poise::CreateReply::default().embed(embed::err(format!(
            "You need level **{}** to prestige. You are level **{level}**.",
            cfg.prestige_level
        ))))
        .await?;
        return Ok(());
    }

    sqlx::query(
        "UPDATE levels SET xp = 0, level = 0, prestige = prestige + 1
         WHERE guild_id = ? AND user_id = ?",
    )
    .bind(guild_id)
    .bind(user_id)
    .execute(&ctx.data().db)
    .await?;

    let earned = super::check_achievements(&ctx.data().db, guild_id, user_id).await;
    let mut body = format!(
        "You are now **prestige {}**. Your level is back to 0 but your lifetime XP and total stats are kept.",
        prestige + 1
    );
    if !earned.is_empty() {
        body.push_str(&format!(
            "\n🏅 {}",
            earned
                .iter()
                .map(|k| achievement_label(k))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    ctx.send(
        poise::CreateReply::default()
            .embed(embed::titled("♻️ Prestige!", embed::XP).description(body)),
    )
    .await?;
    Ok(())
}

/// Give some of your XP to someone else.
#[poise::command(slash_command, guild_only, category = "Leveling")]
pub async fn xptransfer(
    ctx: Ctx<'_>,
    #[description = "Who receives the XP"] user: User,
    #[description = "How much to send"]
    #[min = 1]
    amount: i64,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let sender = ctx.author().id.get() as i64;
    let receiver = user.id.get() as i64;

    if sender == receiver {
        ctx.send(poise::CreateReply::default().embed(embed::err("You cannot send XP to yourself.")))
            .await?;
        return Ok(());
    }
    if user.bot {
        ctx.send(poise::CreateReply::default().embed(embed::err("Bots have no use for XP.")))
            .await?;
        return Ok(());
    }

    ensure_row(&ctx.data().db, guild_id, sender).await?;
    let available: i64 =
        sqlx::query_scalar("SELECT total_xp FROM levels WHERE guild_id = ? AND user_id = ?")
            .bind(guild_id)
            .bind(sender)
            .fetch_one(&ctx.data().db)
            .await?;

    if available < amount {
        ctx.send(poise::CreateReply::default().embed(embed::err(format!(
            "You only have **{}** XP.",
            thousands(available)
        ))))
        .await?;
        return Ok(());
    }

    add_xp(&ctx.data().db, guild_id, sender, -amount).await?;
    if let Some(levelup) = add_xp(&ctx.data().db, guild_id, receiver, amount).await? {
        let cfg = db::leveling_config(&ctx.data().db, guild_id).await?;
        super::apply_levelup(
            ctx.serenity_context(),
            ctx.data(),
            ctx.guild_id().unwrap(),
            user.id,
            Some(ctx.channel_id()),
            &cfg,
            levelup,
        )
        .await;
    }

    ctx.send(poise::CreateReply::default().embed(embed::ok(format!(
        "Sent **{}** XP to {}.",
        thousands(amount),
        user.tag()
    ))))
    .await?;
    Ok(())
}

// ===========================================================================
// Admin: XP
// ===========================================================================

/// Administer XP balances.
#[poise::command(
    slash_command,
    guild_only,
    category = "Leveling",
    subcommands("xp_give", "xp_take", "xp_set", "xp_reset", "xp_resetall"),
    check = "perms::admin_only"
)]
pub async fn xpadmin(_ctx: Ctx<'_>) -> Result<(), Error> {
    Ok(())
}

#[poise::command(slash_command, rename = "give", guild_only)]
pub async fn xp_give(
    ctx: Ctx<'_>,
    #[description = "Member"] user: User,
    #[description = "XP to add"] amount: i64,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    if let Some(levelup) = add_xp(&ctx.data().db, guild_id, user.id.get() as i64, amount).await? {
        let cfg = db::leveling_config(&ctx.data().db, guild_id).await?;
        super::apply_levelup(
            ctx.serenity_context(),
            ctx.data(),
            ctx.guild_id().unwrap(),
            user.id,
            Some(ctx.channel_id()),
            &cfg,
            levelup,
        )
        .await;
    }
    ctx.send(poise::CreateReply::default().embed(embed::ok(format!(
        "Gave **{}** XP to {}.",
        thousands(amount),
        user.tag()
    ))))
    .await?;
    Ok(())
}

#[poise::command(slash_command, rename = "take", guild_only)]
pub async fn xp_take(
    ctx: Ctx<'_>,
    #[description = "Member"] user: User,
    #[description = "XP to remove"]
    #[min = 1]
    amount: i64,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    add_xp(&ctx.data().db, guild_id, user.id.get() as i64, -amount).await?;
    ctx.send(poise::CreateReply::default().embed(embed::ok(format!(
        "Removed **{}** XP from {}.",
        thousands(amount),
        user.tag()
    ))))
    .await?;
    Ok(())
}

#[poise::command(slash_command, rename = "set", guild_only)]
pub async fn xp_set(
    ctx: Ctx<'_>,
    #[description = "Member"] user: User,
    #[description = "Level to put them at"]
    #[min = 0]
    level: i64,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let user_i64 = user.id.get() as i64;
    ensure_row(&ctx.data().db, guild_id, user_i64).await?;

    let total = total_xp_for_level(level);
    sqlx::query(
        "UPDATE levels SET xp = 0, level = ?, total_xp = ? WHERE guild_id = ? AND user_id = ?",
    )
    .bind(level)
    .bind(total)
    .bind(guild_id)
    .bind(user_i64)
    .execute(&ctx.data().db)
    .await?;

    ctx.send(poise::CreateReply::default().embed(embed::ok(format!(
        "{} is now level **{level}** ({} total XP).",
        user.tag(),
        thousands(total)
    ))))
    .await?;
    Ok(())
}

#[poise::command(slash_command, rename = "reset", guild_only)]
pub async fn xp_reset(
    ctx: Ctx<'_>,
    #[description = "Member to reset"] user: User,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    sqlx::query("DELETE FROM levels WHERE guild_id = ? AND user_id = ?")
        .bind(guild_id)
        .bind(user.id.get() as i64)
        .execute(&ctx.data().db)
        .await?;
    ctx.send(
        poise::CreateReply::default()
            .embed(embed::ok(format!("Reset all XP for {}.", user.tag()))),
    )
    .await?;
    Ok(())
}

/// Wipe every level in the server. There is no undo.
#[poise::command(slash_command, rename = "resetall", guild_only)]
pub async fn xp_resetall(
    ctx: Ctx<'_>,
    #[description = "Type the server name to confirm"] confirm: String,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap();
    let name = crate::modules::moderation::guild_name(ctx.serenity_context(), guild_id);

    if confirm.trim() != name {
        ctx.send(poise::CreateReply::default().embed(embed::err(format!(
            "That did not match. To confirm, run this again with `confirm: {name}`."
        ))))
        .await?;
        return Ok(());
    }

    let result = sqlx::query("DELETE FROM levels WHERE guild_id = ?")
        .bind(guild_id.get() as i64)
        .execute(&ctx.data().db)
        .await?;
    ctx.send(poise::CreateReply::default().embed(embed::ok(format!(
        "Wiped levels for **{}** member(s).",
        result.rows_affected()
    ))))
    .await?;
    Ok(())
}

// ===========================================================================
// Admin: leveling configuration
// ===========================================================================

/// Configure the XP system.
#[poise::command(
    slash_command,
    guild_only,
    category = "Leveling",
    subcommands(
        "levels_toggle",
        "levels_rates",
        "levels_announce",
        "levels_reward",
        "levels_rewards",
        "levels_multiplier",
        "levels_noxp",
        "levels_status"
    ),
    check = "perms::admin_only"
)]
pub async fn levels(_ctx: Ctx<'_>) -> Result<(), Error> {
    Ok(())
}

#[poise::command(slash_command, rename = "toggle", guild_only)]
pub async fn levels_toggle(
    ctx: Ctx<'_>,
    #[description = "Enable the XP system?"] enabled: bool,
    #[description = "Also enable voice XP?"] voice: Option<bool>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    db::set_bool(&ctx.data().db, "leveling_config", "enabled", guild_id, enabled).await?;
    if let Some(voice) = voice {
        db::set_bool(
            &ctx.data().db,
            "leveling_config",
            "voice_enabled",
            guild_id,
            voice,
        )
        .await?;
    }
    ctx.send(poise::CreateReply::default().embed(embed::ok(format!(
        "XP system is now {}.",
        if enabled { "on" } else { "off" }
    ))))
    .await?;
    Ok(())
}

#[poise::command(slash_command, rename = "rates", guild_only)]
pub async fn levels_rates(
    ctx: Ctx<'_>,
    #[description = "Minimum XP per message"]
    #[min = 1]
    xp_min: Option<i64>,
    #[description = "Maximum XP per message"]
    #[min = 1]
    xp_max: Option<i64>,
    #[description = "Seconds between XP awards"]
    #[min = 0]
    cooldown: Option<i64>,
    #[description = "XP per minute in voice"]
    #[min = 0]
    voice_per_minute: Option<i64>,
    #[description = "Server-wide multiplier, e.g. 1.5"] global_rate: Option<f64>,
    #[description = "Level needed to prestige"]
    #[min = 10]
    prestige_level: Option<i64>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let dbh = &ctx.data().db;

    if let Some(v) = xp_min {
        db::set_i64(dbh, "leveling_config", "xp_min", guild_id, v).await?;
    }
    if let Some(v) = xp_max {
        db::set_i64(dbh, "leveling_config", "xp_max", guild_id, v).await?;
    }
    if let Some(v) = cooldown {
        db::set_i64(dbh, "leveling_config", "cooldown", guild_id, v).await?;
    }
    if let Some(v) = voice_per_minute {
        db::set_i64(dbh, "leveling_config", "voice_xp_per_min", guild_id, v).await?;
    }
    if let Some(v) = global_rate {
        db::set_real(dbh, "leveling_config", "global_rate", guild_id, v).await?;
    }
    if let Some(v) = prestige_level {
        db::set_i64(dbh, "leveling_config", "prestige_level", guild_id, v).await?;
    }

    ctx.send(poise::CreateReply::default().embed(embed::ok("XP rates updated.")))
        .await?;
    Ok(())
}

#[poise::command(slash_command, rename = "announce", guild_only)]
pub async fn levels_announce(
    ctx: Ctx<'_>,
    #[description = "Announce level-ups?"] enabled: bool,
    #[description = "Channel for announcements, or leave empty for the member's channel"]
    channel: Option<GuildChannel>,
    #[description = "Message template"] message: Option<String>,
    #[description = "Also DM the member?"] dm: Option<bool>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let dbh = &ctx.data().db;

    db::set_bool(dbh, "leveling_config", "announce", guild_id, enabled).await?;
    db::set_opt_i64(
        dbh,
        "leveling_config",
        "levelup_channel",
        guild_id,
        channel.as_ref().map(|c| c.id.get() as i64),
    )
    .await?;
    if let Some(message) = &message {
        db::set_text(
            dbh,
            "leveling_config",
            "levelup_message",
            guild_id,
            message.clone(),
        )
        .await?;
    }
    if let Some(dm) = dm {
        db::set_bool(dbh, "leveling_config", "levelup_dm", guild_id, dm).await?;
    }

    ctx.send(
        poise::CreateReply::default().embed(
            embed::ok("Level-up announcements updated.")
                .footer(embed::footer("Placeholders: {mention} {level} {server}")),
        ),
    )
    .await?;
    Ok(())
}

#[poise::command(slash_command, rename = "reward", guild_only)]
pub async fn levels_reward(
    ctx: Ctx<'_>,
    #[description = "Level at which the role is given"]
    #[min = 1]
    level: i64,
    #[description = "Role to award"] role: Role,
    #[description = "Remove this reward instead"] remove: Option<bool>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;

    if remove.unwrap_or(false) {
        sqlx::query("DELETE FROM level_rewards WHERE guild_id = ? AND level = ? AND role_id = ?")
            .bind(guild_id)
            .bind(level)
            .bind(role.id.get() as i64)
            .execute(&ctx.data().db)
            .await?;
        ctx.send(poise::CreateReply::default().embed(embed::ok("Reward removed.")))
            .await?;
        return Ok(());
    }

    sqlx::query(
        "INSERT OR IGNORE INTO level_rewards (guild_id, level, role_id) VALUES (?, ?, ?)",
    )
    .bind(guild_id)
    .bind(level)
    .bind(role.id.get() as i64)
    .execute(&ctx.data().db)
    .await?;

    ctx.send(poise::CreateReply::default().embed(embed::ok(format!(
        "<@&{}> will be given at level **{level}**.",
        role.id.get()
    ))))
    .await?;
    Ok(())
}

#[poise::command(slash_command, rename = "rewards", guild_only)]
pub async fn levels_rewards(
    ctx: Ctx<'_>,
    #[description = "Keep every reward role (false keeps only the highest)"] stack: Option<bool>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    if let Some(stack) = stack {
        db::set_bool(
            &ctx.data().db,
            "leveling_config",
            "stack_rewards",
            guild_id,
            stack,
        )
        .await?;
    }

    let rows: Vec<(i64, i64)> = sqlx::query_as(
        "SELECT level, role_id FROM level_rewards WHERE guild_id = ? ORDER BY level",
    )
    .bind(guild_id)
    .fetch_all(&ctx.data().db)
    .await?;
    let cfg = db::leveling_config(&ctx.data().db, guild_id).await?;

    let body = if rows.is_empty() {
        "No role rewards. Add one with `/levels reward`.".to_string()
    } else {
        rows.iter()
            .map(|(level, role_id)| format!("Level **{level}** → <@&{role_id}>"))
            .collect::<Vec<_>>()
            .join("\n")
    };

    ctx.send(
        poise::CreateReply::default().embed(
            embed::titled("Level role rewards", embed::INFO)
                .description(body)
                .footer(embed::footer(format!(
                    "Stacking: {}",
                    if cfg.stack_rewards == 1 {
                        "keep every reward"
                    } else {
                        "highest only"
                    }
                ))),
        ),
    )
    .await?;
    Ok(())
}

#[poise::command(slash_command, rename = "multiplier", guild_only)]
pub async fn levels_multiplier(
    ctx: Ctx<'_>,
    #[description = "Role that gets the multiplier"] role: Option<Role>,
    #[description = "Channel that gets the multiplier"] channel: Option<GuildChannel>,
    #[description = "Multiplier, e.g. 2.0. Use 1.0 to clear."] multiplier: f64,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;

    let (kind, target) = match (&role, &channel) {
        (Some(role), _) => ("role", role.id.get() as i64),
        (_, Some(channel)) => ("channel", channel.id.get() as i64),
        _ => {
            ctx.send(
                poise::CreateReply::default()
                    .embed(embed::err("Give me a role or a channel.")),
            )
            .await?;
            return Ok(());
        }
    };

    if (multiplier - 1.0).abs() < f64::EPSILON {
        sqlx::query("DELETE FROM xp_multipliers WHERE guild_id = ? AND kind = ? AND target_id = ?")
            .bind(guild_id)
            .bind(kind)
            .bind(target)
            .execute(&ctx.data().db)
            .await?;
        ctx.send(poise::CreateReply::default().embed(embed::ok("Multiplier cleared.")))
            .await?;
        return Ok(());
    }

    sqlx::query(
        "INSERT INTO xp_multipliers (guild_id, kind, target_id, multiplier) VALUES (?, ?, ?, ?)
         ON CONFLICT(guild_id, kind, target_id) DO UPDATE SET multiplier = excluded.multiplier",
    )
    .bind(guild_id)
    .bind(kind)
    .bind(target)
    .bind(multiplier.clamp(0.0, 10.0))
    .execute(&ctx.data().db)
    .await?;

    ctx.send(poise::CreateReply::default().embed(embed::ok(format!(
        "Multiplier **{multiplier}x** set for that {kind}."
    ))))
    .await?;
    Ok(())
}

#[poise::command(slash_command, rename = "noxp", guild_only)]
pub async fn levels_noxp(
    ctx: Ctx<'_>,
    #[description = "Channel to exclude"] channel: Option<GuildChannel>,
    #[description = "Role to exclude"] role: Option<Role>,
    #[description = "Remove from the exclusion list instead"] remove: Option<bool>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let cfg = db::leveling_config(&ctx.data().db, guild_id).await?;
    let removing = remove.unwrap_or(false);

    if let Some(channel) = &channel {
        let mut ids = db::csv_ids(&cfg.no_xp_channels);
        let id = channel.id.get() as i64;
        if removing {
            ids.retain(|c| *c != id);
        } else if !ids.contains(&id) {
            ids.push(id);
        }
        db::set_text(
            &ctx.data().db,
            "leveling_config",
            "no_xp_channels",
            guild_id,
            db::ids_csv(&ids),
        )
        .await?;
    }

    if let Some(role) = &role {
        let mut ids = db::csv_ids(&cfg.no_xp_roles);
        let id = role.id.get() as i64;
        if removing {
            ids.retain(|r| *r != id);
        } else if !ids.contains(&id) {
            ids.push(id);
        }
        db::set_text(
            &ctx.data().db,
            "leveling_config",
            "no_xp_roles",
            guild_id,
            db::ids_csv(&ids),
        )
        .await?;
    }

    if channel.is_none() && role.is_none() {
        ctx.send(
            poise::CreateReply::default()
                .embed(embed::err("Give me a channel or a role.")),
        )
        .await?;
        return Ok(());
    }

    ctx.send(poise::CreateReply::default().embed(embed::ok("Exclusion list updated.")))
        .await?;
    Ok(())
}

#[poise::command(slash_command, rename = "status", guild_only)]
pub async fn levels_status(ctx: Ctx<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let cfg = db::leveling_config(&ctx.data().db, guild_id).await?;

    let multipliers: Vec<(String, i64, f64)> =
        sqlx::query_as("SELECT kind, target_id, multiplier FROM xp_multipliers WHERE guild_id = ?")
            .bind(guild_id)
            .fetch_all(&ctx.data().db)
            .await?;

    let multiplier_body = if multipliers.is_empty() {
        "none".to_string()
    } else {
        multipliers
            .iter()
            .map(|(kind, id, m)| {
                if kind == "role" {
                    format!("<@&{id}> ×{m}")
                } else {
                    format!("<#{id}> ×{m}")
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    let list = |csv: &str, role: bool| {
        let ids = db::csv_ids(csv);
        if ids.is_empty() {
            "none".to_string()
        } else {
            ids.iter()
                .map(|i| if role { format!("<@&{i}>") } else { format!("<#{i}>") })
                .collect::<Vec<_>>()
                .join(", ")
        }
    };

    ctx.send(
        poise::CreateReply::default().embed(
            embed::titled("XP configuration", embed::INFO)
                .field(
                    "Enabled",
                    if cfg.enabled == 1 { "yes" } else { "no" },
                    true,
                )
                .field(
                    "Voice XP",
                    if cfg.voice_enabled == 1 {
                        format!("{} XP/min", cfg.voice_xp_per_min)
                    } else {
                        "off".to_string()
                    },
                    true,
                )
                .field(
                    "Message XP",
                    format!("{}-{} every {}s", cfg.xp_min, cfg.xp_max, cfg.cooldown),
                    true,
                )
                .field("Global rate", format!("×{}", cfg.global_rate), true)
                .field("Prestige at", format!("level {}", cfg.prestige_level), true)
                .field(
                    "Announcements",
                    if cfg.announce == 1 {
                        cfg.levelup_channel
                            .map(|c| format!("<#{c}>"))
                            .unwrap_or_else(|| "member's channel".to_string())
                    } else {
                        "off".to_string()
                    },
                    true,
                )
                .field("Multipliers", multiplier_body, false)
                .field("No-XP channels", list(&cfg.no_xp_channels, false), true)
                .field("No-XP roles", list(&cfg.no_xp_roles, true), true)
                .field(
                    "Level curve",
                    format!(
                        "Level 1: {} XP · Level 10: {} XP · Level 50: {} XP",
                        thousands(total_xp_for_level(1)),
                        thousands(total_xp_for_level(10)),
                        thousands(total_xp_for_level(50))
                    ),
                    false,
                ),
        ),
    )
    .await?;
    Ok(())
}

/// How much XP a level needs.
#[poise::command(slash_command, guild_only, category = "Leveling")]
pub async fn levelcurve(
    ctx: Ctx<'_>,
    #[description = "Level to look up"]
    #[min = 0]
    #[max = 500]
    level: i64,
) -> Result<(), Error> {
    let to_next = xp_to_next(level);
    let total = total_xp_for_level(level);
    ctx.send(
        poise::CreateReply::default().embed(
            embed::titled(format!("Level {level}"), embed::XP)
                .field("Total XP to reach it", thousands(total), true)
                .field("XP for the next level", thousands(to_next), true)
                .field(
                    "Level at 100k XP",
                    level_from_total(100_000).to_string(),
                    true,
                ),
        ),
    )
    .await?;
    Ok(())
}

pub fn commands() -> Vec<poise::Command<Data, Error>> {
    vec![
        rank(),
        leaderboard(),
        mystats(),
        achievements(),
        prestige(),
        xptransfer(),
        xpadmin(),
        levels(),
        levelcurve(),
    ]
}
