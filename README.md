# MrTechTrivia Discord bot

A community management bot for the **MrTechTrivia** Discord, written in Rust.

- **Stack:** Rust 2021 · [serenity](https://github.com/serenity-rs/serenity) 0.12 · [poise](https://github.com/serenity-rs/poise) 0.6 · SQLite via `sqlx`
- **Scope:** moderation and anti-abuse, welcome and roles, levelling,
  community engagement, voice rooms, announcements and reminders, utilities
- **Commands:** 82 top-level slash commands, 190 commands including subcommands
  (Discord allows at most 100 top-level slash commands; a test enforces it)
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
3. **Configure the environment.** A ready-to-fill `.env` ships with the repo working
   copy (it is gitignored and never committed); if it is missing, copy the reference:

```bash
cp .env.example .env
```

   Fill in `DISCORD_TOKEN` and `OWNER_IDS`, and set `DEV_GUILD_ID` to your server's ID
   while you are setting things up — slash commands then register instantly instead of
   taking up to an hour to propagate globally. Everything else has a working default.

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

   and `/help` for the full command list.

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
| Manage Channels | lockdown, join-to-create rooms, slowmode |
| Move Members | AFK moving, join-to-create |
| Manage Nicknames | `/nick` |
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
| Anti-phishing links | built-in domain list (ban), brand-impersonation heuristics (delete + alert), optional remote feed |
| Anti-invite links | `/automod toggle` |
| Anti-mention spam | configurable mention limit |
| Bad-word filter | `/badword add/remove/list`, matches through leetspeak substitutions |
| Duplicate message detection | similarity-based, configurable repeat limit |
| Suspicious account detection | account-age gate on join → alert or kick |
| New-member verification | `/verification setup` + button panel, optional account-age gate |
| Automatic punishment escalation | `/escalation set` — ladder keyed on active warning count |
| Moderation appeals | `/appeal submit`, `/appeal list`, `/appeal decide` (accepting a ban appeal lifts the ban) |
| Moderator action logs | `/modlogs <moderator>` |
| Custom automod rules | `/rule add` — contains, starts/ends with, word list, regex, attachment, sticker; `/rule test` dry-runs text |
| Channel lockdown | `/lock`, `/unlock` — saves and restores the previous overwrite |
| Emergency server lockdown | `/lockdown start`, `/lockdown end`, `/lockdown status` |

### 2. Welcome and member management

Custom welcome messages (channel and/or DM, plain or embed) · auto-role assignment
(separate list for bots) · button roles · dropdown roles · self-role menus
(`/rolemenu`) · rules acceptance (`/rules`) · member profiles (`/profile`) · custom
nicknames (`/nick`).

Placeholders for welcome and goodbye text: `{mention}` `{user}` `{tag}` `{id}`
`{server}` `{count}`. (`{inviter}` from the old invite tracking still renders, as
"someone", so existing messages do not show raw braces.)

### 3. Levelling and XP

Message-based XP with a configurable range and cooldown · voice-based XP accrued every
five minutes · custom XP rates and a server-wide multiplier · level-up announcements
(channel or DM) · **rank cards rendered as PNGs** · server leaderboard · role rewards
(stacking or highest-only) · per-role and per-channel XP multipliers · `/xptransfer` ·
`/xpadmin reset` / `resetall` · prestige · 11 achievement badges · daily activity
streaks with a small XP bonus · profile customisation · `/levelcurve`.

> **Rank cards** need a TrueType font. The bot looks for `assets/font.ttf`, then common
> system fonts (`segoeui.ttf`, `DejaVuSans.ttf`, …), or you can set `RANK_CARD_FONT`.
> With no font available `/rank` falls back to an embed with a text progress bar —
> same information, no image.

### 4. Community engagement

Polls (`/poll`, anonymous and multiple-choice, auto-closing) · giveaways with required
roles, bonus entries, a minimum level, rerolls and winner DMs · events with RSVP
buttons, attendee caps and 15-minute reminders · a daily question with a 20-question
starter pool · tech trivia (25 seeded questions, add your own) · timed `/quiz` rounds ·
a counting channel · word chain · anonymous feedback to staff (`/feedback`) · quote
collection · community challenges with role and XP rewards · member-count milestones ·
server anniversary announcements.

### 5. Voice channel management

Join-to-create temporary rooms · owner controls (`/voice name`, `limit`, `lock`,
`unlock`, `private`, `public`, `permit`, `reject`) · ownership transfer and `/voice
claim` when the owner leaves · voice activity XP · session tracking · AFK channel with
an idle timeout · automatic deletion of empty rooms (including orphans left by a
restart) · `/voice stats`.

### 6. Announcements and reminders

`/announce` posts an announcement now · reminders (`/remind me`, repeating, DM or
channel).

### 7. Utilities

`/avatar` `/banner` `/userinfo` `/serverinfo` `/servericon` `/roleinfo` `/channelinfo`
`/botinfo` `/ping` `/uptime` `/poll` `/remind` `/calculator` `/translate` `/weather`
`/embed` `/afk` `/snipe` `/editsnipe` `/permissions`
`/timestamp` `/help`.

- `/help` lists all 190 commands grouped by category, with a badge per command for
  who may run it (🟢 anyone, 🔵 staff, 🔴 admin, 🔑 a Discord permission) taken from
  the gate itself, so it cannot drift. `/help category:Moderation` lists one group with
  descriptions; `/help command:remind me` explains a single subcommand.
- `/weather` uses **Open-Meteo** and needs no API key.
- `/calculator` is a self-contained expression evaluator (no `eval`, no dependency).
- `/translate` needs a LibreTranslate-compatible endpoint in `TRANSLATE_API_URL`.
  Without one it says so rather than failing silently.

---

## Removed features

Analytics (`/stats`, `/membercount`, `/countchannel` and all activity tracking), tickets,
starboard, suggestions, confessions, birthdays, invite tracking (another bot handles
it), scheduled messages, custom commands, workflows, outbound webhooks and weekly
reports were removed. Their tables and columns are dropped on startup by
`retire_removed_features` in `db.rs`.

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
  util/                embeds, time, text, permissions, caching, rank-card rendering
  modules/
    moderation/        case engine + actions, cases, purge, lockdown
    automod/            message scanning, raid, nuke, rules UI
    welcome/            join/leave, roles, profiles
    leveling/           XP engine + commands
    community/          polls, feedback, giveaways, events, games, misc
    voice/              join-to-create and tracking
    automation.rs       announcements and reminders
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
  A prefix allowlist has one weakness - anything nobody adds to it is public - so a test
  walks every registered command and fails if one is ungated, carries no
  `required_permissions` and no `check`, unless it is listed as deliberately open.
- **Only a known scam domain is an automatic ban.** The impersonation heuristic works on
  brand names in a hostname, which will see false positives, so it deletes and alerts and
  leaves the decision to staff. The host of a URL is read as the part after the last `@`,
  because `https://discord.com@evil.site/` goes to `evil.site`.
- **Counters are handed out in one statement.** Case and quote numbers come
  from an `UPSERT ... RETURNING`, so two moderators acting at the same instant cannot be
  given the same number - which used to mean the second case failed to save *after* the
  ban had already happened.
- **XP is written with a compare-and-set.** Read-modify-write loses an award whenever two
  land together, and transfers debit with a conditional `UPDATE` so the same XP cannot be
  spent twice.
- **Lockdown saves the previous overwrite** before denying `Send Messages`, so unlocking
  restores exactly what was there instead of wiping staff-made permissions.
- **Anti-nuke strips roles and times out rather than banning.** A false positive that
  bans a senior admin is worse than one a human has to finish, so the destructive step
  is left to staff. The owner is DMed because the log channels may be gone.
- **Anonymous feedback stores no author at all.**
- **Timestamps are one format everywhere** (`%Y-%m-%d %H:%M:%S`, UTC), matching
  SQLite's `datetime('now')`, so Rust and SQL agree without timezone surprises.
- **No `sqlx` compile-time macros.** All queries are runtime (`sqlx::query*`), so the
  project builds without a live database or `DATABASE_URL` set at compile time.
- **Cache guards are never held across an `await`.** Values are copied out of the
  serenity cache in a scoped block first, which keeps the futures `Send`.

### Background jobs

| Interval | Work |
|---|---|
| 30 s | expired timeouts and temp bans, reminders, empty temp voice rooms past their grace period |
| 1 min | closing polls, drawing giveaways, event reminders |
| 5 min | voice XP accrual, AFK sweep |
| 1 h | challenge rewards, pruning |
| 15 min | the once-a-day batch: daily question, anniversaries, achievement refresh (runs after 09:00 UTC) |

Pruning keeps the database small: the message log is trimmed to 14 days (it exists only
for edit/delete logs and `/snipe`), closed voice sessions to 90 days, and short-lived
settings rows once they are
past their expiry. The hourly job also prunes the in-memory automod windows and snipe
slots, so a long-running process does not keep an entry for every member it has ever
seen - and `/snipe` cannot resurrect something deleted days ago.

---

## Operating notes

- **Data lives in `data/bot.db`.** Back up that file (plus `-wal`) to back up everything.
  `/config export` gives a per-server JSON snapshot of the settings.
- **The bot shuts down cleanly on Ctrl-C** so SQLite flushes its write-ahead log. Prefer
  it over killing the process.
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
