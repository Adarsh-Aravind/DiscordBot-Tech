//! Member profiles and per-member activity.

use poise::serenity_prelude as serenity;
use serenity::User;

use crate::state::{Ctx, Data, Error};
use crate::util::{embed, rankcard::thousands, text, time};

async fn ensure_profile(data: &Data, guild_id: i64, user_id: i64) -> Result<(), Error> {
    sqlx::query("INSERT OR IGNORE INTO profiles (guild_id, user_id) VALUES (?, ?)")
        .bind(guild_id)
        .bind(user_id)
        .execute(&data.db)
        .await?;
    Ok(())
}

/// Your profile card: bio, badges, level, activity.
#[poise::command(
    slash_command,
    guild_only,
    category = "Profile",
    subcommands(
        "profile_view",
        "profile_bio",
        "profile_colour",
        "profile_pronouns",
        "profile_favourite",
        "profile_timezone",
        "profile_banner",
        "profile_clear"
    )
)]
pub async fn profile(_ctx: Ctx<'_>) -> Result<(), Error> {
    Ok(())
}

/// Show a profile.
#[poise::command(slash_command, rename = "view", guild_only)]
pub async fn profile_view(
    ctx: Ctx<'_>,
    #[description = "Whose profile, defaults to you"] user: Option<User>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let target = user.unwrap_or_else(|| ctx.author().clone());
    let user_i64 = target.id.get() as i64;
    ensure_profile(ctx.data(), guild_id, user_i64).await?;

    #[derive(sqlx::FromRow)]
    struct Row {
        bio: String,
        color: String,
        banner_url: Option<String>,
        badge: String,
        timezone: Option<String>,
        pronouns: String,
        favourite: String,
    }

    let row: Row = sqlx::query_as("SELECT * FROM profiles WHERE guild_id = ? AND user_id = ?")
        .bind(guild_id)
        .bind(user_i64)
        .fetch_one(&ctx.data().db)
        .await?;

    let level: Option<(i64, i64, i64, i64, i64, i64)> = sqlx::query_as(
        "SELECT level, prestige, total_xp, messages, voice_secs, streak
         FROM levels WHERE guild_id = ? AND user_id = ?",
    )
    .bind(guild_id)
    .bind(user_i64)
    .fetch_optional(&ctx.data().db)
    .await?;

    let joined: Option<String> =
        sqlx::query_scalar("SELECT first_join FROM members WHERE guild_id = ? AND user_id = ?")
            .bind(guild_id)
            .bind(user_i64)
            .fetch_optional(&ctx.data().db)
            .await?;

    let badges: Vec<String> = sqlx::query_scalar(
        "SELECT key FROM achievements WHERE guild_id = ? AND user_id = ? ORDER BY earned_at",
    )
    .bind(guild_id)
    .bind(user_i64)
    .fetch_all(&ctx.data().db)
    .await?;

    let colour = embed::parse_colour(&row.color).unwrap_or_else(|| embed::colour(embed::BRAND));

    let mut e = serenity::CreateEmbed::new()
        .colour(colour)
        .author(
            serenity::CreateEmbedAuthor::new(target.tag()).icon_url(target.face()),
        )
        .thumbnail(target.face());

    if !row.bio.trim().is_empty() {
        e = e.description(text::truncate(&row.bio, 1000));
    }
    if !row.pronouns.trim().is_empty() {
        e = e.field("Pronouns", row.pronouns.clone(), true);
    }
    if let Some(tz) = &row.timezone {
        e = e.field("Timezone", tz.clone(), true);
    }
    if !row.favourite.trim().is_empty() {
        e = e.field("Favourite tech", text::truncate(&row.favourite, 200), true);
    }

    if let Some((level, prestige, total_xp, messages, voice, streak)) = level {
        let rank: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) + 1 FROM levels
             WHERE guild_id = ? AND (prestige > ? OR (prestige = ? AND total_xp > ?))",
        )
        .bind(guild_id)
        .bind(prestige)
        .bind(prestige)
        .bind(total_xp)
        .fetch_one(&ctx.data().db)
        .await?;

        e = e.field(
            "Level",
            if prestige > 0 {
                format!("**{level}** (prestige {prestige})")
            } else {
                format!("**{level}**")
            },
            true,
        )
        .field("Rank", format!("#{rank}"), true)
        .field("Total XP", thousands(total_xp), true)
        .field("Messages", thousands(messages), true)
        .field("Voice time", time::humanize(voice), true)
        .field("Daily streak", format!("{streak} day(s)"), true);
    }

    if !badges.is_empty() {
        e = e.field(
            "Achievements",
            badges
                .iter()
                .map(|b| crate::modules::leveling::achievement_label(b))
                .collect::<Vec<_>>()
                .join(" "),
            false,
        );
    }
    if !row.badge.trim().is_empty() {
        e = e.field("Title", row.badge.clone(), true);
    }
    if let Some(joined) = joined.as_deref().and_then(time::from_sql) {
        e = e.footer(embed::footer(format!(
            "Member since {}",
            joined.format("%d %b %Y")
        )));
    }
    if let Some(banner) = &row.banner_url {
        e = e.image(banner.clone());
    }

    ctx.send(poise::CreateReply::default().embed(e)).await?;
    Ok(())
}

/// Set your bio.
#[poise::command(slash_command, rename = "bio", guild_only)]
pub async fn profile_bio(
    ctx: Ctx<'_>,
    #[description = "Up to 400 characters"] bio: String,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let user_id = ctx.author().id.get() as i64;
    ensure_profile(ctx.data(), guild_id, user_id).await?;

    sqlx::query("UPDATE profiles SET bio = ? WHERE guild_id = ? AND user_id = ?")
        .bind(text::truncate(&bio, 400))
        .bind(guild_id)
        .bind(user_id)
        .execute(&ctx.data().db)
        .await?;

    ctx.send(
        poise::CreateReply::default()
            .embed(embed::ok("Bio saved."))
            .ephemeral(true),
    )
    .await?;
    Ok(())
}

/// Set the accent colour used on your profile and rank card.
#[poise::command(slash_command, rename = "colour", guild_only)]
pub async fn profile_colour(
    ctx: Ctx<'_>,
    #[description = "Hex colour, e.g. #ff8800"] colour: String,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let user_id = ctx.author().id.get() as i64;

    // Parse once, store the canonical form. Re-parsing what we formatted
    // ourselves used to be able to fail, which meant an `unwrap` on `None`.
    let Some(value) = embed::colour_value(&colour) else {
        ctx.send(
            poise::CreateReply::default()
                .embed(embed::err(
                    "That is not a colour I recognise. Try `#5865F2`.",
                ))
                .ephemeral(true),
        )
        .await?;
        return Ok(());
    };
    let normalised = embed::canonical_colour(value);

    ensure_profile(ctx.data(), guild_id, user_id).await?;
    sqlx::query("UPDATE profiles SET color = ? WHERE guild_id = ? AND user_id = ?")
        .bind(&normalised)
        .bind(guild_id)
        .bind(user_id)
        .execute(&ctx.data().db)
        .await?;

    ctx.send(
        poise::CreateReply::default()
            .embed(
                serenity::CreateEmbed::new()
                    .colour(embed::colour(value))
                    .description(format!("✅ Accent colour set to `{normalised}`.")),
            )
            .ephemeral(true),
    )
    .await?;
    Ok(())
}

/// Set your pronouns.
#[poise::command(slash_command, rename = "pronouns", guild_only)]
pub async fn profile_pronouns(
    ctx: Ctx<'_>,
    #[description = "For example: they/them"] pronouns: String,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let user_id = ctx.author().id.get() as i64;
    ensure_profile(ctx.data(), guild_id, user_id).await?;
    sqlx::query("UPDATE profiles SET pronouns = ? WHERE guild_id = ? AND user_id = ?")
        .bind(text::truncate(&pronouns, 40))
        .bind(guild_id)
        .bind(user_id)
        .execute(&ctx.data().db)
        .await?;
    ctx.send(
        poise::CreateReply::default()
            .embed(embed::ok("Saved."))
            .ephemeral(true),
    )
    .await?;
    Ok(())
}

/// Your favourite piece of tech, shown on your profile.
#[poise::command(slash_command, rename = "favourite", guild_only)]
pub async fn profile_favourite(
    ctx: Ctx<'_>,
    #[description = "Favourite gadget, app or setup"] favourite: String,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let user_id = ctx.author().id.get() as i64;
    ensure_profile(ctx.data(), guild_id, user_id).await?;
    sqlx::query("UPDATE profiles SET favourite = ? WHERE guild_id = ? AND user_id = ?")
        .bind(text::truncate(&favourite, 200))
        .bind(guild_id)
        .bind(user_id)
        .execute(&ctx.data().db)
        .await?;
    ctx.send(
        poise::CreateReply::default()
            .embed(embed::ok("Saved."))
            .ephemeral(true),
    )
    .await?;
    Ok(())
}

/// Your timezone, used by reminders.
#[poise::command(slash_command, rename = "timezone", guild_only)]
pub async fn profile_timezone(
    ctx: Ctx<'_>,
    #[description = "IANA name, e.g. Europe/London or Asia/Kolkata"] timezone: String,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let user_id = ctx.author().id.get() as i64;

    if timezone.parse::<chrono_tz::Tz>().is_err() {
        ctx.send(
            poise::CreateReply::default()
                .embed(embed::err(
                    "I do not recognise that timezone. Use an IANA name such as `Europe/London`.",
                ))
                .ephemeral(true),
        )
        .await?;
        return Ok(());
    }

    ensure_profile(ctx.data(), guild_id, user_id).await?;
    sqlx::query("UPDATE profiles SET timezone = ? WHERE guild_id = ? AND user_id = ?")
        .bind(&timezone)
        .bind(guild_id)
        .bind(user_id)
        .execute(&ctx.data().db)
        .await?;
    ctx.send(
        poise::CreateReply::default()
            .embed(embed::ok(format!("Timezone set to `{timezone}`.")))
            .ephemeral(true),
    )
    .await?;
    Ok(())
}

/// A banner image for the bottom of your profile.
#[poise::command(slash_command, rename = "banner", guild_only)]
pub async fn profile_banner(
    ctx: Ctx<'_>,
    #[description = "Direct image URL, or leave empty to remove"] url: Option<String>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let user_id = ctx.author().id.get() as i64;
    ensure_profile(ctx.data(), guild_id, user_id).await?;

    if let Some(url) = &url {
        if !url.starts_with("https://") {
            ctx.send(
                poise::CreateReply::default()
                    .embed(embed::err("The banner must be an `https://` image URL."))
                    .ephemeral(true),
            )
            .await?;
            return Ok(());
        }
    }

    sqlx::query("UPDATE profiles SET banner_url = ? WHERE guild_id = ? AND user_id = ?")
        .bind(&url)
        .bind(guild_id)
        .bind(user_id)
        .execute(&ctx.data().db)
        .await?;

    ctx.send(
        poise::CreateReply::default()
            .embed(embed::ok(if url.is_some() {
                "Banner saved."
            } else {
                "Banner removed."
            }))
            .ephemeral(true),
    )
    .await?;
    Ok(())
}

/// Wipe your profile back to defaults.
#[poise::command(slash_command, rename = "clear", guild_only)]
pub async fn profile_clear(ctx: Ctx<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let user_id = ctx.author().id.get() as i64;
    sqlx::query("DELETE FROM profiles WHERE guild_id = ? AND user_id = ?")
        .bind(guild_id)
        .bind(user_id)
        .execute(&ctx.data().db)
        .await?;
    ctx.send(
        poise::CreateReply::default()
            .embed(embed::ok("Profile cleared."))
            .ephemeral(true),
    )
    .await?;
    Ok(())
}

pub fn commands() -> Vec<poise::Command<Data, Error>> {
    vec![profile()]
}
