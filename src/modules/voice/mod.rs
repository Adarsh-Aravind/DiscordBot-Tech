//! Voice: join-to-create temporary rooms, their controls, session tracking and
//! voice statistics.

use std::time::Instant;

use poise::serenity_prelude as serenity;
use serenity::{
    ChannelId, ChannelType, CreateChannel, EditChannel, EditMember, GuildChannel, GuildId,
    PermissionOverwrite, PermissionOverwriteType, Permissions, RoleId, User, UserId, VoiceState,
};

use crate::db::{self, Db};
use crate::logs::{self, Log};
use crate::state::{Ctx, Data, Error};
use crate::util::{embed, perms, text, time};

// ===========================================================================
// Voice state handling
// ===========================================================================

pub async fn handle_voice_state(
    sctx: &serenity::Context,
    data: &Data,
    old: Option<&VoiceState>,
    new: &VoiceState,
) -> Result<(), Error> {
    let Some(guild_id) = new.guild_id.or_else(|| old.and_then(|o| o.guild_id)) else {
        return Ok(());
    };
    let guild_i64 = guild_id.get() as i64;
    let user_id = new.user_id;

    let old_channel = old.and_then(|o| o.channel_id);
    let new_channel = new.channel_id;

    if old_channel == new_channel {
        return Ok(()); // mute/deaf/stream change, nothing to do
    }

    let cfg = db::voice_config(&data.db, guild_i64).await?;

    // ---- left a channel --------------------------------------------------
    if let Some(left) = old_channel {
        close_session(sctx, data, guild_id, user_id, left).await;

        logs::send(
            sctx,
            &data.db,
            guild_i64,
            Log::Voice,
            embed::titled("🔈 Left voice", embed::MUTED)
                .description(format!("<@{}> left <#{}>", user_id.get(), left.get())),
        )
        .await;

        maybe_delete_temp(sctx, data, guild_id, left, &cfg).await;
    }

    // ---- joined a channel ------------------------------------------------
    if let Some(joined) = new_channel {
        // Somebody is back before the grace period ran out, so the room is not
        // going anywhere.
        data.pending_voice_delete.remove(&joined.get());

        data.voice_since
            .insert((guild_i64 as u64, user_id.get()), Instant::now());

        sqlx::query(
            "INSERT INTO voice_sessions (guild_id, user_id, channel_id, joined_at)
             VALUES (?, ?, ?, ?)",
        )
        .bind(guild_i64)
        .bind(user_id.get() as i64)
        .bind(joined.get() as i64)
        .bind(time::sql_now())
        .execute(&data.db)
        .await?;

        logs::send(
            sctx,
            &data.db,
            guild_i64,
            Log::Voice,
            embed::titled("🔊 Joined voice", embed::SUCCESS)
                .description(format!("<@{}> joined <#{}>", user_id.get(), joined.get())),
        )
        .await;

        // Join to create.
        if Some(joined.get() as i64) == cfg.j2c_channel {
            create_temp_room(sctx, data, guild_id, user_id, &cfg).await;
        }
    }

    Ok(())
}

/// Close the open session row and award voice XP for the time spent.
async fn close_session(
    sctx: &serenity::Context,
    data: &Data,
    guild_id: GuildId,
    user_id: UserId,
    _channel: ChannelId,
) {
    let guild_i64 = guild_id.get() as i64;
    let key = (guild_i64 as u64, user_id.get());

    let seconds = match data.voice_since.remove(&key) {
        Some((_, since)) => since.elapsed().as_secs() as i64,
        None => 0,
    };

    let _ = sqlx::query(
        "UPDATE voice_sessions SET left_at = ?, duration = ?
         WHERE id = (
            SELECT id FROM voice_sessions
            WHERE guild_id = ? AND user_id = ? AND left_at IS NULL
            ORDER BY id DESC LIMIT 1
         )",
    )
    .bind(time::sql_now())
    .bind(seconds)
    .bind(guild_i64)
    .bind(user_id.get() as i64)
    .execute(&data.db)
    .await;

    if seconds >= 60 {
        let _ = crate::modules::leveling::award_voice(sctx, data, guild_id, user_id, seconds).await;
        crate::modules::community::misc::bump_challenges(
            data,
            guild_i64,
            user_id.get() as i64,
            "voice",
            seconds / 60,
        )
        .await;
    }
}

/// Create a personal room for someone who joined the hub channel.
async fn create_temp_room(
    sctx: &serenity::Context,
    data: &Data,
    guild_id: GuildId,
    user_id: UserId,
    cfg: &db::VoiceConfig,
) {
    let guild_i64 = guild_id.get() as i64;

    let name = {
        let display = guild_id
            .to_guild_cached(&sctx.cache)
            .and_then(|g| g.members.get(&user_id).map(|m| m.display_name().to_string()))
            .unwrap_or_else(|| "member".to_string());
        text::truncate(
            &text::render(&cfg.default_name, &[("user", display)]),
            95,
        )
    };

    let mut builder = CreateChannel::new(name.clone())
        .kind(ChannelType::Voice)
        .permissions(vec![PermissionOverwrite {
            allow: Permissions::MANAGE_CHANNELS
                | Permissions::MOVE_MEMBERS
                | Permissions::MUTE_MEMBERS
                | Permissions::CONNECT,
            deny: Permissions::empty(),
            kind: PermissionOverwriteType::Member(user_id),
        }]);

    if cfg.default_limit > 0 {
        builder = builder.user_limit(cfg.default_limit.clamp(0, 99) as u32);
    }
    if let Some(category) = cfg.j2c_category {
        builder = builder.category(ChannelId::new(category as u64));
    }

    let Ok(channel) = guild_id.create_channel(&sctx.http, builder).await else {
        tracing::warn!(guild = guild_i64, "could not create a temp voice room");
        return;
    };

    let _ = sqlx::query(
        "INSERT INTO temp_voice (channel_id, guild_id, owner_id, name, user_limit)
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(channel.id.get() as i64)
    .bind(guild_i64)
    .bind(user_id.get() as i64)
    .bind(&name)
    .bind(cfg.default_limit)
    .execute(&data.db)
    .await;

    // Move them into their new room.
    let _ = guild_id
        .edit_member(
            &sctx.http,
            user_id,
            EditMember::new().voice_channel(channel.id),
        )
        .await;

    logs::send(
        sctx,
        &data.db,
        guild_i64,
        Log::Voice,
        embed::titled("➕ Temporary room created", embed::INFO)
            .description(format!(
                "<#{}> for <@{}>\nThey can manage it with `/voice`.",
                channel.id.get(),
                user_id.get()
            )),
    )
    .await;
}

/// How many people are in a voice channel, read from the cache.
fn occupancy(sctx: &serenity::Context, guild_id: GuildId, channel: ChannelId) -> usize {
    match guild_id.to_guild_cached(&sctx.cache) {
        Some(guild) => guild
            .voice_states
            .values()
            .filter(|state| state.channel_id == Some(channel))
            .count(),
        None => 1,
    }
}

/// Delete a temporary room once it empties out, after the configured grace.
///
/// `empty_grace` is in the config and in the schema but nothing ever read it,
/// so a room vanished the instant its last member left - including when they
/// were only hopping to another channel and straight back.
async fn maybe_delete_temp(
    sctx: &serenity::Context,
    data: &Data,
    guild_id: GuildId,
    channel: ChannelId,
    cfg: &db::VoiceConfig,
) {
    if cfg.delete_empty == 0 {
        return;
    }
    if !is_temp_room(data, channel).await {
        return;
    }
    if occupancy(sctx, guild_id, channel) > 0 {
        return;
    }

    if cfg.empty_grace > 0 {
        // Hand it to the sweep, which checks again when the grace is up.
        data.pending_voice_delete
            .entry(channel.get())
            .or_insert_with(Instant::now);
        return;
    }

    delete_temp_room(sctx, data, channel).await;
}

async fn is_temp_room(data: &Data, channel: ChannelId) -> bool {
    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM temp_voice WHERE channel_id = ?")
        .bind(channel.get() as i64)
        .fetch_one(&data.db)
        .await
        .unwrap_or(0)
        > 0
}

async fn delete_temp_room(sctx: &serenity::Context, data: &Data, channel: ChannelId) {
    data.pending_voice_delete.remove(&channel.get());
    if crate::compat::delete_channel(&sctx.http, channel, "Temporary voice room is empty")
        .await
        .is_ok()
    {
        let _ = sqlx::query("DELETE FROM temp_voice WHERE channel_id = ?")
            .bind(channel.get() as i64)
            .execute(&data.db)
            .await;
    }
}

/// Delete the temporary rooms whose grace period has run out and which are
/// still empty. Runs on the fast loop, since the grace is usually seconds.
pub async fn sweep_pending_deletes(sctx: &serenity::Context, data: &Data) {
    let pending: Vec<(u64, Instant)> = data
        .pending_voice_delete
        .iter()
        .map(|entry| (*entry.key(), *entry.value()))
        .collect();

    for (channel_raw, since) in pending {
        let channel = ChannelId::new(channel_raw);

        let row: Option<i64> =
            sqlx::query_scalar("SELECT guild_id FROM temp_voice WHERE channel_id = ?")
                .bind(channel_raw as i64)
                .fetch_optional(&data.db)
                .await
                .ok()
                .flatten();

        // Not a temporary room any more: nothing to wait for.
        let Some(guild_i64) = row else {
            data.pending_voice_delete.remove(&channel_raw);
            continue;
        };
        let guild_id = GuildId::new(guild_i64 as u64);

        if occupancy(sctx, guild_id, channel) > 0 {
            data.pending_voice_delete.remove(&channel_raw);
            continue;
        }

        let grace = db::voice_config(&data.db, guild_i64)
            .await
            .map(|cfg| cfg.empty_grace.max(0) as u64)
            .unwrap_or(10);
        if since.elapsed().as_secs() < grace {
            continue;
        }

        delete_temp_room(sctx, data, channel).await;
    }
}

/// Grant voice XP to everyone currently sitting in voice. Called on a timer so
/// long sessions accrue XP without waiting for the member to leave.
pub async fn tick_voice_xp(sctx: &serenity::Context, data: &Data) {
    // Collect first so no DashMap guard is held across an await.
    let entries: Vec<((u64, u64), u64)> = data
        .voice_since
        .iter()
        .map(|entry| (*entry.key(), entry.value().elapsed().as_secs()))
        .collect();

    for ((guild_raw, user_raw), seconds) in entries {
        if seconds < 300 {
            continue;
        }
        let guild_id = GuildId::new(guild_raw);
        let user_id = UserId::new(user_raw);

        // Reset the clock so the time is not counted twice.
        data.voice_since
            .insert((guild_raw, user_raw), Instant::now());

        let _ =
            crate::modules::leveling::award_voice(sctx, data, guild_id, user_id, seconds as i64)
                .await;
    }
}

/// Move idle members to the AFK channel.
pub async fn sweep_afk(sctx: &serenity::Context, data: &Data) {
    let configs: Vec<(i64, Option<i64>, i64)> =
        sqlx::query_as("SELECT guild_id, afk_channel, afk_timeout FROM voice_config")
            .fetch_all(&data.db)
            .await
            .unwrap_or_default();

    for (guild_i64, afk_channel, timeout) in configs {
        let Some(afk_channel) = afk_channel else {
            continue;
        };
        if timeout <= 0 {
            continue;
        }
        let guild_id = GuildId::new(guild_i64 as u64);

        // Who is self-muted and has been in voice longer than the timeout?
        let candidates: Vec<UserId> = {
            match guild_id.to_guild_cached(&sctx.cache) {
                Some(guild) => guild
                    .voice_states
                    .values()
                    .filter(|state| {
                        state.channel_id.map(|c| c.get() as i64) != Some(afk_channel)
                            && state.self_mute
                            && state.self_deaf
                    })
                    .map(|state| state.user_id)
                    .collect(),
                None => Vec::new(),
            }
        };

        for user_id in candidates {
            let elapsed = data
                .voice_since
                .get(&(guild_i64 as u64, user_id.get()))
                .map(|since| since.elapsed().as_secs() as i64)
                .unwrap_or(0);
            if elapsed < timeout {
                continue;
            }
            let _ = guild_id
                .edit_member(
                    &sctx.http,
                    user_id,
                    EditMember::new().voice_channel(ChannelId::new(afk_channel as u64)),
                )
                .await;
        }
    }
}

// ===========================================================================
// Owner controls
// ===========================================================================

async fn my_room(ctx: &Ctx<'_>) -> Option<(i64, i64)> {
    let guild_id = ctx.guild_id()?.get() as i64;
    let row: Option<(i64, i64)> = sqlx::query_as(
        "SELECT channel_id, owner_id FROM temp_voice WHERE guild_id = ? AND owner_id = ?",
    )
    .bind(guild_id)
    .bind(ctx.author().id.get() as i64)
    .fetch_optional(&ctx.data().db)
    .await
    .ok()
    .flatten();
    row
}

async fn require_room(ctx: &Ctx<'_>) -> Result<Option<ChannelId>, Error> {
    match my_room(ctx).await {
        Some((channel_id, _)) => Ok(Some(ChannelId::new(channel_id as u64))),
        None => {
            ctx.send(
                poise::CreateReply::default()
                    .embed(embed::err(
                        "You do not own a temporary voice room. Join the join-to-create channel to get one.",
                    ))
                    .ephemeral(true),
            )
            .await?;
            Ok(None)
        }
    }
}

/// Controls for your temporary voice room.
#[poise::command(
    slash_command,
    guild_only,
    category = "Voice",
    subcommands(
        "voice_setup",
        "voice_name",
        "voice_limit",
        "voice_lock",
        "voice_unlock",
        "voice_private",
        "voice_public",
        "voice_permit",
        "voice_reject",
        "voice_transfer",
        "voice_claim",
        "voice_info",
        "voice_stats"
    )
)]
pub async fn voice(_ctx: Ctx<'_>) -> Result<(), Error> {
    Ok(())
}

/// Set up join-to-create.
#[poise::command(slash_command, rename = "setup", guild_only, check = "perms::admin_only")]
pub async fn voice_setup(
    ctx: Ctx<'_>,
    #[description = "The voice channel members join to get a room"] hub: GuildChannel,
    #[description = "Category the rooms are created in"] category: Option<GuildChannel>,
    #[description = "Room name template, {user} is replaced"] name_template: Option<String>,
    #[description = "Default user limit (0 for unlimited)"]
    #[min = 0]
    #[max = 99]
    default_limit: Option<i64>,
    #[description = "AFK channel to move idle members to"] afk_channel: Option<GuildChannel>,
    #[description = "Seconds before an idle member is moved to AFK (0 to disable)"]
    #[min = 0]
    afk_timeout: Option<i64>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let dbh = &ctx.data().db;

    if hub.kind != ChannelType::Voice {
        ctx.send(
            poise::CreateReply::default()
                .embed(embed::err("The hub needs to be a voice channel.")),
        )
        .await?;
        return Ok(());
    }

    db::set_opt_i64(dbh, "voice_config", "j2c_channel", guild_id, Some(hub.id.get() as i64))
        .await?;
    db::set_opt_i64(
        dbh,
        "voice_config",
        "j2c_category",
        guild_id,
        category.as_ref().map(|c| c.id.get() as i64),
    )
    .await?;
    if let Some(template) = name_template {
        db::set_text(dbh, "voice_config", "default_name", guild_id, template).await?;
    }
    if let Some(limit) = default_limit {
        db::set_i64(dbh, "voice_config", "default_limit", guild_id, limit).await?;
    }
    if let Some(channel) = &afk_channel {
        db::set_opt_i64(
            dbh,
            "voice_config",
            "afk_channel",
            guild_id,
            Some(channel.id.get() as i64),
        )
        .await?;
    }
    if let Some(timeout) = afk_timeout {
        db::set_i64(dbh, "voice_config", "afk_timeout", guild_id, timeout).await?;
    }

    let cfg = db::voice_config(dbh, guild_id).await?;
    ctx.send(
        poise::CreateReply::default().embed(
            embed::titled("🔊 Voice configuration", embed::SUCCESS)
                .field("Hub channel", format!("<#{}>", hub.id.get()), true)
                .field(
                    "Rooms category",
                    cfg.j2c_category
                        .map(|c| format!("<#{c}>"))
                        .unwrap_or_else(|| "server root".into()),
                    true,
                )
                .field("Name template", format!("`{}`", cfg.default_name), false)
                .field(
                    "Default limit",
                    if cfg.default_limit == 0 {
                        "unlimited".to_string()
                    } else {
                        cfg.default_limit.to_string()
                    },
                    true,
                )
                .field(
                    "AFK",
                    cfg.afk_channel
                        .map(|c| format!("<#{c}> after {}", time::humanize(cfg.afk_timeout)))
                        .unwrap_or_else(|| "off".into()),
                    true,
                ),
        ),
    )
    .await?;
    Ok(())
}

/// Rename your room.
#[poise::command(slash_command, rename = "name", guild_only)]
pub async fn voice_name(
    ctx: Ctx<'_>,
    #[description = "New name"] name: String,
) -> Result<(), Error> {
    let Some(channel) = require_room(&ctx).await? else {
        return Ok(());
    };
    let clean = text::truncate(name.trim(), 95);

    channel
        .edit(ctx.http(), EditChannel::new().name(clean.clone()))
        .await?;
    sqlx::query("UPDATE temp_voice SET name = ? WHERE channel_id = ?")
        .bind(&clean)
        .bind(channel.get() as i64)
        .execute(&ctx.data().db)
        .await?;

    ctx.send(
        poise::CreateReply::default()
            .embed(embed::ok(format!("Your room is now **{clean}**.")))
            .ephemeral(true),
    )
    .await?;
    Ok(())
}

/// Set how many people can join your room.
#[poise::command(slash_command, rename = "limit", guild_only)]
pub async fn voice_limit(
    ctx: Ctx<'_>,
    #[description = "User limit, 0 for unlimited"]
    #[min = 0]
    #[max = 99]
    limit: u32,
) -> Result<(), Error> {
    let Some(channel) = require_room(&ctx).await? else {
        return Ok(());
    };

    channel
        .edit(ctx.http(), EditChannel::new().user_limit(limit))
        .await?;
    sqlx::query("UPDATE temp_voice SET user_limit = ? WHERE channel_id = ?")
        .bind(limit as i64)
        .bind(channel.get() as i64)
        .execute(&ctx.data().db)
        .await?;

    ctx.send(
        poise::CreateReply::default()
            .embed(embed::ok(if limit == 0 {
                "Your room has no user limit.".to_string()
            } else {
                format!("Your room is limited to **{limit}** people.")
            }))
            .ephemeral(true),
    )
    .await?;
    Ok(())
}

async fn set_everyone(
    ctx: &Ctx<'_>,
    channel: ChannelId,
    allow: Permissions,
    deny: Permissions,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap();
    crate::compat::set_permission(
            ctx.http(), channel,
            PermissionOverwrite {
                allow,
                deny,
                kind: PermissionOverwriteType::Role(RoleId::new(guild_id.get())),
            },
            Some("Temporary voice room control"),
        )
        .await?;
    Ok(())
}

/// Stop anyone new from joining.
#[poise::command(slash_command, rename = "lock", guild_only)]
pub async fn voice_lock(ctx: Ctx<'_>) -> Result<(), Error> {
    let Some(channel) = require_room(&ctx).await? else {
        return Ok(());
    };
    set_everyone(&ctx, channel, Permissions::empty(), Permissions::CONNECT).await?;
    sqlx::query("UPDATE temp_voice SET locked = 1 WHERE channel_id = ?")
        .bind(channel.get() as i64)
        .execute(&ctx.data().db)
        .await?;
    ctx.send(
        poise::CreateReply::default()
            .embed(embed::ok("🔒 Your room is locked. Use `/voice permit` to let someone in."))
            .ephemeral(true),
    )
    .await?;
    Ok(())
}

/// Let anyone join again.
#[poise::command(slash_command, rename = "unlock", guild_only)]
pub async fn voice_unlock(ctx: Ctx<'_>) -> Result<(), Error> {
    let Some(channel) = require_room(&ctx).await? else {
        return Ok(());
    };
    set_everyone(&ctx, channel, Permissions::empty(), Permissions::empty()).await?;
    sqlx::query("UPDATE temp_voice SET locked = 0 WHERE channel_id = ?")
        .bind(channel.get() as i64)
        .execute(&ctx.data().db)
        .await?;
    ctx.send(
        poise::CreateReply::default()
            .embed(embed::ok("🔓 Your room is open."))
            .ephemeral(true),
    )
    .await?;
    Ok(())
}

/// Hide your room from everyone else.
#[poise::command(slash_command, rename = "private", guild_only)]
pub async fn voice_private(ctx: Ctx<'_>) -> Result<(), Error> {
    let Some(channel) = require_room(&ctx).await? else {
        return Ok(());
    };
    set_everyone(
        &ctx,
        channel,
        Permissions::empty(),
        Permissions::VIEW_CHANNEL | Permissions::CONNECT,
    )
    .await?;
    sqlx::query("UPDATE temp_voice SET private = 1, locked = 1 WHERE channel_id = ?")
        .bind(channel.get() as i64)
        .execute(&ctx.data().db)
        .await?;
    ctx.send(
        poise::CreateReply::default()
            .embed(embed::ok("🙈 Your room is hidden and locked."))
            .ephemeral(true),
    )
    .await?;
    Ok(())
}

/// Make your room visible again.
#[poise::command(slash_command, rename = "public", guild_only)]
pub async fn voice_public(ctx: Ctx<'_>) -> Result<(), Error> {
    let Some(channel) = require_room(&ctx).await? else {
        return Ok(());
    };
    set_everyone(&ctx, channel, Permissions::empty(), Permissions::empty()).await?;
    sqlx::query("UPDATE temp_voice SET private = 0, locked = 0 WHERE channel_id = ?")
        .bind(channel.get() as i64)
        .execute(&ctx.data().db)
        .await?;
    ctx.send(
        poise::CreateReply::default()
            .embed(embed::ok("👋 Your room is public."))
            .ephemeral(true),
    )
    .await?;
    Ok(())
}

/// Let a specific member into your locked room.
#[poise::command(slash_command, rename = "permit", guild_only)]
pub async fn voice_permit(
    ctx: Ctx<'_>,
    #[description = "Member to let in"] user: User,
) -> Result<(), Error> {
    let Some(channel) = require_room(&ctx).await? else {
        return Ok(());
    };
    crate::compat::set_permission(
            ctx.http(), channel,
            PermissionOverwrite {
                allow: Permissions::VIEW_CHANNEL | Permissions::CONNECT,
                deny: Permissions::empty(),
                kind: PermissionOverwriteType::Member(user.id),
            },
            Some("Permitted into a temporary room"),
        )
        .await?;
    ctx.send(
        poise::CreateReply::default()
            .embed(embed::ok(format!("<@{}> can join your room.", user.id.get())))
            .ephemeral(true),
    )
    .await?;
    Ok(())
}

/// Remove someone from your room and block them from rejoining.
#[poise::command(slash_command, rename = "reject", guild_only)]
pub async fn voice_reject(
    ctx: Ctx<'_>,
    #[description = "Member to remove"] user: User,
) -> Result<(), Error> {
    let Some(channel) = require_room(&ctx).await? else {
        return Ok(());
    };
    let guild_id = ctx.guild_id().unwrap();

    if user.id == ctx.author().id {
        ctx.send(
            poise::CreateReply::default()
                .embed(embed::err("You cannot reject yourself from your own room."))
                .ephemeral(true),
        )
        .await?;
        return Ok(());
    }

    crate::compat::set_permission(
            ctx.http(), channel,
            PermissionOverwrite {
                allow: Permissions::empty(),
                deny: Permissions::CONNECT,
                kind: PermissionOverwriteType::Member(user.id),
            },
            Some("Rejected from a temporary room"),
        )
        .await?;

    // Disconnect them if they are in there now.
    let in_room = {
        match guild_id.to_guild_cached(&ctx.serenity_context().cache) {
            Some(guild) => guild
                .voice_states
                .get(&user.id)
                .and_then(|s| s.channel_id)
                .map(|c| c == channel)
                .unwrap_or(false),
            None => false,
        }
    };
    if in_room {
        let _ = guild_id
            .edit_member(
                ctx.http(),
                user.id,
                EditMember::new().disconnect_member(),
            )
            .await;
    }

    ctx.send(
        poise::CreateReply::default()
            .embed(embed::ok(format!(
                "<@{}> can no longer join your room.",
                user.id.get()
            )))
            .ephemeral(true),
    )
    .await?;
    Ok(())
}

/// Hand your room to someone else in it.
#[poise::command(slash_command, rename = "transfer", guild_only)]
pub async fn voice_transfer(
    ctx: Ctx<'_>,
    #[description = "New owner"] user: User,
) -> Result<(), Error> {
    let Some(channel) = require_room(&ctx).await? else {
        return Ok(());
    };
    let guild_id = ctx.guild_id().unwrap();

    let in_room = {
        match guild_id.to_guild_cached(&ctx.serenity_context().cache) {
            Some(guild) => guild
                .voice_states
                .get(&user.id)
                .and_then(|s| s.channel_id)
                .map(|c| c == channel)
                .unwrap_or(false),
            None => true,
        }
    };
    if !in_room {
        ctx.send(
            poise::CreateReply::default()
                .embed(embed::err("They need to be in the room to take it over."))
                .ephemeral(true),
        )
        .await?;
        return Ok(());
    }

    sqlx::query("UPDATE temp_voice SET owner_id = ? WHERE channel_id = ?")
        .bind(user.id.get() as i64)
        .bind(channel.get() as i64)
        .execute(&ctx.data().db)
        .await?;

    let _ = crate::compat::set_permission(
            ctx.http(), channel,
            PermissionOverwrite {
                allow: Permissions::MANAGE_CHANNELS | Permissions::MOVE_MEMBERS,
                deny: Permissions::empty(),
                kind: PermissionOverwriteType::Member(user.id),
            },
            Some("Room ownership transferred"),
        )
        .await;

    ctx.send(poise::CreateReply::default().embed(embed::ok(format!(
        "<@{}> now owns the room.",
        user.id.get()
    ))))
    .await?;
    Ok(())
}

/// Take over a room whose owner has left.
#[poise::command(slash_command, rename = "claim", guild_only)]
pub async fn voice_claim(ctx: Ctx<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap();
    let guild_i64 = guild_id.get() as i64;

    // Which temp room is the caller sitting in?
    let current = {
        match guild_id.to_guild_cached(&ctx.serenity_context().cache) {
            Some(guild) => guild
                .voice_states
                .get(&ctx.author().id)
                .and_then(|s| s.channel_id),
            None => None,
        }
    };

    let Some(channel) = current else {
        ctx.send(
            poise::CreateReply::default()
                .embed(embed::err("Join the room you want to claim first."))
                .ephemeral(true),
        )
        .await?;
        return Ok(());
    };

    let owner: Option<i64> =
        sqlx::query_scalar("SELECT owner_id FROM temp_voice WHERE channel_id = ?")
            .bind(channel.get() as i64)
            .fetch_optional(&ctx.data().db)
            .await?;

    let Some(owner) = owner else {
        ctx.send(
            poise::CreateReply::default()
                .embed(embed::err("This is not a temporary room."))
                .ephemeral(true),
        )
        .await?;
        return Ok(());
    };

    // The owner has to actually be gone.
    let owner_present = {
        match guild_id.to_guild_cached(&ctx.serenity_context().cache) {
            Some(guild) => guild
                .voice_states
                .get(&UserId::new(owner as u64))
                .and_then(|s| s.channel_id)
                .map(|c| c == channel)
                .unwrap_or(false),
            None => false,
        }
    };
    if owner_present {
        ctx.send(
            poise::CreateReply::default()
                .embed(embed::err("The owner is still here."))
                .ephemeral(true),
        )
        .await?;
        return Ok(());
    }

    sqlx::query("UPDATE temp_voice SET owner_id = ? WHERE channel_id = ?")
        .bind(ctx.author().id.get() as i64)
        .bind(channel.get() as i64)
        .execute(&ctx.data().db)
        .await?;

    let _ = crate::compat::set_permission(
            ctx.http(), channel,
            PermissionOverwrite {
                allow: Permissions::MANAGE_CHANNELS | Permissions::MOVE_MEMBERS,
                deny: Permissions::empty(),
                kind: PermissionOverwriteType::Member(ctx.author().id),
            },
            Some("Room claimed"),
        )
        .await;

    let _ = guild_i64;
    ctx.send(poise::CreateReply::default().embed(embed::ok("You now own this room.")))
        .await?;
    Ok(())
}

/// Details about your room.
#[poise::command(slash_command, rename = "info", guild_only)]
pub async fn voice_info(ctx: Ctx<'_>) -> Result<(), Error> {
    let Some((channel_id, owner_id)) = my_room(&ctx).await else {
        ctx.send(
            poise::CreateReply::default()
                .embed(embed::err("You do not own a temporary room."))
                .ephemeral(true),
        )
        .await?;
        return Ok(());
    };

    let row: Option<(String, i64, i64, i64, String)> = sqlx::query_as(
        "SELECT name, user_limit, locked, private, created_at FROM temp_voice WHERE channel_id = ?",
    )
    .bind(channel_id)
    .fetch_optional(&ctx.data().db)
    .await?;

    let Some((name, limit, locked, private, created_at)) = row else {
        return Ok(());
    };

    let people = occupancy(
        ctx.serenity_context(),
        ctx.guild_id().unwrap(),
        ChannelId::new(channel_id as u64),
    );

    ctx.send(
        poise::CreateReply::default().embed(
            embed::titled("🔊 Your room", embed::INFO)
                .field("Channel", format!("<#{channel_id}>"), true)
                .field("Owner", format!("<@{owner_id}>"), true)
                .field("Name", name, true)
                .field(
                    "Limit",
                    if limit == 0 {
                        "unlimited".to_string()
                    } else {
                        limit.to_string()
                    },
                    true,
                )
                .field("In the room", people.to_string(), true)
                .field(
                    "Access",
                    match (locked, private) {
                        (_, 1) => "hidden",
                        (1, _) => "locked",
                        _ => "open",
                    },
                    true,
                )
                .footer(embed::footer(format!("Created {created_at} UTC"))),
        ),
    )
    .await?;
    Ok(())
}

/// Voice activity statistics for this server.
#[poise::command(slash_command, rename = "stats", guild_only)]
pub async fn voice_stats(
    ctx: Ctx<'_>,
    #[description = "Stats for one member instead of the server"] user: Option<User>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let dbh = &ctx.data().db;

    if let Some(user) = user {
        let user_i64 = user.id.get() as i64;
        let total: i64 = sqlx::query_scalar(
            "SELECT COALESCE(voice_secs, 0) FROM levels WHERE guild_id = ? AND user_id = ?",
        )
        .bind(guild_id)
        .bind(user_i64)
        .fetch_optional(dbh)
        .await?
        .unwrap_or(0);

        let sessions: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM voice_sessions WHERE guild_id = ? AND user_id = ?",
        )
        .bind(guild_id)
        .bind(user_i64)
        .fetch_one(dbh)
        .await?;

        let longest: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(duration), 0) FROM voice_sessions
             WHERE guild_id = ? AND user_id = ?",
        )
        .bind(guild_id)
        .bind(user_i64)
        .fetch_one(dbh)
        .await?;

        ctx.send(
            poise::CreateReply::default().embed(
                embed::titled(format!("Voice · {}", user.tag()), embed::INFO)
                    .field("Total time", time::humanize(total), true)
                    .field("Sessions", sessions.to_string(), true)
                    .field("Longest session", time::humanize(longest), true)
                    .thumbnail(user.face()),
            ),
        )
        .await?;
        return Ok(());
    }

    let total: i64 =
        sqlx::query_scalar("SELECT COALESCE(SUM(duration), 0) FROM voice_sessions WHERE guild_id = ?")
            .bind(guild_id)
            .fetch_one(dbh)
            .await?;

    let top: Vec<(i64, i64)> = sqlx::query_as(
        "SELECT user_id, voice_secs FROM levels
         WHERE guild_id = ? AND voice_secs > 0 ORDER BY voice_secs DESC LIMIT 10",
    )
    .bind(guild_id)
    .fetch_all(dbh)
    .await?;

    let busiest: Vec<(i64, i64)> = sqlx::query_as(
        "SELECT channel_id, SUM(duration) FROM voice_sessions
         WHERE guild_id = ? GROUP BY channel_id ORDER BY SUM(duration) DESC LIMIT 5",
    )
    .bind(guild_id)
    .fetch_all(dbh)
    .await?;

    let leaderboard = if top.is_empty() {
        "-".to_string()
    } else {
        top.iter()
            .enumerate()
            .map(|(i, (user_id, secs))| {
                format!("**{}.** <@{user_id}> — {}", i + 1, time::humanize(*secs))
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    let channels = if busiest.is_empty() {
        "-".to_string()
    } else {
        busiest
            .iter()
            .map(|(channel_id, secs)| format!("<#{channel_id}> — {}", time::humanize(*secs)))
            .collect::<Vec<_>>()
            .join("\n")
    };

    ctx.send(
        poise::CreateReply::default().embed(
            embed::titled("🔊 Voice statistics", embed::INFO)
                .field("Total tracked time", time::humanize(total), false)
                .field("Most active members", leaderboard, false)
                .field("Busiest channels", channels, false)
                .footer(embed::footer(
                    "Tracking starts when the bot comes online, so early history is not included.",
                )),
        ),
    )
    .await?;
    Ok(())
}

/// Clean up temp rooms that outlived a restart.
pub async fn cleanup_orphans(sctx: &serenity::Context, db: &Db) {
    let rows: Vec<(i64, i64)> = sqlx::query_as("SELECT channel_id, guild_id FROM temp_voice")
        .fetch_all(db)
        .await
        .unwrap_or_default();

    for (channel_i64, guild_i64) in rows {
        let channel = ChannelId::new(channel_i64 as u64);
        let guild_id = GuildId::new(guild_i64 as u64);
        if occupancy(sctx, guild_id, channel) == 0 {
            let _ = crate::compat::delete_channel(
                &sctx.http,
                channel,
                "Orphaned temporary voice room",
            )
            .await;
            let _ = sqlx::query("DELETE FROM temp_voice WHERE channel_id = ?")
                .bind(channel_i64)
                .execute(db)
                .await;
        }
    }
}

pub fn commands() -> Vec<poise::Command<Data, Error>> {
    vec![voice()]
}
