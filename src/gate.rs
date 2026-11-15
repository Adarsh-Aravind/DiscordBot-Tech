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
    "counting",
    "wordchain",
    "milestonechannel",
    "feedbackchannel",
    "lockdown",
    "clearwarns",
    "purgeuser",
    "escalation",
    "voice setup",
    "dailyquestion channel",
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
    "event create",
    "event cancel",
    "event ping",
    "challenge create",
    "challenge end",
    "quote remove",
    "trivia add",
    "dailyquestion now",
    "dailyquestion add",
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
            "You need a moderator permission (Manage Messages, Kick, Ban or Timeout), \
             or a role set with `/config role`.",
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
        assert_eq!(required_access("event list"), Access::Everyone);
        assert_eq!(required_access("event create"), Access::Staff);
        assert_eq!(required_access("dailyquestion channel"), Access::Admin);
    }

    /// Commands that anyone in the server may run.
    ///
    /// This list is the safety net for the one weakness of a prefix allowlist:
    /// a command nobody adds to `ADMIN` or `STAFF` is public by default. Adding
    /// a command that is not gated, has no `required_permissions` and no
    /// `check` now fails this test, and the fix is either to gate it or to say
    /// here, on purpose, that it is open to everyone.
    const OPEN_TO_EVERYONE: &[&str] = &[
        "achievements",
        "afk",
        "appeal submit",
        "avatar",
        "banner",
        "botinfo",
        "calculator",
        "challenge list",
        "challenge progress",
        "channelinfo",
        "event info",
        "event list",
        "feedback",
        "help",
        "leaderboard",
        "levelcurve",
        "mystats",
        "permissions",
        "ping",
        "poll close",
        "poll create",
        "poll results",
        "prestige",
        "profile banner",
        "profile bio",
        "profile clear",
        "profile colour",
        "profile favourite",
        "profile pronouns",
        "profile timezone",
        "profile view",
        "quote add",
        "quote get",
        "quote list",
        "quote random",
        "rank",
        "remind cancel",
        "remind list",
        "remind me",
        "roleinfo",
        "servericon",
        "serverinfo",
        "timestamp",
        "translate",
        "trivia leaderboard",
        "trivia play",
        "trivia stats",
        "uptime",
        "userinfo",
        "voice claim",
        "voice info",
        "voice limit",
        "voice lock",
        "voice name",
        "voice permit",
        "voice private",
        "voice public",
        "voice reject",
        "voice stats",
        "voice transfer",
        "voice unlock",
        "weather",
        "xptransfer",
    ];

    /// Walk every registered command and collect the ones nothing protects.
    fn unprotected(
        command: &poise::Command<crate::state::Data, crate::state::Error>,
        out: &mut Vec<String>,
    ) {
        if !command.subcommands.is_empty() {
            // A group is not invokable on its own; only its leaves are.
            for sub in &command.subcommands {
                unprotected(sub, out);
            }
            return;
        }

        let gated = required_access(&command.qualified_name) != Access::Everyone;
        let needs_discord_permission = !command.required_permissions.is_empty();
        let has_own_check = !command.checks.is_empty();

        if !gated && !needs_discord_permission && !has_own_check {
            out.push(command.qualified_name.clone());
        }
    }

    /// Every command with `qualified_name` filled in ("profile view", not
    /// "view"). poise only does this when it builds the framework, so the tests
    /// have to do it themselves or they see bare subcommand names.
    fn all_commands() -> Vec<poise::Command<crate::state::Data, crate::state::Error>> {
        let mut commands = crate::modules::all();
        poise::framework::set_qualified_names(&mut commands);
        commands
    }

    #[test]
    fn nothing_is_accidentally_public() {
        let mut open = Vec::new();
        for command in all_commands() {
            unprotected(&command, &mut open);
        }

        let unexpected: Vec<&String> = open
            .iter()
            .filter(|name| !OPEN_TO_EVERYONE.contains(&name.as_str()))
            .collect();

        assert!(
            unexpected.is_empty(),
            "these commands are open to everyone and nothing says they should be.\n\
             Add them to ADMIN or STAFF in gate.rs, give them required_permissions, \
             or list them in OPEN_TO_EVERYONE if that is deliberate:\n{unexpected:#?}"
        );
    }

    /// The other direction: an entry here that no longer matches a command is
    /// stale, and stale entries are how a real command ends up excused.
    #[test]
    fn the_open_list_has_no_leftovers() {
        let mut open = Vec::new();
        for command in all_commands() {
            unprotected(&command, &mut open);
        }

        let stale: Vec<&&str> = OPEN_TO_EVERYONE
            .iter()
            .filter(|name| !open.iter().any(|found| found == *name))
            .collect();

        assert!(
            stale.is_empty(),
            "these are listed as open to everyone but no longer match an \
             ungated command - remove them from OPEN_TO_EVERYONE:\n{stale:#?}"
        );
    }

    #[test]
    fn open_commands_stay_open() {
        for name in [
            "rank", "leaderboard", "profile view", "help", "ping", "avatar", "weather",
            "poll create", "feedback", "trivia play", "remind me", "voice name",
            "quote add", "profile timezone", "serverinfo",
        ] {
            assert_eq!(required_access(name), Access::Everyone, "{name}");
        }
    }

    /// Discord rejects the whole registration (and the bot fails to start) past
    /// 100 top-level slash commands. Group new commands as subcommands instead.
    #[test]
    fn slash_commands_fit_discord_limit() {
        // Same rule poise uses in `create_as_slash_command`.
        let slash = crate::modules::all()
            .iter()
            .filter(|c| c.slash_action.is_some())
            .count();
        assert!(slash <= 100, "{slash} top-level slash commands; Discord allows 100");
    }
}
