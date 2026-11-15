//! Commands for configuring automod: toggles, thresholds, word lists,
//! exemptions, custom rules and manual raid mode.

use poise::serenity_prelude as serenity;
use serenity::{GuildChannel, Role, User};

use crate::db;
use crate::state::{Ctx, Data, Error};
use crate::util::{embed, perms, text, time};

#[derive(Debug, poise::ChoiceParameter)]
pub enum Feature {
    #[name = "anti-spam (per member rate)"]
    Antispam,
    #[name = "anti-flood (per channel rate)"]
    Antiflood,
    #[name = "anti-raid (join bursts)"]
    Antiraid,
    #[name = "anti-nuke (destructive admins)"]
    Antinuke,
    #[name = "anti-phishing links"]
    Antiphish,
    #[name = "anti-invite links"]
    Antiinvite,
    #[name = "anti-mention spam"]
    Antimention,
    #[name = "bad word filter"]
    Badwords,
    #[name = "duplicate message detection"]
    Duplicates,
    #[name = "excessive caps"]
    Caps,
    #[name = "suspicious new accounts"]
    Suspicious,
    #[name = "delete offending messages"]
    DeleteViolations,
}

impl Feature {
    fn column(&self) -> &'static str {
        match self {
            Feature::Antispam => "antispam",
            Feature::Antiflood => "antiflood",
            Feature::Antiraid => "antiraid",
            Feature::Antinuke => "antinuke",
            Feature::Antiphish => "antiphish",
            Feature::Antiinvite => "antiinvite",
            Feature::Antimention => "antimention",
            Feature::Badwords => "badwords_enabled",
            Feature::Duplicates => "duplicate_enabled",
            Feature::Caps => "caps_enabled",
            Feature::Suspicious => "suspicious_accounts",
            Feature::DeleteViolations => "delete_violations",
        }
    }
}

#[derive(Debug, poise::ChoiceParameter)]
pub enum Threshold {
    #[name = "spam: messages"]
    SpamMsgs,
    #[name = "spam: window seconds"]
    SpamWindow,
    #[name = "flood: messages"]
    FloodMsgs,
    #[name = "flood: window seconds"]
    FloodWindow,
    #[name = "raid: joins"]
    RaidJoins,
    #[name = "raid: window seconds"]
    RaidWindow,
    #[name = "nuke: actions"]
    NukeThreshold,
    #[name = "nuke: window seconds"]
    NukeWindow,
    #[name = "mentions per message"]
    MentionLimit,
    #[name = "duplicate repeats"]
    DuplicateLimit,
    #[name = "caps percentage"]
    CapsPercent,
    #[name = "minimum account age (seconds)"]
    SuspiciousMinAge,
}

impl Threshold {
    fn column(&self) -> &'static str {
        match self {
            Threshold::SpamMsgs => "antispam_msgs",
            Threshold::SpamWindow => "antispam_window",
            Threshold::FloodMsgs => "antiflood_msgs",
            Threshold::FloodWindow => "antiflood_window",
            Threshold::RaidJoins => "antiraid_joins",
            Threshold::RaidWindow => "antiraid_window",
            Threshold::NukeThreshold => "antinuke_threshold",
            Threshold::NukeWindow => "antinuke_window",
            Threshold::MentionLimit => "mention_limit",
            Threshold::DuplicateLimit => "duplicate_limit",
            Threshold::CapsPercent => "caps_percent",
            Threshold::SuspiciousMinAge => "suspicious_min_age",
        }
    }
}

#[derive(Debug, poise::ChoiceParameter)]
pub enum RaidResponse {
    #[name = "lock every channel"]
    Lockdown,
    #[name = "kick joiners"]
    Kick,
    #[name = "ban joiners"]
    Ban,
    #[name = "alert staff only"]
    Alert,
}

/// Configure the automatic moderation engine.
#[poise::command(
    slash_command,
    guild_only,
    category = "Automod",
    subcommands(
        "automod_status",
        "automod_toggle",
        "automod_set",
        "automod_alerts",
        "automod_raidaction",
        "automod_exempt",
        "automod_reset"
    ),
    check = "perms::admin_only"
)]
pub async fn automod(_ctx: Ctx<'_>) -> Result<(), Error> {
    Ok(())
}

fn flag(value: i64) -> &'static str {
    if value == 1 {
        "🟢 on"
    } else {
        "🔴 off"
    }
}

/// Show every automod setting at a glance.
#[poise::command(slash_command, rename = "status", guild_only)]
pub async fn automod_status(ctx: Ctx<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let cfg = db::automod_config(&ctx.data().db, guild_id).await?;

    let badwords: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM badwords WHERE guild_id = ?")
        .bind(guild_id)
        .fetch_one(&ctx.data().db)
        .await?;
    let rules: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM automod_rules WHERE guild_id = ?")
        .bind(guild_id)
        .fetch_one(&ctx.data().db)
        .await?;
    let exempt: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM automod_exempt WHERE guild_id = ?")
            .bind(guild_id)
            .fetch_one(&ctx.data().db)
            .await?;

    let embed = embed::titled("🛡️ Automod configuration", embed::INFO)
        .field(
            "Rate limits",
            format!(
                "Anti-spam {} · {} msgs / {}s\nAnti-flood {} · {} msgs / {}s",
                flag(cfg.antispam),
                cfg.antispam_msgs,
                cfg.antispam_window,
                flag(cfg.antiflood),
                cfg.antiflood_msgs,
                cfg.antiflood_window
            ),
            false,
        )
        .field(
            "Server protection",
            format!(
                "Anti-raid {} · {} joins / {}s → **{}**\nAnti-nuke {} · {} actions / {}s",
                flag(cfg.antiraid),
                cfg.antiraid_joins,
                cfg.antiraid_window,
                cfg.antiraid_action,
                flag(cfg.antinuke),
                cfg.antinuke_threshold,
                cfg.antinuke_window
            ),
            false,
        )
        .field(
            "Content filters",
            format!(
                "Phishing {}\nInvites {}\nMentions {} (limit {})\nBad words {} ({badwords} words)\nDuplicates {} (limit {})\nCaps {} ({}%)",
                flag(cfg.antiphish),
                flag(cfg.antiinvite),
                flag(cfg.antimention),
                cfg.mention_limit,
                flag(cfg.badwords_enabled),
                flag(cfg.duplicate_enabled),
                cfg.duplicate_limit,
                flag(cfg.caps_enabled),
                cfg.caps_percent
            ),
            false,
        )
        .field(
            "Joins",
            format!(
                "Suspicious accounts {} · minimum age **{}** → **{}**",
                flag(cfg.suspicious_accounts),
                time::humanize(cfg.suspicious_min_age),
                cfg.suspicious_action
            ),
            false,
        )
        .field(
            "Other",
            format!(
                "Delete violations {}\nAlert channel: {}\nCustom rules: **{rules}** · exemptions: **{exempt}**",
                flag(cfg.delete_violations),
                cfg.alert_channel
                    .map(|c| format!("<#{c}>"))
                    .unwrap_or_else(|| "mod log".to_string())
            ),
            false,
        );

    ctx.send(poise::CreateReply::default().embed(embed)).await?;
    Ok(())
}

/// Switch an automod feature on or off.
#[poise::command(slash_command, rename = "toggle", guild_only)]
pub async fn automod_toggle(
    ctx: Ctx<'_>,
    #[description = "Which feature"] feature: Feature,
    #[description = "Turn it on?"] enabled: bool,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    db::set_bool(
        &ctx.data().db,
        "automod_config",
        feature.column(),
        guild_id,
        enabled,
    )
    .await?;
    super::invalidate(guild_id);

    ctx.send(poise::CreateReply::default().embed(embed::ok(format!(
        "**{}** is now {}.",
        feature.column(),
        if enabled { "on" } else { "off" }
    ))))
    .await?;
    Ok(())
}

/// Change a threshold.
#[poise::command(slash_command, rename = "set", guild_only)]
pub async fn automod_set(
    ctx: Ctx<'_>,
    #[description = "Which threshold"] setting: Threshold,
    #[description = "New value"]
    #[min = 1]
    value: i64,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    db::set_i64(
        &ctx.data().db,
        "automod_config",
        setting.column(),
        guild_id,
        value,
    )
    .await?;
    super::invalidate(guild_id);

    ctx.send(poise::CreateReply::default().embed(embed::ok(format!(
        "`{}` set to **{value}**.",
        setting.column()
    ))))
    .await?;
    Ok(())
}

/// Where automod alerts go. Leave the channel empty to fall back to the mod log.
#[poise::command(slash_command, rename = "alerts", guild_only)]
pub async fn automod_alerts(
    ctx: Ctx<'_>,
    #[description = "Alert channel"] channel: Option<GuildChannel>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let value = channel.as_ref().map(|c| c.id.get() as i64);
    db::set_opt_i64(
        &ctx.data().db,
        "automod_config",
        "alert_channel",
        guild_id,
        value,
    )
    .await?;
    super::invalidate(guild_id);

    let body = match channel {
        Some(c) => format!("Automod alerts will go to <#{}>.", c.id.get()),
        None => "Automod alerts will go to the mod log.".to_string(),
    };
    ctx.send(poise::CreateReply::default().embed(embed::ok(body)))
        .await?;
    Ok(())
}

/// What should happen when a raid is detected.
#[poise::command(slash_command, rename = "raidaction", guild_only)]
pub async fn automod_raidaction(
    ctx: Ctx<'_>,
    #[description = "Response"] response: RaidResponse,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let value = match response {
        RaidResponse::Lockdown => "lockdown",
        RaidResponse::Kick => "kick",
        RaidResponse::Ban => "ban",
        RaidResponse::Alert => "alert",
    };
    db::set_text(
        &ctx.data().db,
        "automod_config",
        "antiraid_action",
        guild_id,
        value.to_string(),
    )
    .await?;
    super::invalidate(guild_id);

    ctx.send(poise::CreateReply::default().embed(embed::ok(format!(
        "On the next raid I will **{value}**."
    ))))
    .await?;
    Ok(())
}

/// Reset automod to the shipped defaults.
#[poise::command(slash_command, rename = "reset", guild_only)]
pub async fn automod_reset(ctx: Ctx<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    sqlx::query("DELETE FROM automod_config WHERE guild_id = ?")
        .bind(guild_id)
        .execute(&ctx.data().db)
        .await?;
    db::ensure_guild(&ctx.data().db, guild_id).await?;
    super::invalidate(guild_id);
    ctx.send(
        poise::CreateReply::default()
            .embed(embed::ok("Automod is back to its default settings.")),
    )
    .await?;
    Ok(())
}

// ===========================================================================
// Exemptions
// ===========================================================================

/// Roles, channels or members that automod ignores.
#[poise::command(
    slash_command,
    rename = "exempt",
    guild_only,
    subcommands("exempt_add", "exempt_remove", "exempt_list")
)]
pub async fn automod_exempt(_ctx: Ctx<'_>) -> Result<(), Error> {
    Ok(())
}

/// Exempt a role, channel or member from automod.
#[poise::command(slash_command, rename = "add", guild_only)]
pub async fn exempt_add(
    ctx: Ctx<'_>,
    #[description = "Role to exempt"] role: Option<Role>,
    #[description = "Channel to exempt"] channel: Option<GuildChannel>,
    #[description = "Member to exempt"] user: Option<User>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let mut added = Vec::new();

    for (kind, id, label) in [
        ("role", role.as_ref().map(|r| r.id.get()), "role"),
        ("channel", channel.as_ref().map(|c| c.id.get()), "channel"),
        ("user", user.as_ref().map(|u| u.id.get()), "member"),
    ] {
        if let Some(id) = id {
            sqlx::query(
                "INSERT OR IGNORE INTO automod_exempt (guild_id, kind, target) VALUES (?, ?, ?)",
            )
            .bind(guild_id)
            .bind(kind)
            .bind(id as i64)
            .execute(&ctx.data().db)
            .await?;
            super::invalidate(guild_id);
            added.push(match kind {
                "role" => format!("{label} <@&{id}>"),
                "channel" => format!("{label} <#{id}>"),
                _ => format!("{label} <@{id}>"),
            });
        }
    }

    let body = if added.is_empty() {
        embed::err("Give me at least one role, channel or member to exempt.")
    } else {
        embed::ok(format!("Automod will now ignore {}.", added.join(", ")))
    };
    ctx.send(poise::CreateReply::default().embed(body)).await?;
    Ok(())
}

/// Remove an automod exemption.
#[poise::command(slash_command, rename = "remove", guild_only)]
pub async fn exempt_remove(
    ctx: Ctx<'_>,
    #[description = "Role"] role: Option<Role>,
    #[description = "Channel"] channel: Option<GuildChannel>,
    #[description = "Member"] user: Option<User>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let mut removed = 0u64;

    for (kind, id) in [
        ("role", role.as_ref().map(|r| r.id.get())),
        ("channel", channel.as_ref().map(|c| c.id.get())),
        ("user", user.as_ref().map(|u| u.id.get())),
    ] {
        if let Some(id) = id {
            let result = sqlx::query(
                "DELETE FROM automod_exempt WHERE guild_id = ? AND kind = ? AND target = ?",
            )
            .bind(guild_id)
            .bind(kind)
            .bind(id as i64)
            .execute(&ctx.data().db)
            .await?;
            super::invalidate(guild_id);
            removed += result.rows_affected();
        }
    }

    ctx.send(poise::CreateReply::default().embed(embed::ok(format!(
        "Removed **{removed}** exemption(s)."
    ))))
    .await?;
    Ok(())
}

/// Show everything automod ignores.
#[poise::command(slash_command, rename = "list", guild_only)]
pub async fn exempt_list(ctx: Ctx<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let rows: Vec<(String, i64)> =
        sqlx::query_as("SELECT kind, target FROM automod_exempt WHERE guild_id = ? ORDER BY kind")
            .bind(guild_id)
            .fetch_all(&ctx.data().db)
            .await?;

    let body = if rows.is_empty() {
        "Nothing is exempt. Staff roles are always exempt automatically.".to_string()
    } else {
        rows.iter()
            .map(|(kind, id)| match kind.as_str() {
                "role" => format!("Role <@&{id}>"),
                "channel" => format!("Channel <#{id}>"),
                _ => format!("Member <@{id}>"),
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    ctx.send(
        poise::CreateReply::default()
            .embed(embed::titled("Automod exemptions", embed::INFO).description(body)),
    )
    .await?;
    Ok(())
}

// ===========================================================================
// Bad words
// ===========================================================================

/// Manage the filtered word list.
#[poise::command(
    slash_command,
    guild_only,
    category = "Automod",
    subcommands("badword_add", "badword_remove", "badword_list"),
    check = "perms::admin_only"
)]
pub async fn badword(_ctx: Ctx<'_>) -> Result<(), Error> {
    Ok(())
}

/// Add words, comma separated. Matching ignores case and common letter swaps.
#[poise::command(slash_command, rename = "add", guild_only, ephemeral)]
pub async fn badword_add(
    ctx: Ctx<'_>,
    #[description = "Word or comma separated list"] words: String,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let mut added = 0;
    for word in words.split(',') {
        let word = text::normalize_leet(word.trim());
        if word.is_empty() {
            continue;
        }
        let result = sqlx::query("INSERT OR IGNORE INTO badwords (guild_id, word) VALUES (?, ?)")
            .bind(guild_id)
            .bind(&word)
            .execute(&ctx.data().db)
            .await?;
        added += result.rows_affected();
    }
    super::invalidate(guild_id);
    ctx.send(
        poise::CreateReply::default()
            .embed(embed::ok(format!("Added **{added}** word(s) to the filter.")))
            .ephemeral(true),
    )
    .await?;
    Ok(())
}

/// Take a word off the filter.
#[poise::command(slash_command, rename = "remove", guild_only, ephemeral)]
pub async fn badword_remove(
    ctx: Ctx<'_>,
    #[description = "Word to remove"] word: String,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let normalised = text::normalize_leet(word.trim());
    let result = sqlx::query("DELETE FROM badwords WHERE guild_id = ? AND word = ?")
        .bind(guild_id)
        .bind(&normalised)
        .execute(&ctx.data().db)
        .await?;
    super::invalidate(guild_id);

    let body = if result.rows_affected() == 0 {
        embed::err("That word is not in the filter.")
    } else {
        embed::ok("Removed.")
    };
    ctx.send(poise::CreateReply::default().embed(body).ephemeral(true))
        .await?;
    Ok(())
}

/// Show the filtered words.
#[poise::command(slash_command, rename = "list", guild_only, ephemeral)]
pub async fn badword_list(ctx: Ctx<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let words: Vec<String> =
        sqlx::query_scalar("SELECT word FROM badwords WHERE guild_id = ? ORDER BY word")
            .bind(guild_id)
            .fetch_all(&ctx.data().db)
            .await?;

    let body = if words.is_empty() {
        "The filter is empty.".to_string()
    } else {
        format!(
            "**{}** word(s):\n||{}||",
            words.len(),
            text::truncate(&words.join(", "), 3000)
        )
    };
    ctx.send(
        poise::CreateReply::default()
            .embed(embed::titled("Filtered words", embed::MUTED).description(body))
            .ephemeral(true),
    )
    .await?;
    Ok(())
}

// ===========================================================================
// Link allowlist
// ===========================================================================

/// Domains that may be linked. While this list has entries, every other link
/// is removed.
#[poise::command(
    slash_command,
    guild_only,
    category = "Automod",
    subcommands("allowlist_add", "allowlist_remove", "allowlist_list"),
    check = "perms::admin_only"
)]
pub async fn allowlist(_ctx: Ctx<'_>) -> Result<(), Error> {
    Ok(())
}

/// Allow links to a domain. While the list has any entry, links elsewhere are deleted.
#[poise::command(slash_command, rename = "add", guild_only)]
pub async fn allowlist_add(
    ctx: Ctx<'_>,
    #[description = "Domain, e.g. youtube.com"] domain: String,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let clean = domain
        .trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_start_matches("www.")
        .split('/')
        .next()
        .unwrap_or("")
        .to_lowercase();

    if clean.is_empty() || !clean.contains('.') {
        ctx.send(poise::CreateReply::default().embed(embed::err("That is not a domain.")))
            .await?;
        return Ok(());
    }

    sqlx::query("INSERT OR IGNORE INTO link_allowlist (guild_id, domain) VALUES (?, ?)")
        .bind(guild_id)
        .bind(&clean)
        .execute(&ctx.data().db)
        .await?;
    super::invalidate(guild_id);

    ctx.send(poise::CreateReply::default().embed(embed::ok(format!(
        "`{clean}` is allowed. **Note:** while the allowlist has any entry, links to other domains are deleted."
    ))))
    .await?;
    Ok(())
}

/// Take a domain off the link allowlist.
#[poise::command(slash_command, rename = "remove", guild_only)]
pub async fn allowlist_remove(
    ctx: Ctx<'_>,
    #[description = "Domain to remove"] domain: String,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let result = sqlx::query("DELETE FROM link_allowlist WHERE guild_id = ? AND domain = ?")
        .bind(guild_id)
        .bind(domain.trim().to_lowercase())
        .execute(&ctx.data().db)
        .await?;
    super::invalidate(guild_id);
    let body = if result.rows_affected() == 0 {
        embed::err("Not on the list.")
    } else {
        embed::ok("Removed.")
    };
    ctx.send(poise::CreateReply::default().embed(body)).await?;
    Ok(())
}

/// Show the link allowlist.
#[poise::command(slash_command, rename = "list", guild_only)]
pub async fn allowlist_list(ctx: Ctx<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let domains: Vec<String> =
        sqlx::query_scalar("SELECT domain FROM link_allowlist WHERE guild_id = ? ORDER BY domain")
            .bind(guild_id)
            .fetch_all(&ctx.data().db)
            .await?;

    let body = if domains.is_empty() {
        "Empty, so all links are allowed (phishing detection still applies).".to_string()
    } else {
        domains
            .iter()
            .map(|d| format!("• `{d}`"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    ctx.send(
        poise::CreateReply::default()
            .embed(embed::titled("Link allowlist", embed::INFO).description(body)),
    )
    .await?;
    Ok(())
}

// ===========================================================================
// Custom rules
// ===========================================================================

#[derive(Debug, poise::ChoiceParameter)]
pub enum TriggerKind {
    #[name = "contains text"]
    Contains,
    #[name = "starts with"]
    StartsWith,
    #[name = "ends with"]
    EndsWith,
    #[name = "word list (comma separated)"]
    WordList,
    #[name = "regular expression"]
    Regex,
    #[name = "has any attachment"]
    Attachment,
    #[name = "has any sticker"]
    Sticker,
}

impl TriggerKind {
    fn key(&self) -> &'static str {
        match self {
            TriggerKind::Contains => "contains",
            TriggerKind::StartsWith => "startswith",
            TriggerKind::EndsWith => "endswith",
            TriggerKind::WordList => "wordlist",
            TriggerKind::Regex => "regex",
            TriggerKind::Attachment => "attachment",
            TriggerKind::Sticker => "sticker",
        }
    }
}

#[derive(Debug, poise::ChoiceParameter)]
pub enum RuleAction {
    #[name = "delete the message"]
    Delete,
    #[name = "delete and warn"]
    Warn,
    #[name = "delete and time out"]
    Timeout,
    #[name = "delete and kick"]
    Kick,
    #[name = "delete and ban"]
    Ban,
    #[name = "alert staff only"]
    Alert,
}

impl RuleAction {
    fn key(&self) -> &'static str {
        match self {
            RuleAction::Delete => "delete",
            RuleAction::Warn => "warn",
            RuleAction::Timeout => "timeout",
            RuleAction::Kick => "kick",
            RuleAction::Ban => "ban",
            RuleAction::Alert => "alert",
        }
    }
}

/// Build your own automod rules.
#[poise::command(
    slash_command,
    guild_only,
    category = "Automod",
    subcommands("rule_add", "rule_remove", "rule_list", "rule_toggle", "rule_test"),
    check = "perms::admin_only"
)]
pub async fn rule(_ctx: Ctx<'_>) -> Result<(), Error> {
    Ok(())
}

/// Create or update a custom automod rule.
#[poise::command(slash_command, rename = "add", guild_only)]
pub async fn rule_add(
    ctx: Ctx<'_>,
    #[description = "Short name for the rule"] name: String,
    #[description = "What triggers it"] trigger: TriggerKind,
    #[description = "Pattern / text / word list (ignored for attachment and sticker)"]
    pattern: Option<String>,
    #[description = "What to do"] action: RuleAction,
    #[description = "Timeout length when the action is a timeout, e.g. 30m"] duration: Option<String>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let pattern = pattern.unwrap_or_default();

    let needs_pattern = !matches!(trigger, TriggerKind::Attachment | TriggerKind::Sticker);
    if needs_pattern && pattern.trim().is_empty() {
        ctx.send(
            poise::CreateReply::default()
                .embed(embed::err("That trigger needs a pattern.")),
        )
        .await?;
        return Ok(());
    }

    // Reject a regex that will not compile rather than failing silently later.
    if matches!(trigger, TriggerKind::Regex) {
        if let Err(why) = regex::Regex::new(&pattern) {
            ctx.send(poise::CreateReply::default().embed(embed::err(format!(
                "That regular expression does not compile:\n```\n{why}\n```"
            ))))
            .await?;
            return Ok(());
        }
    }

    let secs = duration
        .as_deref()
        .and_then(time::parse_duration)
        .map(|d| d.as_secs() as i64);

    let result = sqlx::query(
        "INSERT INTO automod_rules
            (guild_id, name, trigger_kind, pattern, action, duration, created_by)
         VALUES (?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(guild_id, name) DO UPDATE SET
            trigger_kind = excluded.trigger_kind,
            pattern = excluded.pattern,
            action = excluded.action,
            duration = excluded.duration",
    )
    .bind(guild_id)
    .bind(name.trim())
    .bind(trigger.key())
    .bind(&pattern)
    .bind(action.key())
    .bind(secs)
    .bind(ctx.author().id.get() as i64)
    .execute(&ctx.data().db)
    .await;

    match result {
        Ok(_) => {
            super::invalidate(guild_id);
            ctx.send(poise::CreateReply::default().embed(embed::ok(format!(
                "Rule **{}** saved: {} → {}.",
                name.trim(),
                trigger.key(),
                action.key()
            ))))
            .await?;
        }
        Err(why) => {
            ctx.send(
                poise::CreateReply::default()
                    .embed(embed::err(format!("Could not save that rule: {why}"))),
            )
            .await?;
        }
    }
    Ok(())
}

/// Delete a custom rule.
#[poise::command(slash_command, rename = "remove", guild_only)]
pub async fn rule_remove(
    ctx: Ctx<'_>,
    #[description = "Rule name"] name: String,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let result = sqlx::query("DELETE FROM automod_rules WHERE guild_id = ? AND name = ?")
        .bind(guild_id)
        .bind(name.trim())
        .execute(&ctx.data().db)
        .await?;
    super::invalidate(guild_id);
    let body = if result.rows_affected() == 0 {
        embed::err("No rule with that name.")
    } else {
        embed::ok(format!("Rule **{}** deleted.", name.trim()))
    };
    ctx.send(poise::CreateReply::default().embed(body)).await?;
    Ok(())
}

/// Switch a custom rule on or off.
#[poise::command(slash_command, rename = "toggle", guild_only)]
pub async fn rule_toggle(
    ctx: Ctx<'_>,
    #[description = "Rule name"] name: String,
    #[description = "Enabled?"] enabled: bool,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let result = sqlx::query("UPDATE automod_rules SET enabled = ? WHERE guild_id = ? AND name = ?")
        .bind(i64::from(enabled))
        .bind(guild_id)
        .bind(name.trim())
        .execute(&ctx.data().db)
        .await?;
    super::invalidate(guild_id);
    let body = if result.rows_affected() == 0 {
        embed::err("No rule with that name.")
    } else {
        embed::ok(format!(
            "Rule **{}** is now {}.",
            name.trim(),
            if enabled { "enabled" } else { "disabled" }
        ))
    };
    ctx.send(poise::CreateReply::default().embed(body)).await?;
    Ok(())
}

/// Show the custom rules and what they do.
#[poise::command(slash_command, rename = "list", guild_only)]
pub async fn rule_list(ctx: Ctx<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let rows: Vec<(String, i64, String, String, String)> = sqlx::query_as(
        "SELECT name, enabled, trigger_kind, pattern, action FROM automod_rules
         WHERE guild_id = ? ORDER BY name",
    )
    .bind(guild_id)
    .fetch_all(&ctx.data().db)
    .await?;

    let body = if rows.is_empty() {
        "No custom rules yet. Add one with `/rule add`.".to_string()
    } else {
        rows.iter()
            .map(|(name, enabled, trigger, pattern, action)| {
                format!(
                    "{} **{name}** · `{trigger}` → `{action}`\n└ `{}`",
                    if *enabled == 1 { "🟢" } else { "🔴" },
                    text::truncate(pattern, 80)
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    ctx.send(
        poise::CreateReply::default()
            .embed(embed::titled("Custom automod rules", embed::INFO).description(body)),
    )
    .await?;
    Ok(())
}

/// Check what a piece of text would trigger, without punishing anyone.
#[poise::command(slash_command, rename = "test", guild_only, ephemeral)]
pub async fn rule_test(
    ctx: Ctx<'_>,
    #[description = "Text to test"] content: String,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let mut hits: Vec<String> = Vec::new();

    let invites = text::find_invites(&content);
    if !invites.is_empty() {
        hits.push(format!("anti-invite: `{}`", invites.join("`, `")));
    }
    let domains = text::find_domains(&content);
    if !domains.is_empty() {
        let list = ctx.data().phish_domains.read();
        for domain in &domains {
            if list.contains(domain.as_str()) {
                hits.push(format!("anti-phishing: `{domain}` is on the list → ban"));
            } else if super::scan::looks_like_phish(domain) {
                hits.push(format!(
                    "anti-phishing: `{domain}` looks like impersonation → delete and alert"
                ));
            }
        }
    }
    let mentions = text::count_mentions(&content);
    if mentions > 0 {
        hits.push(format!("mentions counted: {mentions}"));
    }
    let caps = text::caps_ratio(&content);
    if caps > 0 {
        hits.push(format!("uppercase: {caps}%"));
    }

    let words: Vec<String> = sqlx::query_scalar("SELECT word FROM badwords WHERE guild_id = ?")
        .bind(guild_id)
        .fetch_all(&ctx.data().db)
        .await?;
    let normalised = text::normalize_leet(&content);
    for word in words {
        // Same rule as the scanner: whole words, unless the entry is a phrase.
        let matched = if word.contains(' ') {
            normalised.contains(&word)
        } else {
            text::contains_word(&normalised, &word)
        };
        if matched {
            hits.push(format!("bad word: `{word}`"));
        }
    }

    let rules: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT name, trigger_kind, pattern FROM automod_rules WHERE guild_id = ? AND enabled = 1",
    )
    .bind(guild_id)
    .fetch_all(&ctx.data().db)
    .await?;
    let lowered = content.to_lowercase();
    for (name, trigger, pattern) in rules {
        let matched = match trigger.as_str() {
            "contains" => lowered.contains(&pattern.to_lowercase()),
            "startswith" => lowered.starts_with(&pattern.to_lowercase()),
            "endswith" => lowered.ends_with(&pattern.to_lowercase()),
            "wordlist" => pattern
                .split(',')
                .any(|w| text::contains_word(&lowered, w.trim())),
            "regex" => regex::Regex::new(&pattern)
                .map(|re| re.is_match(&content))
                .unwrap_or(false),
            _ => false,
        };
        if matched {
            hits.push(format!("custom rule: **{name}**"));
        }
    }

    let body = if hits.is_empty() {
        "Nothing would trigger on that.".to_string()
    } else {
        hits.iter().map(|h| format!("• {h}")).collect::<Vec<_>>().join("\n")
    };

    ctx.send(
        poise::CreateReply::default()
            .embed(embed::titled("Automod test", embed::INFO).description(body))
            .ephemeral(true),
    )
    .await?;
    Ok(())
}

// ===========================================================================
// Manual raid mode
// ===========================================================================

/// Turn raid mode on or off by hand.
#[poise::command(
    slash_command,
    guild_only,
    category = "Automod",
    check = "perms::admin_only"
)]
pub async fn raidmode(
    ctx: Ctx<'_>,
    #[description = "On or off"] enabled: bool,
    #[description = "Minutes to stay on (default 30)"]
    #[min = 1]
    #[max = 1440]
    minutes: Option<i64>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap();
    super::raid::set_raid_mode(ctx.data(), guild_id, enabled, minutes.unwrap_or(30));

    let body = if enabled {
        format!(
            "Raid mode is **on** for {} minutes. New joins will be handled by the configured response.",
            minutes.unwrap_or(30)
        )
    } else {
        "Raid mode is **off**.".to_string()
    };
    ctx.send(poise::CreateReply::default().embed(embed::ok(body)))
        .await?;
    Ok(())
}

/// Remove accounts that joined in the last N minutes. Use after a raid.
#[poise::command(
    slash_command,
    guild_only,
    category = "Automod",
    check = "perms::admin_only"
)]
pub async fn raidpurge(
    ctx: Ctx<'_>,
    #[description = "Members who joined within this many minutes"]
    #[min = 1]
    #[max = 720]
    minutes: i64,
    #[description = "Ban instead of kick"] ban: Option<bool>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap();
    ctx.defer().await?;

    let removed = super::raid::purge_recent_joins(
        ctx.serenity_context(),
        ctx.data(),
        guild_id,
        minutes,
        ban.unwrap_or(false),
    )
    .await;

    ctx.send(poise::CreateReply::default().embed(embed::ok(format!(
        "Removed **{removed}** account(s) that joined in the last {minutes} minute(s)."
    ))))
    .await?;
    Ok(())
}

pub fn commands() -> Vec<poise::Command<Data, Error>> {
    vec![
        automod(),
        badword(),
        allowlist(),
        rule(),
        raidmode(),
        raidpurge(),
    ]
}
