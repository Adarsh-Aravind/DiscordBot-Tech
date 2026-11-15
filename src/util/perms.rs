//! Permission, role-hierarchy and staff checks.

use std::collections::HashMap;

use poise::serenity_prelude as serenity;
use serenity::{GuildId, Member, Permissions, RoleId, UserId};

use crate::db;
use crate::state::{Ctx, Error};

/// Copy role positions out of the cache so no cache guard is held across an await.
pub fn role_positions(ctx: &serenity::Context, guild_id: GuildId) -> HashMap<RoleId, i64> {
    let mut map = HashMap::new();
    if let Some(guild) = guild_id.to_guild_cached(&ctx.cache) {
        for (id, role) in guild.roles.iter() {
            map.insert(*id, role.position as i64);
        }
    }
    map
}

pub fn guild_owner(ctx: &serenity::Context, guild_id: GuildId) -> Option<UserId> {
    guild_id.to_guild_cached(&ctx.cache).map(|g| g.owner_id)
}

pub fn bot_id(ctx: &serenity::Context) -> UserId {
    ctx.cache.current_user().id
}

/// Guild-level permissions implied by a set of roles, read from the cache.
///
/// Channel overwrites are deliberately ignored: this answers "does this person
/// hold a moderator permission in this server", where the base roles are what
/// matters. An empty set is returned when the guild is not cached, so callers
/// decide for themselves whether that should fail open or closed.
pub fn permissions_from_roles(
    ctx: &serenity::Context,
    guild_id: GuildId,
    roles: &[RoleId],
) -> Permissions {
    let Some(guild) = guild_id.to_guild_cached(&ctx.cache) else {
        return Permissions::empty();
    };
    let mut perms = Permissions::empty();
    if let Some(everyone) = guild.roles.get(&RoleId::new(guild_id.get())) {
        perms |= everyone.permissions;
    }
    for role_id in roles {
        if let Some(role) = guild.roles.get(role_id) {
            perms |= role.permissions;
        }
    }
    perms
}

/// True when these roles carry one of the permissions we treat as staff.
///
/// Automod uses this so a moderator who was never given a role from
/// `/config role` is still exempt from the automatic punishments. It is
/// deliberately the same permission set as [`is_staff`].
pub fn roles_are_staff(ctx: &serenity::Context, guild_id: GuildId, roles: &[RoleId]) -> bool {
    let perms = permissions_from_roles(ctx, guild_id, roles);
    perms.administrator()
        || perms.manage_guild()
        || perms.manage_messages()
        || perms.kick_members()
        || perms.ban_members()
        || perms.moderate_members()
}

/// Highest role position held by a member. Returns -1 when nothing is cached.
pub fn top_position(positions: &HashMap<RoleId, i64>, member: &Member) -> i64 {
    member
        .roles
        .iter()
        .filter_map(|r| positions.get(r))
        .copied()
        .max()
        .unwrap_or(-1)
}

/// Whether `actor` is allowed to moderate `target`, and whether the bot can carry it out.
/// Returns a human readable reason on refusal.
pub async fn can_moderate(
    ctx: &serenity::Context,
    guild_id: GuildId,
    actor: UserId,
    target: UserId,
) -> Result<(), String> {
    if actor == target {
        return Err("You cannot use this on yourself.".into());
    }
    let me = bot_id(ctx);
    if target == me {
        return Err("I am not going to do that to myself.".into());
    }
    if let Some(owner) = guild_owner(ctx, guild_id) {
        if target == owner {
            return Err("That member owns this server.".into());
        }
        if actor == owner {
            // Owner outranks everyone; only the bot's own position can still block us.
            return bot_can_act(ctx, guild_id, target).await;
        }
    }

    let actor_member = guild_id
        .member(ctx, actor)
        .await
        .map_err(|_| "I could not look you up in this server.".to_string())?;
    let target_member = match guild_id.member(ctx, target).await {
        Ok(m) => m,
        // Not a member (e.g. banning someone who already left) - nothing to outrank.
        Err(_) => return Ok(()),
    };

    let positions = role_positions(ctx, guild_id);
    let actor_top = top_position(&positions, &actor_member);
    let target_top = top_position(&positions, &target_member);

    if target_top >= actor_top {
        return Err("That member has a role equal to or above yours.".into());
    }
    bot_can_act(ctx, guild_id, target).await
}

/// Can the bot itself act on this member (role position + permissions)?
pub async fn bot_can_act(
    ctx: &serenity::Context,
    guild_id: GuildId,
    target: UserId,
) -> Result<(), String> {
    let me = bot_id(ctx);
    let bot_member = match guild_id.member(ctx, me).await {
        Ok(m) => m,
        Err(_) => return Ok(()),
    };
    let target_member = match guild_id.member(ctx, target).await {
        Ok(m) => m,
        Err(_) => return Ok(()),
    };
    let positions = role_positions(ctx, guild_id);
    if top_position(&positions, &target_member) >= top_position(&positions, &bot_member) {
        return Err(
            "That member is above me in the role list, so I cannot action them. Move my role higher."
                .into(),
        );
    }
    Ok(())
}

/// True when the member holds one of the configured mod/admin/staff roles, is the
/// guild owner, a bot owner, or has an equivalent Discord permission.
pub async fn is_staff(ctx: &Ctx<'_>) -> bool {
    let data = ctx.data();
    if data.is_owner(ctx.author().id.get()) {
        return true;
    }
    let Some(guild_id) = ctx.guild_id() else {
        return false;
    };
    if guild_owner(ctx.serenity_context(), guild_id) == Some(ctx.author().id) {
        return true;
    }

    let Ok(cfg) = db::guild_config(&data.db, guild_id.get() as i64).await else {
        return false;
    };
    let allowed: Vec<i64> = db::csv_ids(&cfg.mod_roles)
        .into_iter()
        .chain(db::csv_ids(&cfg.admin_roles))
        .chain(db::csv_ids(&cfg.staff_roles))
        .collect();

    if let Some(member) = ctx.author_member().await {
        if member
            .roles
            .iter()
            .any(|r| allowed.contains(&(r.get() as i64)))
        {
            return true;
        }
        if let Ok(perms) = member.permissions(ctx.serenity_context()) {
            // Keep this list in step with `roles_are_staff`, which automod uses.
            if perms.administrator()
                || perms.manage_guild()
                || perms.manage_messages()
                || perms.kick_members()
                || perms.ban_members()
                || perms.moderate_members()
            {
                return true;
            }
        }
    }
    false
}

/// poise check: refuse the command unless the caller is staff.
pub async fn staff_only(ctx: Ctx<'_>) -> Result<bool, Error> {
    if is_staff(&ctx).await {
        return Ok(true);
    }
    ctx.send(
        poise::CreateReply::default()
            .embed(crate::util::embed::err(
                "This command is for staff only. It needs a moderator permission \
                 (Manage Messages, Kick, Ban or Timeout), or a role set with \
                 `/config staffroles`.",
            ))
            .ephemeral(true),
    )
    .await?;
    Ok(false)
}

/// True when the member may change the bot's configuration.
pub async fn is_admin(ctx: &Ctx<'_>) -> bool {
    let data = ctx.data();
    if data.is_owner(ctx.author().id.get()) {
        return true;
    }
    let Some(guild_id) = ctx.guild_id() else {
        return false;
    };
    if guild_owner(ctx.serenity_context(), guild_id) == Some(ctx.author().id) {
        return true;
    }

    if let Ok(cfg) = db::guild_config(&data.db, guild_id.get() as i64).await {
        let admins = db::csv_ids(&cfg.admin_roles);
        if let Some(member) = ctx.author_member().await {
            if member
                .roles
                .iter()
                .any(|r| admins.contains(&(r.get() as i64)))
            {
                return true;
            }
            if let Ok(resolved) = member.permissions(ctx.serenity_context()) {
                if resolved.administrator() || resolved.manage_guild() {
                    return true;
                }
            }
        }
    }
    false
}

/// poise check: refuse unless the caller is an admin (Manage Guild or configured admin role).
pub async fn admin_only(ctx: Ctx<'_>) -> Result<bool, Error> {
    if is_admin(&ctx).await {
        return Ok(true);
    }
    ctx.send(
        poise::CreateReply::default()
            .embed(crate::util::embed::err(
                "This command needs the **Manage Server** permission or a configured admin role.",
            ))
            .ephemeral(true),
    )
    .await?;
    Ok(false)
}

