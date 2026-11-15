-- MrTechTrivia bot schema. Executed on every startup; every statement is idempotent.
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;

-- ===========================================================================
-- Core guild configuration
-- ===========================================================================
CREATE TABLE IF NOT EXISTS guild_config (
    guild_id          INTEGER PRIMARY KEY,
    prefix            TEXT    NOT NULL DEFAULT '!',
    modlog_channel    INTEGER,
    msglog_channel    INTEGER,
    joinlog_channel   INTEGER,
    voicelog_channel  INTEGER,
    serverlog_channel INTEGER,
    mute_role         INTEGER,
    mod_roles         TEXT    NOT NULL DEFAULT '',
    admin_roles       TEXT    NOT NULL DEFAULT '',
    staff_roles       TEXT    NOT NULL DEFAULT '',
    appeal_channel    INTEGER,
    report_channel    INTEGER,
    timezone          TEXT    NOT NULL DEFAULT 'UTC',
    locked_down       INTEGER NOT NULL DEFAULT 0,
    created_at        TEXT    NOT NULL DEFAULT (datetime('now'))
);

-- Free-form settings for the long tail of toggles.
CREATE TABLE IF NOT EXISTS kv_settings (
    guild_id   INTEGER NOT NULL,
    key        TEXT    NOT NULL,
    value      TEXT    NOT NULL,
    -- When set, the row is rubbish after this instant. Used for the short lived
    -- keys (in-flight trivia answers) so a sweep can drop the expired ones
    -- without taking the live ones with them.
    expires_at TEXT,
    PRIMARY KEY (guild_id, key)
);

-- Monotonic per-guild counters: case numbers, quote numbers and so on.
-- Handing out a number is a single UPSERT ... RETURNING, so two concurrent
-- callers can never be given the same one.
CREATE TABLE IF NOT EXISTS counters (
    guild_id INTEGER NOT NULL,
    name     TEXT    NOT NULL,
    value    INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (guild_id, name)
);

-- ===========================================================================
-- Moderation
-- ===========================================================================
CREATE TABLE IF NOT EXISTS mod_cases (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    guild_id    INTEGER NOT NULL,
    case_no     INTEGER NOT NULL,
    action      TEXT    NOT NULL,
    target_id   INTEGER NOT NULL,
    target_tag  TEXT    NOT NULL DEFAULT '',
    mod_id      INTEGER NOT NULL,
    mod_tag     TEXT    NOT NULL DEFAULT '',
    reason      TEXT    NOT NULL DEFAULT 'No reason provided',
    duration    INTEGER,
    expires_at  TEXT,
    active      INTEGER NOT NULL DEFAULT 1,
    automated   INTEGER NOT NULL DEFAULT 0,
    created_at  TEXT    NOT NULL DEFAULT (datetime('now'))
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_cases_guild_no ON mod_cases(guild_id, case_no);
CREATE INDEX IF NOT EXISTS idx_cases_target ON mod_cases(guild_id, target_id);
CREATE INDEX IF NOT EXISTS idx_cases_mod ON mod_cases(guild_id, mod_id);
CREATE INDEX IF NOT EXISTS idx_cases_expiry ON mod_cases(active, expires_at);

CREATE TABLE IF NOT EXISTS appeals (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    guild_id   INTEGER NOT NULL,
    case_no    INTEGER,
    user_id    INTEGER NOT NULL,
    content    TEXT    NOT NULL,
    status     TEXT    NOT NULL DEFAULT 'pending',
    reviewer   INTEGER,
    note       TEXT,
    created_at TEXT    NOT NULL DEFAULT (datetime('now')),
    decided_at TEXT
);

CREATE TABLE IF NOT EXISTS escalation_rules (
    guild_id   INTEGER NOT NULL,
    warn_count INTEGER NOT NULL,
    action     TEXT    NOT NULL,
    duration   INTEGER,
    PRIMARY KEY (guild_id, warn_count)
);

CREATE TABLE IF NOT EXISTS lockdowns (
    guild_id     INTEGER NOT NULL,
    channel_id   INTEGER NOT NULL,
    prev_allow   INTEGER NOT NULL DEFAULT 0,
    prev_deny    INTEGER NOT NULL DEFAULT 0,
    had_override INTEGER NOT NULL DEFAULT 0,
    locked_at    TEXT    NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (guild_id, channel_id)
);

-- ===========================================================================
-- Automod / anti-abuse
-- ===========================================================================
CREATE TABLE IF NOT EXISTS automod_config (
    guild_id            INTEGER PRIMARY KEY,
    antispam            INTEGER NOT NULL DEFAULT 1,
    antispam_msgs       INTEGER NOT NULL DEFAULT 5,
    antispam_window     INTEGER NOT NULL DEFAULT 5,
    antiflood           INTEGER NOT NULL DEFAULT 1,
    antiflood_msgs      INTEGER NOT NULL DEFAULT 12,
    antiflood_window    INTEGER NOT NULL DEFAULT 10,
    antiraid            INTEGER NOT NULL DEFAULT 1,
    antiraid_joins      INTEGER NOT NULL DEFAULT 8,
    antiraid_window     INTEGER NOT NULL DEFAULT 20,
    antiraid_action     TEXT    NOT NULL DEFAULT 'lockdown',
    antinuke            INTEGER NOT NULL DEFAULT 1,
    antinuke_threshold  INTEGER NOT NULL DEFAULT 4,
    antinuke_window     INTEGER NOT NULL DEFAULT 30,
    antiphish           INTEGER NOT NULL DEFAULT 1,
    antiinvite          INTEGER NOT NULL DEFAULT 1,
    antimention         INTEGER NOT NULL DEFAULT 1,
    mention_limit       INTEGER NOT NULL DEFAULT 6,
    badwords_enabled    INTEGER NOT NULL DEFAULT 1,
    duplicate_enabled   INTEGER NOT NULL DEFAULT 1,
    duplicate_limit     INTEGER NOT NULL DEFAULT 3,
    caps_enabled        INTEGER NOT NULL DEFAULT 0,
    caps_percent        INTEGER NOT NULL DEFAULT 75,
    suspicious_accounts INTEGER NOT NULL DEFAULT 1,
    suspicious_min_age  INTEGER NOT NULL DEFAULT 86400,
    suspicious_action   TEXT    NOT NULL DEFAULT 'alert',
    alert_channel       INTEGER,
    delete_violations   INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE IF NOT EXISTS automod_rules (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    guild_id     INTEGER NOT NULL,
    name         TEXT    NOT NULL,
    enabled      INTEGER NOT NULL DEFAULT 1,
    trigger_kind TEXT    NOT NULL,
    pattern      TEXT    NOT NULL,
    action       TEXT    NOT NULL DEFAULT 'delete',
    duration     INTEGER,
    threshold    INTEGER NOT NULL DEFAULT 1,
    window_secs  INTEGER NOT NULL DEFAULT 60,
    created_by   INTEGER NOT NULL DEFAULT 0,
    created_at   TEXT    NOT NULL DEFAULT (datetime('now'))
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_rules_name ON automod_rules(guild_id, name);

CREATE TABLE IF NOT EXISTS badwords (
    guild_id INTEGER NOT NULL,
    word     TEXT    NOT NULL,
    PRIMARY KEY (guild_id, word)
);

CREATE TABLE IF NOT EXISTS automod_exempt (
    guild_id INTEGER NOT NULL,
    kind     TEXT    NOT NULL,
    target   INTEGER NOT NULL,
    PRIMARY KEY (guild_id, kind, target)
);

CREATE TABLE IF NOT EXISTS link_allowlist (
    guild_id INTEGER NOT NULL,
    domain   TEXT    NOT NULL,
    PRIMARY KEY (guild_id, domain)
);

CREATE TABLE IF NOT EXISTS antinuke_log (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    guild_id   INTEGER NOT NULL,
    actor_id   INTEGER NOT NULL,
    action     TEXT    NOT NULL,
    target     TEXT    NOT NULL DEFAULT '',
    created_at TEXT    NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_nuke_actor ON antinuke_log(guild_id, actor_id, created_at);

-- ===========================================================================
-- Message logging (edit/delete logs, snipe, duplicate detection)
-- ===========================================================================
CREATE TABLE IF NOT EXISTS message_log (
    message_id  INTEGER PRIMARY KEY,
    guild_id    INTEGER NOT NULL,
    channel_id  INTEGER NOT NULL,
    author_id   INTEGER NOT NULL,
    author_tag  TEXT    NOT NULL DEFAULT '',
    content     TEXT    NOT NULL DEFAULT '',
    attachments TEXT    NOT NULL DEFAULT '',
    created_at  TEXT    NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_msglog_chan ON message_log(channel_id, created_at);

-- ===========================================================================
-- Welcome / member management
-- ===========================================================================
CREATE TABLE IF NOT EXISTS welcome_config (
    guild_id          INTEGER PRIMARY KEY,
    welcome_channel   INTEGER,
    welcome_message   TEXT    NOT NULL DEFAULT 'Welcome {mention} to **{server}**! You are member #{count}.',
    welcome_embed     INTEGER NOT NULL DEFAULT 1,
    welcome_image     TEXT,
    welcome_dm        TEXT,
    goodbye_channel   INTEGER,
    goodbye_message   TEXT    NOT NULL DEFAULT '{tag} has left the server.',
    autoroles         TEXT    NOT NULL DEFAULT '',
    bot_autoroles     TEXT    NOT NULL DEFAULT '',
    rules_channel     INTEGER,
    rules_message     INTEGER,
    rules_role        INTEGER,
    verify_channel    INTEGER,
    verify_role       INTEGER,
    verify_mode       TEXT    NOT NULL DEFAULT 'button',
    verify_min_age    INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS role_menus (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    guild_id    INTEGER NOT NULL,
    channel_id  INTEGER NOT NULL,
    message_id  INTEGER,
    name        TEXT    NOT NULL,
    title       TEXT    NOT NULL DEFAULT 'Self roles',
    description TEXT    NOT NULL DEFAULT '',
    style       TEXT    NOT NULL DEFAULT 'button',
    min_choices INTEGER NOT NULL DEFAULT 0,
    max_choices INTEGER NOT NULL DEFAULT 25,
    roles       TEXT    NOT NULL DEFAULT '[]'
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_menus_name ON role_menus(guild_id, name);

CREATE TABLE IF NOT EXISTS members (
    guild_id    INTEGER NOT NULL,
    user_id     INTEGER NOT NULL,
    first_join  TEXT    NOT NULL DEFAULT (datetime('now')),
    last_join   TEXT    NOT NULL DEFAULT (datetime('now')),
    last_leave  TEXT,
    join_count  INTEGER NOT NULL DEFAULT 1,
    last_seen   TEXT,
    verified    INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (guild_id, user_id)
);

CREATE TABLE IF NOT EXISTS profiles (
    guild_id   INTEGER NOT NULL,
    user_id    INTEGER NOT NULL,
    bio        TEXT    NOT NULL DEFAULT '',
    color      TEXT    NOT NULL DEFAULT '#5865F2',
    banner_url TEXT,
    badge      TEXT    NOT NULL DEFAULT '',
    timezone   TEXT,
    pronouns   TEXT    NOT NULL DEFAULT '',
    favourite  TEXT    NOT NULL DEFAULT '',
    PRIMARY KEY (guild_id, user_id)
);

CREATE TABLE IF NOT EXISTS afk (
    guild_id INTEGER NOT NULL,
    user_id  INTEGER NOT NULL,
    reason   TEXT    NOT NULL DEFAULT 'AFK',
    since    TEXT    NOT NULL DEFAULT (datetime('now')),
    pings    INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (guild_id, user_id)
);

-- ===========================================================================
-- Leveling / XP
-- ===========================================================================
CREATE TABLE IF NOT EXISTS leveling_config (
    guild_id         INTEGER PRIMARY KEY,
    enabled          INTEGER NOT NULL DEFAULT 1,
    xp_min           INTEGER NOT NULL DEFAULT 15,
    xp_max           INTEGER NOT NULL DEFAULT 25,
    cooldown         INTEGER NOT NULL DEFAULT 60,
    voice_xp_per_min INTEGER NOT NULL DEFAULT 5,
    voice_enabled    INTEGER NOT NULL DEFAULT 1,
    levelup_channel  INTEGER,
    levelup_message  TEXT    NOT NULL DEFAULT 'GG {mention}, you reached **level {level}**!',
    levelup_dm       INTEGER NOT NULL DEFAULT 0,
    announce         INTEGER NOT NULL DEFAULT 1,
    stack_rewards    INTEGER NOT NULL DEFAULT 1,
    global_rate      REAL    NOT NULL DEFAULT 1.0,
    prestige_level   INTEGER NOT NULL DEFAULT 100,
    no_xp_channels   TEXT    NOT NULL DEFAULT '',
    no_xp_roles      TEXT    NOT NULL DEFAULT ''
);

CREATE TABLE IF NOT EXISTS levels (
    guild_id    INTEGER NOT NULL,
    user_id     INTEGER NOT NULL,
    xp          INTEGER NOT NULL DEFAULT 0,
    total_xp    INTEGER NOT NULL DEFAULT 0,
    level       INTEGER NOT NULL DEFAULT 0,
    prestige    INTEGER NOT NULL DEFAULT 0,
    messages    INTEGER NOT NULL DEFAULT 0,
    voice_secs  INTEGER NOT NULL DEFAULT 0,
    last_xp     TEXT,
    streak      INTEGER NOT NULL DEFAULT 0,
    best_streak INTEGER NOT NULL DEFAULT 0,
    last_active TEXT,
    PRIMARY KEY (guild_id, user_id)
);
CREATE INDEX IF NOT EXISTS idx_levels_board ON levels(guild_id, prestige DESC, total_xp DESC);

CREATE TABLE IF NOT EXISTS level_rewards (
    guild_id INTEGER NOT NULL,
    level    INTEGER NOT NULL,
    role_id  INTEGER NOT NULL,
    PRIMARY KEY (guild_id, level, role_id)
);

CREATE TABLE IF NOT EXISTS xp_multipliers (
    guild_id   INTEGER NOT NULL,
    kind       TEXT    NOT NULL,
    target_id  INTEGER NOT NULL,
    multiplier REAL    NOT NULL DEFAULT 1.0,
    PRIMARY KEY (guild_id, kind, target_id)
);

CREATE TABLE IF NOT EXISTS achievements (
    guild_id  INTEGER NOT NULL,
    user_id   INTEGER NOT NULL,
    key       TEXT    NOT NULL,
    earned_at TEXT    NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (guild_id, user_id, key)
);

-- ===========================================================================
-- Community engagement
-- ===========================================================================
CREATE TABLE IF NOT EXISTS polls (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    guild_id   INTEGER NOT NULL,
    channel_id INTEGER NOT NULL,
    message_id INTEGER,
    author_id  INTEGER NOT NULL,
    question   TEXT    NOT NULL,
    options    TEXT    NOT NULL DEFAULT '[]',
    anonymous  INTEGER NOT NULL DEFAULT 0,
    multi      INTEGER NOT NULL DEFAULT 0,
    ends_at    TEXT,
    closed     INTEGER NOT NULL DEFAULT 0,
    created_at TEXT    NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS poll_votes (
    poll_id INTEGER NOT NULL,
    user_id INTEGER NOT NULL,
    choice  INTEGER NOT NULL,
    PRIMARY KEY (poll_id, user_id, choice)
);


CREATE TABLE IF NOT EXISTS giveaways (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    guild_id      INTEGER NOT NULL,
    channel_id    INTEGER NOT NULL,
    message_id    INTEGER,
    host_id       INTEGER NOT NULL,
    prize         TEXT    NOT NULL,
    winners       INTEGER NOT NULL DEFAULT 1,
    ends_at       TEXT    NOT NULL,
    ended         INTEGER NOT NULL DEFAULT 0,
    required_role INTEGER,
    bonus_role    INTEGER,
    bonus_entries INTEGER NOT NULL DEFAULT 0,
    min_level     INTEGER NOT NULL DEFAULT 0,
    last_winners  TEXT    NOT NULL DEFAULT ''
);

CREATE TABLE IF NOT EXISTS giveaway_entries (
    giveaway_id INTEGER NOT NULL,
    user_id     INTEGER NOT NULL,
    entries     INTEGER NOT NULL DEFAULT 1,
    PRIMARY KEY (giveaway_id, user_id)
);

CREATE TABLE IF NOT EXISTS events (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    guild_id      INTEGER NOT NULL,
    name          TEXT    NOT NULL,
    description   TEXT    NOT NULL DEFAULT '',
    location      TEXT    NOT NULL DEFAULT 'Discord',
    host_id       INTEGER NOT NULL,
    channel_id    INTEGER NOT NULL,
    message_id    INTEGER,
    starts_at     TEXT    NOT NULL,
    max_attendees INTEGER NOT NULL DEFAULT 0,
    reminder_sent INTEGER NOT NULL DEFAULT 0,
    started       INTEGER NOT NULL DEFAULT 0,
    created_at    TEXT    NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS event_signups (
    event_id INTEGER NOT NULL,
    user_id  INTEGER NOT NULL,
    status   TEXT    NOT NULL DEFAULT 'going',
    PRIMARY KEY (event_id, user_id)
);

CREATE TABLE IF NOT EXISTS quotes (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    guild_id   INTEGER NOT NULL,
    number     INTEGER NOT NULL,
    author_id  INTEGER NOT NULL,
    content    TEXT    NOT NULL,
    added_by   INTEGER NOT NULL,
    link       TEXT,
    created_at TEXT    NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS anon_feedback (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    guild_id   INTEGER NOT NULL,
    content    TEXT    NOT NULL,
    created_at TEXT    NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS counting (
    guild_id      INTEGER NOT NULL,
    channel_id    INTEGER NOT NULL,
    current       INTEGER NOT NULL DEFAULT 0,
    last_user     INTEGER,
    best          INTEGER NOT NULL DEFAULT 0,
    allow_repeat  INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (guild_id, channel_id)
);

CREATE TABLE IF NOT EXISTS wordchain (
    guild_id   INTEGER NOT NULL,
    channel_id INTEGER NOT NULL,
    last_word  TEXT    NOT NULL DEFAULT '',
    last_user  INTEGER,
    length     INTEGER NOT NULL DEFAULT 0,
    best       INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (guild_id, channel_id)
);

CREATE TABLE IF NOT EXISTS wordchain_used (
    guild_id   INTEGER NOT NULL,
    channel_id INTEGER NOT NULL,
    word       TEXT    NOT NULL,
    PRIMARY KEY (guild_id, channel_id, word)
);

CREATE TABLE IF NOT EXISTS trivia_questions (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    guild_id   INTEGER NOT NULL DEFAULT 0,
    question   TEXT    NOT NULL,
    answer     TEXT    NOT NULL,
    choices    TEXT    NOT NULL DEFAULT '[]',
    category   TEXT    NOT NULL DEFAULT 'tech',
    difficulty TEXT    NOT NULL DEFAULT 'medium',
    added_by   INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS trivia_scores (
    guild_id    INTEGER NOT NULL,
    user_id     INTEGER NOT NULL,
    correct     INTEGER NOT NULL DEFAULT 0,
    wrong       INTEGER NOT NULL DEFAULT 0,
    streak      INTEGER NOT NULL DEFAULT 0,
    best_streak INTEGER NOT NULL DEFAULT 0,
    points      INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (guild_id, user_id)
);

CREATE TABLE IF NOT EXISTS daily_questions (
    id       INTEGER PRIMARY KEY AUTOINCREMENT,
    guild_id INTEGER NOT NULL DEFAULT 0,
    question TEXT    NOT NULL,
    used_at  TEXT
);

CREATE TABLE IF NOT EXISTS challenges (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    guild_id    INTEGER NOT NULL,
    name        TEXT    NOT NULL,
    description TEXT    NOT NULL DEFAULT '',
    goal_kind   TEXT    NOT NULL DEFAULT 'messages',
    goal_value  INTEGER NOT NULL DEFAULT 100,
    reward_role INTEGER,
    reward_xp   INTEGER NOT NULL DEFAULT 0,
    starts_at   TEXT    NOT NULL DEFAULT (datetime('now')),
    ends_at     TEXT    NOT NULL,
    announced   INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS challenge_progress (
    challenge_id INTEGER NOT NULL,
    user_id      INTEGER NOT NULL,
    value        INTEGER NOT NULL DEFAULT 0,
    completed    INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (challenge_id, user_id)
);

CREATE TABLE IF NOT EXISTS milestones (
    guild_id     INTEGER NOT NULL,
    kind         TEXT    NOT NULL,
    value        INTEGER NOT NULL,
    announced_at TEXT    NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (guild_id, kind, value)
);

-- ===========================================================================
-- Voice
-- ===========================================================================
CREATE TABLE IF NOT EXISTS voice_config (
    guild_id       INTEGER PRIMARY KEY,
    j2c_channel    INTEGER,
    j2c_category   INTEGER,
    default_name   TEXT    NOT NULL DEFAULT '{user}''s room',
    default_limit  INTEGER NOT NULL DEFAULT 0,
    afk_channel    INTEGER,
    afk_timeout    INTEGER NOT NULL DEFAULT 900,
    delete_empty   INTEGER NOT NULL DEFAULT 1,
    empty_grace    INTEGER NOT NULL DEFAULT 10
);

CREATE TABLE IF NOT EXISTS temp_voice (
    channel_id INTEGER PRIMARY KEY,
    guild_id   INTEGER NOT NULL,
    owner_id   INTEGER NOT NULL,
    name       TEXT    NOT NULL DEFAULT '',
    user_limit INTEGER NOT NULL DEFAULT 0,
    locked     INTEGER NOT NULL DEFAULT 0,
    private    INTEGER NOT NULL DEFAULT 0,
    created_at TEXT    NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS voice_sessions (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    guild_id   INTEGER NOT NULL,
    user_id    INTEGER NOT NULL,
    channel_id INTEGER NOT NULL,
    joined_at  TEXT    NOT NULL DEFAULT (datetime('now')),
    left_at    TEXT,
    duration   INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_vs_user ON voice_sessions(guild_id, user_id);
CREATE INDEX IF NOT EXISTS idx_vs_open ON voice_sessions(guild_id, user_id, left_at);

-- ===========================================================================
-- Bot metadata
-- ===========================================================================
CREATE TABLE IF NOT EXISTS bot_meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

-- ===========================================================================
-- Reminders
-- ===========================================================================
CREATE TABLE IF NOT EXISTS reminders (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    guild_id   INTEGER NOT NULL DEFAULT 0,
    channel_id INTEGER NOT NULL,
    user_id    INTEGER NOT NULL,
    content    TEXT    NOT NULL,
    remind_at  TEXT    NOT NULL,
    via_dm     INTEGER NOT NULL DEFAULT 0,
    repeat_secs INTEGER NOT NULL DEFAULT 0,
    created_at TEXT    NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_rem_at ON reminders(remind_at);


