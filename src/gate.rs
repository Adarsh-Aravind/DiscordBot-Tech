//! Central access policy.
//!
//! poise runs the checks attached to the *invoked* command, and a subcommand
//! does not inherit its parent's check. Rather than repeat `check = ...` on
//! every subcommand (and risking a forgotten one), access is
//! decided here from the command's fully qualified name, and wired in as the
//! framework-wide `command_check`.
//!
//! Matching is longest-prefix on whole path segments, so `"automod"` covers
//! every `/automod ...` subcommand while a more specific entry such as
//! `"appeal decide"` can override a looser one.

use crate::state::{Ctx, Error};
use crate::util::embed;
use crate::util::perms;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// Anyone in the server.
    Everyone,
    /// Configured staff/mod roles, or Manage Messages.
    Staff,
    /// Configured admin roles, or Manage Server.
    Admin,
}

/// Commands and command trees that need the admin tier.
const ADMIN: &[&str] = &[
    "config",
    "register",
    "automod",
    "badword",
    "allowlist",
    "rule",
    "raidmode",
    "raidpurge",
    "welcome",
    "verification",
    "rolemenu",
    "rules",
    "role",
    "levels",
    "xpadmin",
    "tickets",
    "counting",
    "wordchain",
    "customcommand",
    "workflow",
    "webhook",
    "weeklyreport",
    "schedule",
    "milestonechannel",
    "countchannel",
    "feedbackchannel",
    "bonusinvites",
    "lockdown",
    "clearwarns",
    "purgeuser",
    "escalation",
    "starboard setup",
    "starboard off",
    "voice setup",
    "suggest channel",
    "dailyquestion channel",
    "confess channel",
    "confess reveal",
    "confess ban",
    "birthday channel",
];

/// Commands and command trees that need the staff tier.
const STAFF: &[&str] = &[
    "warn",
    "note",
    "history",
    "warnings",
    "unwarn",
    "modlogs",
    "case",
    "purge",
    "lock",
    "unlock",
    "slowmode",
    "nick",
    "snipe",
    "editsnipe",
    "embed",
    "announce",
    "quiz",
    "giveaway",
    "appeal list",
    "appeal decide",
    "suggest status",
    "event create",
    "event cancel",
    "event ping",
    "challenge create",
    "challenge end",
    "quote remove",
    "trivia add",
    "dailyquestion now",
    "dailyquestion add",
    "stats staff",
    "stats moderation",
    "ticket claim",
    "ticket assign",
    "ticket add",
    "ticket remove",
    "ticket priority",
    "ticket rename",
    "ticket reopen",
    "ticket transcript",
];

/// Does `name` start with `prefix` on a whole-segment boundary?
fn matches(name: &str, prefix: &str) -> bool {
    name == prefix
        || (name.starts_with(prefix) && name.as_bytes().get(prefix.len()) == Some(&b' '))
}

/// The tier required for a command, by longest matching prefix.
pub fn required_access(qualified_name: &str) -> Access {
    let mut best: (usize, Access) = (0, Access::Everyone);

    for entry in ADMIN {
        if matches(qualified_name, entry) && entry.len() > best.0 {
            best = (entry.len(), Access::Admin);
        }
    }
    for entry in STAFF {
        if matches(qualified_name, entry) && entry.len() > best.0 {
            best = (entry.len(), Access::Staff);
        }
    }
    best.1
}

/// The framework-wide check. Also bootstraps the guild's config rows so the
/// first command run in a new server does not race the defaults.
pub async fn check(ctx: Ctx<'_>) -> Result<bool, Error> {
    if let Some(guild_id) = ctx.guild_id() {
        let _ = crate::db::ensure_guild(&ctx.data().db, guild_id.get() as i64).await;
    }

    let name = ctx.command().qualified_name.clone();
    let required = required_access(&name);

    if required == Access::Everyone {
        return Ok(true);
    }

    // Bot owners always pass.
    if ctx.data().is_owner(ctx.author().id.get()) {
        return Ok(true);
    }

    let allowed = match required {
        Access::Everyone => true,
        Access::Staff => perms::is_staff(&ctx).await,
        Access::Admin => perms::is_admin(&ctx).await,
    };

    if allowed {
        return Ok(true);
    }

    let (tier, hint) = match required {
        Access::Admin => (
            "administrator",
            "You need **Manage Server** or a role set with `/config role admin`.",
        ),
        _ => (
            "staff",
            "You need **Manage Messages** or a role set with `/config role`.",
        ),
    };

    ctx.send(
        poise::CreateReply::default()
            .embed(
                embed::err(format!("`/{name}` is {tier} only."))
                    .footer(embed::footer(hint)),
            )
            .ephemeral(true),
    )
    .await?;
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_segments_only() {
        // "rule" must not match "rules" or the two trees would blur together.
        assert!(matches("rule add", "rule"));
        assert!(!matches("rules post", "rule"));
    }

    #[test]
    fn trees_are_covered() {
        assert_eq!(required_access("automod exempt add"), Access::Admin);
        assert_eq!(required_access("config log"), Access::Admin);
        assert_eq!(required_access("warn"), Access::Staff);
    }

    #[test]
    fn longest_prefix_wins() {
        // /appeal is open to everyone, but reviewing appeals is not.
        assert_eq!(required_access("appeal submit"), Access::Everyone);
        assert_eq!(required_access("appeal decide"), Access::Staff);
        assert_eq!(required_access("starboard top"), Access::Everyone);
        assert_eq!(required_access("starboard setup"), Access::Admin);
        assert_eq!(required_access("ticket new"), Access::Everyone);
        assert_eq!(required_access("ticket close"), Access::Everyone);
        assert_eq!(required_access("ticket claim"), Access::Staff);
        assert_eq!(required_access("tickets panel"), Access::Admin);
    }

    #[test]
    fn open_commands_stay_open() {
        for name in [
            "rank", "leaderboard", "profile view", "help", "ping", "avatar", "weather",
            "poll create", "suggest new", "trivia play", "invites", "birthday set",
            "voice name", "stats server", "remind me", "confess send", "quote add",
        ] {
            assert_eq!(required_access(name), Access::Everyone, "{name}");
        }
    }
}
