# MrTechTrivia Discord bot

A community management bot for the **MrTechTrivia** Discord, written in Rust.

- **Stack:** Rust 2021 · [serenity](https://github.com/serenity-rs/serenity) 0.12 · [poise](https://github.com/serenity-rs/poise) 0.6 · SQLite via `sqlx`
- **Scope:** moderation and anti-abuse, welcome and roles, levelling, tickets,
  community engagement, voice rooms, analytics, automation, utilities
- **Commands:** 65 top-level commands, ~300 command definitions including subcommands
- **Storage:** one SQLite file, created and migrated on first run — no external services required

---

## Quick start

```bash
cargo build --release
```

1. **Create the application.** Go to the
   [Discord developer portal](https://discord.com/developers/applications) → *New Application* →
   *Bot* → *Reset Token*, and copy the token.
2. **Enable the privileged intents.** On the same Bot page, switch on
   **Server Members Intent** and **Message Content Intent**. The bot will not start
   correctly without them: member tracking, welcome messages, automod, levelling and
   logging all depend on them.
3. **Configure the environment.**

```bash
cp .env.example .env
```

   Fill in `DISCORD_TOKEN`, and set `DEV_GUILD_ID` to your server's ID while you are
   setting things up — slash commands then register instantly instead of taking up to
   an hour to propagate globally.

4. **Invite the bot** with the scopes `bot` and `applications.commands`, and the
   permissions listed under [Permissions](#permissions).
5. **Run it.**

```bash
cargo run --release
```

6. **Set it up in Discord.** Start with:

```
/config overview
```

   That lists what is configured and what is still missing, with the exact command for
   each remaining step.

---

## Permissions

The bot asks Discord for nothing it does not use. The minimum for everything to work:

| Permission | Needed for |
|---|---|
| View Channels, Send Messages, Embed Links, Attach Files, Read Message History | everything |
| Manage Messages | purge, automod deletions, pinning |
| Manage Roles | auto-roles, role menus, verification, level rewards |
| Kick Members, Ban Members | `/kick`, `/ban`, raid response |
| Moderate Members | `/timeout`, automod timeouts |
| Manage Channels | tickets, lockdown, join-to-create rooms, slowmode |
| Move Members | AFK moving, join-to-create |
| Manage Nicknames | `/nick` |
| Manage Server | invite tracking (reading the invite list) |
| View Audit Log | anti-nuke |
| Add Reactions | counting and word-chain feedback |

**Role position matters.** The bot can only act on members and roles *below* its own
highest role. `/permissions` reports what it is missing, and most failures reply with a
hint about role position rather than a bare error.

---

## Feature map

Everything below is implemented. Where a feature has a caveat, it is stated.

### 1. Moderation and security

| Feature | Commands / behaviour |
|---|---|
| Ban / unban | `/ban` (optionally temporary, with message deletion), `/hackban` by ID, `/unban` |
| Kick | `/kick`, `/softban` (ban + unban to clear messages) |
| Timeout / remove timeout | `/timeout`, `/untimeout` — uses Discord's native timeout |
| Warning system | `/warn`, `/unwarn`, `/clearwarns` |
| Warning history | `/warnings`, `/history` |
| Moderation case IDs | every action gets a sequential per-server case number |
| Message purge / bulk delete | `/purge` with author, text and type filters; `/purgeuser` server-wide |
| Message edit / delete logs | automatic, with before/after and attachment URLs |
| Anti-spam | per-member message rate, configurable window |
| Anti-flood | per-channel message rate |
| Anti-raid | join-burst detection → lockdown, kick, ban or alert; raid mode |
| Anti-nuke | audit-log watch for destructive bursts → strips roles, timeout, alerts the owner |
| Anti-phishing links | built-in domain list, brand-impersonation heuristics, optional remote feed |
| Anti-invite links | `/automod toggle` |
| Anti-mention spam | configurable mention limit |
| Bad-word filter | `/badword add/remove/list`, matches through leetspeak substitutions |
| Duplicate message detection | similarity-based, configurable repeat limit |
| Suspicious account detection | account-age gate on join → alert or kick |
| New-member verification | `/verification setup` + button panel, optional account-age gate |
| Automatic punishment escalation | `/escalation set` — ladder keyed on active warning count |
| Moderation appeals | `/appeal submit`, `/appeal list`, `/appeal decide` (accepting a ban appeal lifts the ban) |
| Moderator action logs | `/modlogs <moderator>`, `/stats staff` |
| Custom automod rules | `/rule add` — contains, starts/ends with, word list, regex, attachment, sticker; `/rule test` dry-runs text |
| Channel lockdown | `/lock`, `/unlock` — saves and restores the previous overwrite |
| Emergency server lockdown | `/lockdown start`, `/lockdown end`, `/lockdown status` |

### 2. Welcome and member management

Custom welcome messages (channel and/or DM, plain or embed) · auto-role assignment
(separate list for bots) · button roles · dropdown roles · self-role menus
(`/rolemenu`) · rules acceptance (`/rules`) · member profiles (`/profile`) · member
activity tracking (`/stats me`) · invite tracking (`/invites`, `/inviteboard`,
`/inviteinfo`, `/bonusinvites`) · custom nicknames (`/nick`).

Placeholders for welcome and goodbye text: `{mention}` `{user}` `{tag}` `{id}`
`{server}` `{count}` `{inviter}`.

### 3. Levelling and XP

Message-based XP with a configurable range and cooldown · voice-based XP accrued every
five minutes · custom XP rates and a server-wide multiplier · level-up announcements
(channel or DM) · **rank cards rendered as PNGs** · server leaderboard · role rewards
(stacking or highest-only) · per-role and per-channel XP multipliers · `/xptransfer` ·
`/xpadmin reset` / `resetall` · prestige · 12 achievement badges · daily activity
streaks with a small XP bonus · profile customisation · `/levelcurve`.

> **Rank cards** need a TrueType font. The bot looks for `assets/font.ttf`, then common
> system fonts (`segoeui.ttf`, `DejaVuSans.ttf`, …), or you can set `RANK_CARD_FONT`.
> With no font available `/rank` falls back to an embed with a text progress bar —
> same information, no image.

### 4. Tickets and support

Ticket panels with buttons or a dropdown · multiple categories with their own support
roles, channel category and template · private channels · claiming · assignment ·
add/remove members · four priority levels (reflected in the channel name) · closing
with a reason · reopening (carrying the old conversation across) · plain-text
transcripts attached to the log and DMed to the opener · automatic closure after a
configurable idle period · 1–5 star feedback ratings · first-response tracking ·
`/tickets stats` (volume, average first response, average time to close, average
rating) · `/tickets staff` leaderboard.

### 5. Community engagement

Polls (`/poll`, anonymous and multiple-choice, auto-closing) · suggestions with voting
and a status workflow that DMs the author · giveaways with required roles, bonus
entries, a minimum level, rerolls and winner DMs · scheduled announcements
(`/schedule`) · events with RSVP buttons, attendee caps and 15-minute reminders ·
birthday announcements with a role for the day · a daily question with a 20-question
starter pool · tech trivia (25 seeded questions, add your own) · timed `/quiz` rounds ·
a counting channel · word chain · confessions (anonymous, with an audited
`/confess reveal` for abuse) · anonymous feedback · starboard · quote collection ·
community challenges with role and XP rewards · member-count milestones · server
anniversary announcements.

### 6. Voice channel management

Join-to-create temporary rooms · owner controls (`/voice name`, `limit`, `lock`,
`unlock`, `private`, `public`, `permit`, `reject`) · ownership transfer and `/voice
claim` when the owner leaves · voice activity XP · session tracking · AFK channel with
an idle timeout · automatic deletion of empty rooms (including orphans left by a
restart) · `/voice stats`.

### 7. Analytics

`/stats server` · `/stats members` (growth, best/worst day, rejoins) ·
`/stats activity` (DAU/WAU/MAU and stickiness) · `/stats channels` ·
`/stats peak` (hourly heat chart) · `/stats retention` · `/stats staff` ·
`/stats commands` · `/stats moderation` · `/stats me` · `/membercount` ·
`/countchannel` (live member count in a channel name) · `/botinfo` for uptime.

### 8. Automation

Scheduled and repeating messages · reminders (`/remind me`, repeating, DM or channel) ·
a workflow engine (`/workflow`) with seven triggers, six actions, threshold conditions
and delayed execution · custom text commands (`/customcommand`) · outbound webhook
integrations · automatic weekly reports.

### 9. Utilities

`/avatar` `/banner` `/userinfo` `/serverinfo` `/servericon` `/roleinfo` `/channelinfo`
`/botinfo` `/ping` `/uptime` `/poll` `/remind` `/calculator` `/translate` `/weather`
`/embed` `/afk` `/snipe` `/editsnipe` `/inviteinfo` `/membercount` `/permissions`
`/timestamp` `/help`.

- `/weather` uses **Open-Meteo** and needs no API key.
- `/calculator` is a self-contained expression evaluator (no `eval`, no dependency).
- `/translate` needs a LibreTranslate-compatible endpoint in `TRANSLATE_API_URL`.
  Without one it says so rather than failing silently.

---

## Not included, and why

Your list placed **gaming statistics, clan rankings, tournament statistics and economy
statistics** under analytics, but the gaming, clan, tournament and economy *modules*
those would report on were not part of the request (sections 4, 5 and 9 of your
numbering are absent). Reporting on data nothing produces would be an empty screen, so
those four analytics items are not implemented. Everything else on your list is.

If you want the economy, clans or tournaments themselves, they slot in alongside the
existing modules and their statistics then follow naturally.

---

## Architecture

```
src/
  main.rs              startup, framework options, error handling
  config.rs            process configuration from the environment
  state.rs             BotState: pool, config, in-memory counters; Data = Arc<BotState>
  db.rs                pool, schema bootstrap, typed per-guild config rows, seed data
  gate.rs              one access-policy table covering every (sub)command
  compat.rs            wrappers for the serenity calls that move between versions
  logs.rs              one place that decides which channel audit output goes to
  events.rs            the gateway event router
  tasks.rs             background loops (5 intervals) and the daily jobs
  util/                embeds, time, text, permissions, rank-card rendering
  modules/
    moderation/        case engine + actions, cases, purge, lockdown
    automod/            message scanning, raid, nuke, rules UI
    welcome/            join/leave, roles, invites, profiles
    leveling/           XP engine + commands
    tickets/            lifecycle + commands
    community/          polls, suggestions, giveaways, events, games, starboard, misc
    voice/              join-to-create and tracking
    analytics.rs        counters and reports
    automation.rs       scheduler, workflows, custom commands, webhooks
    utility.rs          information and tools
    settings.rs         /config
migrations/schema.sql   the whole schema, idempotent, run on every boot
```

A few decisions worth knowing about:

- **Every punishment goes through `moderation::punish`.** Manual commands, automod and
  escalation all call it, so the case record, the member DM, the mod-log embed and the
  escalation check can never drift apart.
- **Access control lives in `gate.rs`.** poise runs the checks on the command that was
  invoked, and a subcommand does not inherit its parent's `check`. Rather than repeat a
  check on every subcommand and risk missing one, the policy is a single table matched
  by longest command-path prefix, wired in as the framework-wide `command_check`. It has
  unit tests, including that `/appeal submit` stays open while `/appeal decide` does not.
- **Lockdown saves the previous overwrite** before denying `Send Messages`, so unlocking
  restores exactly what was there instead of wiping staff-made permissions.
- **Anti-nuke strips roles and times out rather than banning.** A false positive that
  bans a senior admin is worse than one a human has to finish, so the destructive step
  is left to staff. The owner is DMed because the log channels may be gone.
- **Confessions store the author.** They are never displayed, but `/confess reveal`
  exists for abuse handling and writes every use to the mod log, so it cannot be done
  quietly. Anonymous feedback stores no author at all.
- **Timestamps are one format everywhere** (`%Y-%m-%d %H:%M:%S`, UTC), matching
  SQLite's `datetime('now')`, so Rust and SQL agree without timezone surprises.
- **No `sqlx` compile-time macros.** All queries are runtime (`sqlx::query*`), so the
  project builds without a live database or `DATABASE_URL` set at compile time.
- **Cache guards are never held across an `await`.** Values are copied out of the
  serenity cache in a scoped block first, which keeps the futures `Send`.

### Background jobs

| Interval | Work |
|---|---|
| 30 s | expired timeouts and temp bans, scheduled messages, reminders, delayed workflow actions |
| 1 min | closing polls, drawing giveaways, event reminders |
| 5 min | voice XP accrual, AFK sweep, idle ticket auto-close |
| 1 h | weekly reports, member-count channels, challenge rewards, pruning, member-count snapshots |
| 15 min | the once-a-day batch: birthdays, daily question, anniversaries, achievement refresh (runs after 09:00 UTC) |

Pruning keeps the database small: the message log is trimmed to 14 days (it exists only
for edit/delete logs and `/snipe`), per-day activity to ~13 months, closed voice
sessions to 90 days.

---

## Operating notes

- **Data lives in `data/bot.db`.** Back up that file (plus `-wal`) to back up everything.
  `/config export` gives a per-server JSON snapshot of the settings.
- **The bot shuts down cleanly on Ctrl-C** so SQLite flushes its write-ahead log. Prefer
  it over killing the process.
- **Analytics start when the bot joins.** It cannot backfill history it never saw; the
  relevant embeds say so rather than implying the numbers are complete.
- **Invite tracking needs Manage Server** and works by diffing invite use counts on each
  join — the standard approach, and the only one Discord allows. It cannot attribute a
  join through a vanity URL or a widget.
- **Logging:** set `RUST_LOG=mrtechtrivia_bot=debug` for verbose output.

### Running as a service (systemd)

```ini
[Unit]
Description=MrTechTrivia Discord bot
After=network-online.target

[Service]
Type=simple
WorkingDirectory=/opt/mrtechtrivia-bot
EnvironmentFile=/opt/mrtechtrivia-bot/.env
ExecStart=/opt/mrtechtrivia-bot/target/release/mrtechtrivia-bot
Restart=on-failure
RestartSec=10

[Install]
WantedBy=multi-user.target
```

---

## Development

```bash
cargo check
cargo clippy --all-targets
cargo test
cargo fmt
```

`cargo test` runs the access-policy tests in `gate.rs`. The expression evaluator in
`modules/utility.rs` and the helpers in `util/` are the other natural places to add
tests.

To add a feature area: create `src/modules/<name>.rs` (or a directory with `mod.rs`),
expose `pub fn commands() -> Vec<poise::Command<Data, Error>>`, add it to
`modules::all()`, add any tables to `migrations/schema.sql` (always
`CREATE TABLE IF NOT EXISTS`), and add an entry to `gate.rs` if it needs restricting.

## Licence

MIT.
