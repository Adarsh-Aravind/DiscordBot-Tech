//! Invite tracking: who invited whom, and the invite leaderboard.
//!
//! Discord gives no "who used this invite" event, so the standard trick is to
//! keep a snapshot of every invite's use count and diff it on each join.

use poise::serenity_prelude as serenity;
use serenity::{GuildId, User, UserId};

use crate::state::{Ctx, Data, Error};
use crate::util::{embed, perms};

/// Pull the live invite list into our snapshot table. Called on ready, on
/// invite create/delete and after every attribution.
pub async fn refresh_cache(sctx: &serenity::Context, data: &Data, guild_id: GuildId) {
    let Ok(invites) = guild_id.invites(&sctx.http).await else {
        // Missing Manage Guild: invite tracking simply stays off.
        return;
    };
    let guild_i64 = guild_id.get() as i64;

    for invite in &invites {
        let _ = sqlx::query(
            "INSERT INTO invite_cache (guild_id, code, inviter, uses) VALUES (?, ?, ?, ?)
             ON CONFLICT(guild_id, code) DO UPDATE SET uses = excluded.uses, inviter = excluded.inviter",
        )
        .bind(guild_i64)
        .bind(&invite.code)
        .bind(invite.inviter.as_ref().map(|u| u.id.get() as i64).unwrap_or(0))
        .bind(invite.uses as i64)
        .execute(&data.db)
        .await;
    }

    // Forget codes that no longer exist.
    let live: Vec<String> = invites.iter().map(|i| i.code.clone()).collect();
    let stored: Vec<String> =
        sqlx::query_scalar("SELECT code FROM invite_cache WHERE guild_id = ?")
            .bind(guild_i64)
            .fetch_all(&data.db)
            .await
            .unwrap_or_default();
    for code in stored {
        if !live.contains(&code) {
            let _ = sqlx::query("DELETE FROM invite_cache WHERE guild_id = ? AND code = ?")
                .bind(guild_i64)
                .bind(&code)
                .execute(&data.db)
                .await;
        }
    }
}

/// Work out which invite a new member used. Returns (inviter_id, their total).
pub async fn attribute_join(
    sctx: &serenity::Context,
    data: &Data,
    guild_id: GuildId,
    joined: UserId,
) -> Option<(i64, i64)> {
    let guild_i64 = guild_id.get() as i64;

    let live = guild_id.invites(&sctx.http).await.ok()?;
    let stored: Vec<(String, i64, i64)> =
        sqlx::query_as("SELECT code, inviter, uses FROM invite_cache WHERE guild_id = ?")
            .bind(guild_i64)
            .fetch_all(&data.db)
            .await
            .ok()?;

    let mut used: Option<(String, i64)> = None;
    for invite in &live {
        let previous = stored
            .iter()
            .find(|(code, _, _)| code == &invite.code)
            .map(|(_, _, uses)| *uses)
            .unwrap_or(0);
        if invite.uses as i64 > previous {
            let inviter = invite
                .inviter
                .as_ref()
                .map(|u| u.id.get() as i64)
                .unwrap_or(0);
            used = Some((invite.code.clone(), inviter));
            break;
        }
    }

    // Always resync, whether or not we identified the invite.
    refresh_cache(sctx, data, guild_id).await;

    let (code, inviter) = used?;
    if inviter == 0 || inviter == joined.get() as i64 {
        return None;
    }

    let _ = sqlx::query(
        "UPDATE members SET inviter_id = ?, invite_code = ? WHERE guild_id = ? AND user_id = ?",
    )
    .bind(inviter)
    .bind(&code)
    .bind(guild_i64)
    .bind(joined.get() as i64)
    .execute(&data.db)
    .await;

    let _ = sqlx::query(
        "INSERT INTO invite_stats (guild_id, user_id, total) VALUES (?, ?, 1)
         ON CONFLICT(guild_id, user_id) DO UPDATE SET total = invite_stats.total + 1",
    )
    .bind(guild_i64)
    .bind(inviter)
    .execute(&data.db)
    .await;

    let total: i64 = sqlx::query_scalar(
        "SELECT total FROM invite_stats WHERE guild_id = ? AND user_id = ?",
    )
    .bind(guild_i64)
    .bind(inviter)
    .fetch_optional(&data.db)
    .await
    .ok()
    .flatten()
    .unwrap_or(0);

    Some((inviter, total))
}

/// How many people someone has brought in.
#[poise::command(slash_command, prefix_command, guild_only, category = "Community")]
pub async fn invites(
    ctx: Ctx<'_>,
    #[description = "Whose invites to show, defaults to you"] user: Option<User>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let target = user.unwrap_or_else(|| ctx.author().clone());
    let user_i64 = target.id.get() as i64;

    let stats: Option<(i64, i64, i64, i64)> = sqlx::query_as(
        "SELECT total, departed, fake, bonus FROM invite_stats WHERE guild_id = ? AND user_id = ?",
    )
    .bind(guild_id)
    .bind(user_i64)
    .fetch_optional(&ctx.data().db)
    .await?;

    let (total, departed, fake, bonus) = stats.unwrap_or((0, 0, 0, 0));
    let net = total + bonus - departed - fake;

    let still_here: Vec<i64> = sqlx::query_scalar(
        "SELECT user_id FROM members
         WHERE guild_id = ? AND inviter_id = ? AND (last_leave IS NULL OR last_leave < last_join)
         ORDER BY last_join DESC LIMIT 10",
    )
    .bind(guild_id)
    .bind(user_i64)
    .fetch_all(&ctx.data().db)
    .await?;

    let recent = if still_here.is_empty() {
        "-".to_string()
    } else {
        still_here
            .iter()
            .map(|id| format!("<@{id}>"))
            .collect::<Vec<_>>()
            .join(", ")
    };

    ctx.send(
        poise::CreateReply::default().embed(
            embed::titled(format!("Invites · {}", target.tag()), embed::INFO)
                .description(format!("**{net}** effective invite(s)"))
                .field("Joined", total.to_string(), true)
                .field("Left again", departed.to_string(), true)
                .field("Bonus", bonus.to_string(), true)
                .field("Still in the server", recent, false)
                .thumbnail(target.face()),
        ),
    )
    .await?;
    Ok(())
}

/// Top inviters in this server.
#[poise::command(slash_command, guild_only, category = "Community")]
pub async fn inviteboard(ctx: Ctx<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let rows: Vec<(i64, i64, i64, i64)> = sqlx::query_as(
        "SELECT user_id, total, departed, bonus FROM invite_stats
         WHERE guild_id = ? ORDER BY (total + bonus - departed) DESC LIMIT 15",
    )
    .bind(guild_id)
    .fetch_all(&ctx.data().db)
    .await?;

    let body = if rows.is_empty() {
        "Nobody has invited anyone yet, or I am missing the **Manage Server** permission needed to read invites."
            .to_string()
    } else {
        rows.iter()
            .enumerate()
            .map(|(i, (user_id, total, departed, bonus))| {
                let medal = match i {
                    0 => "🥇",
                    1 => "🥈",
                    2 => "🥉",
                    _ => "▫️",
                };
                format!(
                    "{medal} **{}.** <@{user_id}> — **{}** net ({total} joined, {departed} left)",
                    i + 1,
                    total + bonus - departed
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    ctx.send(
        poise::CreateReply::default()
            .embed(embed::titled("🎟️ Invite leaderboard", embed::BRAND).description(body)),
    )
    .await?;
    Ok(())
}

/// Give or take bonus invites.
#[poise::command(
    slash_command,
    guild_only,
    category = "Community",
    check = "perms::admin_only"
)]
pub async fn bonusinvites(
    ctx: Ctx<'_>,
    #[description = "Member"] user: User,
    #[description = "Bonus to add (negative to subtract)"] amount: i64,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    sqlx::query(
        "INSERT INTO invite_stats (guild_id, user_id, bonus) VALUES (?, ?, ?)
         ON CONFLICT(guild_id, user_id) DO UPDATE SET bonus = invite_stats.bonus + ?",
    )
    .bind(guild_id)
    .bind(user.id.get() as i64)
    .bind(amount)
    .bind(amount)
    .execute(&ctx.data().db)
    .await?;

    ctx.send(poise::CreateReply::default().embed(embed::ok(format!(
        "Adjusted bonus invites for {} by **{amount}**.",
        user.tag()
    ))))
    .await?;
    Ok(())
}

/// Details about a specific invite code.
#[poise::command(slash_command, guild_only, category = "Utility")]
pub async fn inviteinfo(
    ctx: Ctx<'_>,
    #[description = "Invite code or link"] code: String,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap();
    let clean = code
        .trim()
        .rsplit('/')
        .next()
        .unwrap_or(code.trim())
        .to_string();

    let invites = match guild_id.invites(ctx.http()).await {
        Ok(list) => list,
        Err(_) => {
            ctx.send(poise::CreateReply::default().embed(embed::err(
                "I need the **Manage Server** permission to read invites.",
            )))
            .await?;
            return Ok(());
        }
    };

    let Some(invite) = invites.iter().find(|i| i.code == clean) else {
        ctx.send(
            poise::CreateReply::default()
                .embed(embed::err("No invite with that code exists in this server.")),
        )
        .await?;
        return Ok(());
    };

    let joined: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM members WHERE guild_id = ? AND invite_code = ?",
    )
    .bind(guild_id.get() as i64)
    .bind(&clean)
    .fetch_one(&ctx.data().db)
    .await?;

    ctx.send(
        poise::CreateReply::default().embed(
            embed::titled(format!("Invite `{}`", invite.code), embed::INFO)
                .field("Uses", invite.uses.to_string(), true)
                .field(
                    "Created by",
                    invite
                        .inviter
                        .as_ref()
                        .map(|u| format!("<@{}>", u.id.get()))
                        .unwrap_or_else(|| "unknown".to_string()),
                    true,
                )
                .field("Channel", format!("<#{}>", invite.channel.id.get()), true)
                .field("Tracked joins", joined.to_string(), true)
                .field(
                    "Max uses",
                    if invite.max_uses == 0 {
                        "unlimited".to_string()
                    } else {
                        invite.max_uses.to_string()
                    },
                    true,
                )
                .field(
                    "Temporary membership",
                    if invite.temporary { "yes" } else { "no" },
                    true,
                ),
        ),
    )
    .await?;
    Ok(())
}

pub fn commands() -> Vec<poise::Command<Data, Error>> {
    vec![invites(), inviteboard(), bonusinvites(), inviteinfo()]
}
