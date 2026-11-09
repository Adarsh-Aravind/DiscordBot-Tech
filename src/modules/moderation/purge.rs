//! Bulk message deletion with filters.

use poise::serenity_prelude as serenity;
use serenity::{GetMessages, Message, User};

use crate::compat;
use crate::logs::{self, Log};
use crate::state::{Ctx, Data, Error};
use crate::util::{embed, perms, text};

#[derive(Debug, poise::ChoiceParameter)]
pub enum PurgeFilter {
    #[name = "everything"]
    All,
    #[name = "bots only"]
    Bots,
    #[name = "humans only"]
    Humans,
    #[name = "with attachments"]
    Attachments,
    #[name = "with embeds"]
    Embeds,
    #[name = "with links"]
    Links,
    #[name = "with invites"]
    Invites,
    #[name = "pinned excluded"]
    Unpinned,
}

fn keep(message: &Message, filter: &PurgeFilter, user_id: Option<u64>, contains: Option<&str>) -> bool {
    if let Some(id) = user_id {
        if message.author.id.get() != id {
            return false;
        }
    }
    if let Some(needle) = contains {
        if !message
            .content
            .to_lowercase()
            .contains(&needle.to_lowercase())
        {
            return false;
        }
    }
    match filter {
        PurgeFilter::All => true,
        PurgeFilter::Bots => message.author.bot,
        PurgeFilter::Humans => !message.author.bot,
        PurgeFilter::Attachments => !message.attachments.is_empty(),
        PurgeFilter::Embeds => !message.embeds.is_empty(),
        PurgeFilter::Links => !text::find_domains(&message.content).is_empty(),
        PurgeFilter::Invites => !text::find_invites(&message.content).is_empty(),
        PurgeFilter::Unpinned => !message.pinned,
    }
}

/// Delete recent messages in this channel, with optional filters.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    category = "Moderation",
    required_permissions = "MANAGE_MESSAGES",
    default_member_permissions = "MANAGE_MESSAGES",
    aliases("clear", "prune")
)]
pub async fn purge(
    ctx: Ctx<'_>,
    #[description = "How many recent messages to examine (1-200)"]
    #[min = 1]
    #[max = 200]
    amount: u16,
    #[description = "Only delete this member's messages"] user: Option<User>,
    #[description = "Only delete messages containing this text"] contains: Option<String>,
    #[description = "Filter by message type"] filter: Option<PurgeFilter>,
) -> Result<(), Error> {
    let Some(guild_id) = ctx.guild_id() else {
        return Ok(());
    };
    ctx.defer_ephemeral().await?;

    let filter = filter.unwrap_or(PurgeFilter::All);
    let channel = ctx.channel_id();
    let http = ctx.http();

    // Walk back through history in pages of 100 until we have enough.
    let mut candidates: Vec<Message> = Vec::new();
    let mut before: Option<serenity::MessageId> = None;
    let mut examined = 0u16;

    while examined < amount {
        let batch_size = (amount - examined).min(100) as u8;
        let mut request = GetMessages::new().limit(batch_size);
        if let Some(id) = before {
            request = request.before(id);
        }
        let batch = channel.messages(http, request).await?;
        if batch.is_empty() {
            break;
        }
        examined += batch.len() as u16;
        before = batch.last().map(|m| m.id);
        candidates.extend(batch);
    }

    let user_id = user.as_ref().map(|u| u.id.get());
    let targets: Vec<serenity::MessageId> = candidates
        .iter()
        .filter(|m| keep(m, &filter, user_id, contains.as_deref()))
        .filter(|m| !m.pinned)
        .map(|m| m.id)
        .collect();

    if targets.is_empty() {
        ctx.send(
            poise::CreateReply::default()
                .embed(embed::warn("Nothing matched those filters."))
                .ephemeral(true),
        )
        .await?;
        return Ok(());
    }

    let deleted = compat::bulk_delete(&ctx.serenity_context().http, channel, &targets).await?;

    let mut detail = format!("Deleted **{deleted}** message(s) in <#{}>.", channel.get());
    if let Some(u) = &user {
        detail.push_str(&format!("\nAuthor filter: {}", u.tag()));
    }
    if let Some(c) = &contains {
        detail.push_str(&format!("\nText filter: `{}`", text::truncate(c, 60)));
    }

    ctx.send(
        poise::CreateReply::default()
            .embed(embed::ok(detail.clone()))
            .ephemeral(true),
    )
    .await?;

    logs::send(
        ctx.serenity_context(),
        &ctx.data().db,
        guild_id.get() as i64,
        Log::Message,
        embed::titled("🧹 Messages purged", embed::WARN)
            .description(detail)
            .footer(embed::footer(format!("By {}", ctx.author().tag()))),
    )
    .await;

    Ok(())
}

/// Delete every message from a member across the last 100 messages of each
/// channel they were active in. Useful after a spam raid.
#[poise::command(
    slash_command,
    guild_only,
    category = "Moderation",
    check = "perms::admin_only"
)]
pub async fn purgeuser(
    ctx: Ctx<'_>,
    #[description = "Member whose messages should be removed"] user: User,
    #[description = "How many recent messages per channel to scan (max 100)"]
    #[min = 10]
    #[max = 100]
    depth: Option<u8>,
) -> Result<(), Error> {
    let Some(guild_id) = ctx.guild_id() else {
        return Ok(());
    };
    ctx.defer_ephemeral().await?;

    let depth = depth.unwrap_or(50);
    let http = ctx.http();

    // Channel list is copied out of the cache first so no guard is held.
    let channels: Vec<serenity::ChannelId> = {
        match guild_id.to_guild_cached(&ctx.serenity_context().cache) {
            Some(guild) => guild
                .channels
                .iter()
                .filter(|(_, c)| c.kind == serenity::ChannelType::Text)
                .map(|(id, _)| *id)
                .collect(),
            None => vec![ctx.channel_id()],
        }
    };

    let mut total = 0usize;
    for channel in channels {
        let Ok(messages) = channel.messages(http, GetMessages::new().limit(depth)).await else {
            continue;
        };
        let ids: Vec<serenity::MessageId> = messages
            .iter()
            .filter(|m| m.author.id == user.id && !m.pinned)
            .map(|m| m.id)
            .collect();
        if ids.is_empty() {
            continue;
        }
        if let Ok(count) =
            compat::bulk_delete(&ctx.serenity_context().http, channel, &ids).await
        {
            total += count;
        }
    }

    let detail = format!(
        "Removed **{total}** message(s) from {} across the server.",
        user.tag()
    );
    ctx.send(
        poise::CreateReply::default()
            .embed(embed::ok(detail.clone()))
            .ephemeral(true),
    )
    .await?;

    logs::send(
        ctx.serenity_context(),
        &ctx.data().db,
        guild_id.get() as i64,
        Log::Message,
        embed::titled("🧹 User purge", embed::WARN)
            .description(detail)
            .footer(embed::footer(format!("By {}", ctx.author().tag()))),
    )
    .await;
    Ok(())
}

pub fn commands() -> Vec<poise::Command<Data, Error>> {
    vec![purge(), purgeuser()]
}
