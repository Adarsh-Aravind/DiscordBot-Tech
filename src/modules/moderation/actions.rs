//! The punishment commands. All of them delegate to [`super::punish`].

use poise::serenity_prelude as serenity;
use serenity::{User, UserId};

use super::{punish, Action, PunishRequest};
use crate::state::{Ctx, Data, Error};
use crate::util::{embed, perms, time};

/// Accept a raw id, a `<@id>` mention or `<@!id>`.
pub fn parse_user_id(raw: &str) -> Option<UserId> {
    let cleaned: String = raw
        .trim()
        .trim_start_matches("<@")
        .trim_start_matches('!')
        .trim_end_matches('>')
        .chars()
        .filter(|c| c.is_ascii_digit())
        .collect();
    cleaned.parse::<u64>().ok().map(UserId::new)
}

async fn run(
    ctx: Ctx<'_>,
    target: UserId,
    target_tag: String,
    action: Action,
    reason: Option<String>,
    duration: Option<String>,
    delete_days: Option<u8>,
) -> Result<(), Error> {
    let Some(guild_id) = ctx.guild_id() else {
        return Ok(());
    };
    ctx.defer().await?;

    // Hierarchy and self-target guards (skipped for unban: the user is not a member).
    if action != Action::Unban {
        if let Err(why) =
            perms::can_moderate(ctx.serenity_context(), guild_id, ctx.author().id, target).await
        {
            ctx.send(poise::CreateReply::default().embed(embed::err(why)))
                .await?;
            return Ok(());
        }
    }

    let duration_secs = match duration.as_deref() {
        Some(raw) if !raw.trim().is_empty() => match time::parse_duration(raw) {
            Some(d) => Some(d.as_secs() as i64),
            None => {
                ctx.send(poise::CreateReply::default().embed(embed::err(
                    "I could not read that duration. Try `10m`, `2h`, `7d` or `1h30m`.",
                )))
                .await?;
                return Ok(());
            }
        },
        _ => None,
    };

    if action == Action::Timeout && duration_secs.is_none() {
        ctx.send(
            poise::CreateReply::default()
                .embed(embed::err("A timeout needs a duration, for example `30m`.")),
        )
        .await?;
        return Ok(());
    }

    let req = PunishRequest {
        guild_id,
        target,
        target_tag,
        moderator: ctx.author().id,
        moderator_tag: ctx.author().tag(),
        action,
        reason: reason.unwrap_or_else(|| "No reason provided".to_string()),
        duration: duration_secs,
        delete_days: delete_days.unwrap_or(0),
        automated: false,
        silent: false,
    };

    match punish(ctx.serenity_context(), ctx.data(), req).await {
        Ok(outcome) => {
            let mut description = format!(
                "{} <@{}> was **{}**.",
                action.emoji(),
                target.get(),
                action.past_tense()
            );
            if let Some(secs) = duration_secs {
                description.push_str(&format!(" Duration: `{}`.", time::humanize(secs)));
            }
            if !outcome.dm_delivered && !matches!(action, Action::Note | Action::Unban) {
                description.push_str("\n*Could not DM them - their DMs are closed.*");
            }
            if let Some(extra) = outcome.escalated {
                description.push_str(&format!("\n⚡ Escalation applied: **{extra}**."));
            }
            ctx.send(
                poise::CreateReply::default().embed(
                    embed::titled(format!("Case #{}", outcome.case_no), action.colour())
                        .description(description),
                ),
            )
            .await?;
        }
        Err(why) => {
            ctx.send(poise::CreateReply::default().embed(embed::err(format!(
                "That did not work: {why}\nCheck that I have the right permission and that my role is above theirs."
            ))))
            .await?;
        }
    }
    Ok(())
}

/// Ban a member, optionally for a limited time.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    category = "Moderation",
    required_permissions = "BAN_MEMBERS",
    default_member_permissions = "BAN_MEMBERS"
)]
pub async fn ban(
    ctx: Ctx<'_>,
    #[description = "Member to ban"] user: User,
    #[description = "Why are they being banned?"] reason: Option<String>,
    #[description = "Temporary ban length, e.g. 7d. Leave empty for permanent."] duration: Option<
        String,
    >,
    #[description = "Days of their recent messages to delete (0-7)"]
    #[min = 0]
    #[max = 7]
    delete_days: Option<u8>,
) -> Result<(), Error> {
    let tag = user.tag();
    run(
        ctx,
        user.id,
        tag,
        Action::Ban,
        reason,
        duration,
        delete_days,
    )
    .await
}

/// Ban by user ID, for members who already left.
#[poise::command(
    slash_command,
    guild_only,
    category = "Moderation",
    required_permissions = "BAN_MEMBERS",
    default_member_permissions = "BAN_MEMBERS"
)]
pub async fn hackban(
    ctx: Ctx<'_>,
    #[description = "User ID to ban"] user_id: String,
    #[description = "Why are they being banned?"] reason: Option<String>,
) -> Result<(), Error> {
    let Some(target) = parse_user_id(&user_id) else {
        ctx.send(poise::CreateReply::default().embed(embed::err("That is not a valid user ID.")))
            .await?;
        return Ok(());
    };
    run(
        ctx,
        target,
        format!("ID {}", target.get()),
        Action::Ban,
        reason,
        None,
        Some(0),
    )
    .await
}

/// Lift a ban.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    category = "Moderation",
    required_permissions = "BAN_MEMBERS",
    default_member_permissions = "BAN_MEMBERS"
)]
pub async fn unban(
    ctx: Ctx<'_>,
    #[description = "User ID of the banned account"] user_id: String,
    #[description = "Why is the ban being lifted?"] reason: Option<String>,
) -> Result<(), Error> {
    let Some(target) = parse_user_id(&user_id) else {
        ctx.send(poise::CreateReply::default().embed(embed::err("That is not a valid user ID.")))
            .await?;
        return Ok(());
    };
    run(
        ctx,
        target,
        format!("ID {}", target.get()),
        Action::Unban,
        reason,
        None,
        None,
    )
    .await
}

/// Ban then immediately unban, which clears the member's recent messages.
#[poise::command(
    slash_command,
    guild_only,
    category = "Moderation",
    required_permissions = "BAN_MEMBERS",
    default_member_permissions = "BAN_MEMBERS"
)]
pub async fn softban(
    ctx: Ctx<'_>,
    #[description = "Member to softban"] user: User,
    #[description = "Reason"] reason: Option<String>,
    #[description = "Days of messages to clear (1-7)"]
    #[min = 1]
    #[max = 7]
    delete_days: Option<u8>,
) -> Result<(), Error> {
    let tag = user.tag();
    run(
        ctx,
        user.id,
        tag,
        Action::Softban,
        reason,
        None,
        Some(delete_days.unwrap_or(1)),
    )
    .await
}

/// Remove a member from the server. They can rejoin with a new invite.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    category = "Moderation",
    required_permissions = "KICK_MEMBERS",
    default_member_permissions = "KICK_MEMBERS"
)]
pub async fn kick(
    ctx: Ctx<'_>,
    #[description = "Member to kick"] user: User,
    #[description = "Reason"] reason: Option<String>,
) -> Result<(), Error> {
    let tag = user.tag();
    run(ctx, user.id, tag, Action::Kick, reason, None, None).await
}

/// Time a member out so they cannot talk or join voice.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    category = "Moderation",
    required_permissions = "MODERATE_MEMBERS",
    default_member_permissions = "MODERATE_MEMBERS"
)]
pub async fn timeout(
    ctx: Ctx<'_>,
    #[description = "Member to time out"] user: User,
    #[description = "How long, e.g. 10m, 2h, 1d (max 28d)"] duration: String,
    #[description = "Reason"] reason: Option<String>,
) -> Result<(), Error> {
    let tag = user.tag();
    run(
        ctx,
        user.id,
        tag,
        Action::Timeout,
        reason,
        Some(duration),
        None,
    )
    .await
}

/// End a member's timeout early.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    category = "Moderation",
    required_permissions = "MODERATE_MEMBERS",
    default_member_permissions = "MODERATE_MEMBERS"
)]
pub async fn untimeout(
    ctx: Ctx<'_>,
    #[description = "Member whose timeout should end"] user: User,
    #[description = "Reason"] reason: Option<String>,
) -> Result<(), Error> {
    let tag = user.tag();
    run(ctx, user.id, tag, Action::Untimeout, reason, None, None).await
}

/// Warn a member. Warnings feed the escalation ladder.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    category = "Moderation",
    check = "perms::staff_only"
)]
pub async fn warn(
    ctx: Ctx<'_>,
    #[description = "Member to warn"] user: User,
    #[description = "What did they do?"] reason: String,
) -> Result<(), Error> {
    let tag = user.tag();
    run(ctx, user.id, tag, Action::Warn, Some(reason), None, None).await
}

/// Attach a private staff note to a member. No DM, no punishment.
#[poise::command(
    slash_command,
    guild_only,
    category = "Moderation",
    check = "perms::staff_only"
)]
pub async fn note(
    ctx: Ctx<'_>,
    #[description = "Member the note is about"] user: User,
    #[description = "Note content"] note: String,
) -> Result<(), Error> {
    let tag = user.tag();
    run(ctx, user.id, tag, Action::Note, Some(note), None, None).await
}

/// Set the per-channel slowmode.
#[poise::command(
    slash_command,
    guild_only,
    category = "Moderation",
    required_permissions = "MANAGE_CHANNELS",
    default_member_permissions = "MANAGE_CHANNELS"
)]
pub async fn slowmode(
    ctx: Ctx<'_>,
    #[description = "Seconds between messages, 0 to switch off (max 21600)"]
    #[min = 0]
    #[max = 21600]
    seconds: u16,
    #[description = "Channel, defaults to this one"] channel: Option<serenity::GuildChannel>,
) -> Result<(), Error> {
    let target = channel.map(|c| c.id).unwrap_or_else(|| ctx.channel_id());
    target
        .edit(
            ctx.http(),
            serenity::EditChannel::new().rate_limit_per_user(seconds),
        )
        .await?;

    let body = if seconds == 0 {
        format!("Slowmode is off in <#{}>.", target.get())
    } else {
        format!(
            "Slowmode in <#{}> is now **{}** between messages.",
            target.get(),
            time::humanize(seconds as i64)
        )
    };
    ctx.send(poise::CreateReply::default().embed(embed::ok(body)))
        .await?;
    Ok(())
}

pub fn commands() -> Vec<poise::Command<Data, Error>> {
    vec![
        ban(),
        hackban(),
        unban(),
        softban(),
        kick(),
        timeout(),
        untimeout(),
        warn(),
        note(),
        slowmode(),
    ]
}
