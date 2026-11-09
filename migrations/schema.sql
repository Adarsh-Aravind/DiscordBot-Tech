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
    birthday_channel  INTEGER,
    birthday_role     INTEGER,
    appeal_channel    INTEGER,
    report_channel    INTEGER,
    timezone          TEXT    NOT NULL DEFAULT 'UTC',
    locked_down       INTEGER NOT NULL DEFAULT 0,
    created_at        TEXT    NOT NULL DEFAULT (datetime('now'))
);

-- Free-form settings for the long tail of toggles.
CREATE TABLE IF NOT EXISTS kv_settings (
    guild_id INTEGER NOT NULL,
    key      TEXT    NOT NULL,
    value    TEXT    NOT NULL,
    PRIMARY KEY (guild_id, key)
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
    inviter_id  INTEGER,
    invite_code TEXT,
    last_seen   TEXT,
    verified    INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (guild_id, user_id)
);

CREATE TABLE IF NOT EXISTS invite_cache (
    guild_id INTEGER NOT NULL,
    code     TEXT    NOT NULL,
    inviter  INTEGER NOT NULL DEFAULT 0,
    uses     INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (guild_id, code)
);

CREATE TABLE IF NOT EXISTS invite_stats (
    guild_id INTEGER NOT NULL,
    user_id  INTEGER NOT NULL,
    total    INTEGER NOT NULL DEFAULT 0,
    departed INTEGER NOT NULL DEFAULT 0,
    fake     INTEGER NOT NULL DEFAULT 0,
    bonus    INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (guild_id, user_id)
);

CREATE TABLE IF NOT EXISTS profiles (
    guild_id   INTEGER NOT NULL,
    user_id    INTEGER NOT NULL,
    bio        TEXT    NOT NULL DEFAULT '',
    color      TEXT    NOT NULL DEFAULT '#5865F2',
    banner_url TEXT,
    badge      TEXT    NOT NULL DEFAULT '',
    birthday   TEXT,
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
-- Tickets
-- ===========================================================================
CREATE TABLE IF NOT EXISTS ticket_config (
    guild_id           INTEGER PRIMARY KEY,
    category_id        INTEGER,
    log_channel        INTEGER,
    transcript_channel INTEGER,
    support_roles      TEXT    NOT NULL DEFAULT '',
    max_open           INTEGER NOT NULL DEFAULT 2,
    auto_close_hours   INTEGER NOT NULL DEFAULT 72,
    ask_feedback       INTEGER NOT NULL DEFAULT 1,
    next_number        INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE IF NOT EXISTS ticket_categories (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    guild_id      INTEGER NOT NULL,
    key           TEXT    NOT NULL,
    label         TEXT    NOT NULL,
    emoji         TEXT    NOT NULL DEFAULT '',
    description   TEXT    NOT NULL DEFAULT '',
    template      TEXT    NOT NULL DEFAULT '',
    support_roles TEXT    NOT NULL DEFAULT '',
    category_id   INTEGER
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_tcat_key ON ticket_categories(guild_id, key);

CREATE TABLE IF NOT EXISTS tickets (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    guild_id       INTEGER NOT NULL,
    number         INTEGER NOT NULL,
    channel_id     INTEGER NOT NULL,
    user_id        INTEGER NOT NULL,
    category       TEXT    NOT NULL DEFAULT 'general',
    subject        TEXT    NOT NULL DEFAULT '',
    priority       TEXT    NOT NULL DEFAULT 'normal',
    status         TEXT    NOT NULL DEFAULT 'open',
    claimed_by     INTEGER,
    assigned_to    INTEGER,
    created_at     TEXT    NOT NULL DEFAULT (datetime('now')),
    first_reply_at TEXT,
    first_reply_by INTEGER,
    last_activity  TEXT    NOT NULL DEFAULT (datetime('now')),
    closed_at      TEXT,
    closed_by      INTEGER,
    close_reason   TEXT,
    rating         INTEGER,
    feedback       TEXT
);
CREATE INDEX IF NOT EXISTS idx_tickets_guild ON tickets(guild_id, status);
CREATE UNIQUE INDEX IF NOT EXISTS idx_tickets_channel ON tickets(channel_id);

CREATE TABLE IF NOT EXISTS ticket_members (
    ticket_id INTEGER NOT NULL,
    user_id   INTEGER NOT NULL,
    PRIMARY KEY (ticket_id, user_id)
);

CREATE TABLE IF NOT EXISTS ticket_messages (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    ticket_id  INTEGER NOT NULL,
    author_id  INTEGER NOT NULL,
    author_tag TEXT    NOT NULL DEFAULT '',
    is_staff   INTEGER NOT NULL DEFAULT 0,
    content    TEXT    NOT NULL DEFAULT '',
    created_at TEXT    NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_tmsg_ticket ON ticket_messages(ticket_id, id);

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

CREATE TABLE IF NOT EXISTS suggestions (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    guild_id   INTEGER NOT NULL,
    number     INTEGER NOT NULL,
    user_id    INTEGER NOT NULL,
    content    TEXT    NOT NULL,
    channel_id INTEGER NOT NULL,
    message_id INTEGER,
    status     TEXT    NOT NULL DEFAULT 'pending',
    staff_note TEXT,
    reviewer   INTEGER,
    created_at TEXT    NOT NULL DEFAULT (datetime('now'))
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_sugg_no ON suggestions(guild_id, number);

CREATE TABLE IF NOT EXISTS suggestion_votes (
    suggestion_id INTEGER NOT NULL,
    user_id       INTEGER NOT NULL,
    vote          INTEGER NOT NULL,
    PRIMARY KEY (suggestion_id, user_id)
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

CREATE TABLE IF NOT EXISTS starboard_config (
    guild_id     INTEGER PRIMARY KEY,
    channel_id   INTEGER,
    threshold    INTEGER NOT NULL DEFAULT 4,
    emoji        TEXT    NOT NULL DEFAULT '⭐',
    self_star    INTEGER NOT NULL DEFAULT 0,
    ignore_bots  INTEGER NOT NULL DEFAULT 1,
    nsfw_allowed INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS starboard_entries (
    guild_id       INTEGER NOT NULL,
    message_id     INTEGER NOT NULL,
    star_message   INTEGER NOT NULL,
    stars          INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (guild_id, message_id)
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

CREATE TABLE IF NOT EXISTS confessions (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    guild_id   INTEGER NOT NULL,
    number     INTEGER NOT NULL,
    user_id    INTEGER NOT NULL,
    content    TEXT    NOT NULL,
    message_id INTEGER,
    banned     INTEGER NOT NULL DEFAULT 0,
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
-- Analytics
-- ===========================================================================
CREATE TABLE IF NOT EXISTS stats_daily (
    guild_id   INTEGER NOT NULL,
    day        TEXT    NOT NULL,
    messages   INTEGER NOT NULL DEFAULT 0,
    joins      INTEGER NOT NULL DEFAULT 0,
    leaves     INTEGER NOT NULL DEFAULT 0,
    voice_secs INTEGER NOT NULL DEFAULT 0,
    commands   INTEGER NOT NULL DEFAULT 0,
    mod_actions INTEGER NOT NULL DEFAULT 0,
    member_count INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (guild_id, day)
);

CREATE TABLE IF NOT EXISTS member_activity (
    guild_id   INTEGER NOT NULL,
    user_id    INTEGER NOT NULL,
    day        TEXT    NOT NULL,
    messages   INTEGER NOT NULL DEFAULT 0,
    voice_secs INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (guild_id, user_id, day)
);
CREATE INDEX IF NOT EXISTS idx_ma_day ON member_activity(guild_id, day);

CREATE TABLE IF NOT EXISTS channel_activity (
    guild_id   INTEGER NOT NULL,
    channel_id INTEGER NOT NULL,
    day        TEXT    NOT NULL,
    messages   INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (guild_id, channel_id, day)
);

CREATE TABLE IF NOT EXISTS hourly_activity (
    guild_id INTEGER NOT NULL,
    hour     INTEGER NOT NULL,
    messages INTEGER NOT NULL DEFAULT 0,
    voice    INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (guild_id, hour)
);

CREATE TABLE IF NOT EXISTS command_usage (
    guild_id INTEGER NOT NULL,
    command  TEXT    NOT NULL,
    uses     INTEGER NOT NULL DEFAULT 0,
    errors   INTEGER NOT NULL DEFAULT 0,
    last_use TEXT,
    PRIMARY KEY (guild_id, command)
);

CREATE TABLE IF NOT EXISTS bot_meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

-- ===========================================================================
-- Automation / workflows
-- ===========================================================================
CREATE TABLE IF NOT EXISTS scheduled_messages (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    guild_id    INTEGER NOT NULL,
    channel_id  INTEGER NOT NULL,
    content     TEXT    NOT NULL,
    as_embed    INTEGER NOT NULL DEFAULT 0,
    title       TEXT,
    next_run    TEXT    NOT NULL,
    interval_secs INTEGER NOT NULL DEFAULT 0,
    enabled     INTEGER NOT NULL DEFAULT 1,
    created_by  INTEGER NOT NULL,
    runs        INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_sched_next ON scheduled_messages(enabled, next_run);

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

CREATE TABLE IF NOT EXISTS workflows (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    guild_id    INTEGER NOT NULL,
    name        TEXT    NOT NULL,
    trigger_kind TEXT   NOT NULL,
    trigger_arg TEXT    NOT NULL DEFAULT '',
    conditions  TEXT    NOT NULL DEFAULT '[]',
    actions     TEXT    NOT NULL DEFAULT '[]',
    delay_secs  INTEGER NOT NULL DEFAULT 0,
    enabled     INTEGER NOT NULL DEFAULT 1,
    runs        INTEGER NOT NULL DEFAULT 0,
    created_by  INTEGER NOT NULL DEFAULT 0,
    created_at  TEXT    NOT NULL DEFAULT (datetime('now'))
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_wf_name ON workflows(guild_id, name);

CREATE TABLE IF NOT EXISTS workflow_queue (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    workflow_id INTEGER NOT NULL,
    guild_id    INTEGER NOT NULL,
    user_id     INTEGER NOT NULL DEFAULT 0,
    channel_id  INTEGER NOT NULL DEFAULT 0,
    context     TEXT    NOT NULL DEFAULT '{}',
    run_at      TEXT    NOT NULL
);

CREATE TABLE IF NOT EXISTS custom_commands (
    guild_id      INTEGER NOT NULL,
    name          TEXT    NOT NULL,
    response      TEXT    NOT NULL,
    as_embed      INTEGER NOT NULL DEFAULT 0,
    required_role INTEGER,
    uses          INTEGER NOT NULL DEFAULT 0,
    created_by    INTEGER NOT NULL,
    created_at    TEXT    NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (guild_id, name)
);

CREATE TABLE IF NOT EXISTS webhook_targets (
    id       INTEGER PRIMARY KEY AUTOINCREMENT,
    guild_id INTEGER NOT NULL,
    name     TEXT    NOT NULL,
    url      TEXT    NOT NULL,
    events   TEXT    NOT NULL DEFAULT '',
    enabled  INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE IF NOT EXISTS weekly_reports (
    guild_id   INTEGER PRIMARY KEY,
    channel_id INTEGER NOT NULL,
    weekday    INTEGER NOT NULL DEFAULT 1,
    hour       INTEGER NOT NULL DEFAULT 12,
    enabled    INTEGER NOT NULL DEFAULT 1,
    last_sent  TEXT
);
