//! Anonymous feedback sent privately to the staff team.

use poise::serenity_prelude as serenity;
use serenity::{CreateMessage, GuildChannel};

use crate::db;
use crate::state::{Ctx, Error};
use crate::util::{embed, perms, text};

/// Anonymous feedback straight to the staff team.
#[poise::command(slash_command, guild_only, category = "Community", ephemeral)]
pub async fn feedback(
    ctx: Ctx<'_>,
    #[description = "What would you like staff to know?"] message: String,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;

    sqlx::query("INSERT INTO anon_feedback (guild_id, content) VALUES (?, ?)")
        .bind(guild_id)
        .bind(&message)
        .execute(&ctx.data().db)
        .await?;

    // Deliberately no author field: this is anonymous by design. The row in
    // anon_feedback carries no user id either.
    let target = db::kv_get(&ctx.data().db, guild_id, "feedback_channel")
        .await
        .and_then(|c| c.parse::<u64>().ok())
        .map(serenity::ChannelId::new);

    let notice = embed::titled("📨 Anonymous feedback", embed::INFO)
        .description(text::truncate(&message, 3000))
        .footer(embed::footer("Sent anonymously - no author is recorded."));

    if let Some(channel) = target {
        channel
            .send_message(ctx.http(), CreateMessage::new().embed(notice))
            .await?;
    } else {
        crate::logs::send(
            ctx.serenity_context(),
            &ctx.data().db,
            guild_id,
            crate::logs::Log::Mod,
            notice,
        )
        .await;
    }

    ctx.send(
        poise::CreateReply::default()
            .embed(embed::ok(
                "Sent. Staff will see the message but not who wrote it.",
            ))
            .ephemeral(true),
    )
    .await?;
    Ok(())
}

/// Choose where anonymous feedback is delivered.
#[poise::command(
    slash_command,
    guild_only,
    category = "Community",
    check = "perms::admin_only"
)]
pub async fn feedbackchannel(
    ctx: Ctx<'_>,
    #[description = "Channel for anonymous feedback"] channel: GuildChannel,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    db::kv_set(
        &ctx.data().db,
        guild_id,
        "feedback_channel",
        &channel.id.get().to_string(),
    )
    .await?;
    ctx.send(poise::CreateReply::default().embed(embed::ok(format!(
        "Anonymous feedback will go to <#{}>.",
        channel.id.get()
    ))))
    .await?;
    Ok(())
}
