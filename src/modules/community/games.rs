//! Games: trivia, quizzes, the counting channel, word chain and daily questions.

use poise::serenity_prelude as serenity;
use serenity::{
    ChannelId, ComponentInteraction, CreateActionRow, CreateButton, CreateInteractionResponse,
    CreateInteractionResponseMessage, CreateMessage, GuildChannel, Message, ReactionType,
};

use crate::db;
use crate::state::{Ctx, Data, Error};
use crate::util::{embed, perms, text};

const LETTERS: [&str; 4] = ["🇦", "🇧", "🇨", "🇩"];

// ===========================================================================
// Trivia
// ===========================================================================

/// Tech trivia. Answer with the buttons.
#[poise::command(
    slash_command,
    guild_only,
    category = "Games",
    subcommands("trivia_play", "trivia_board", "trivia_add", "trivia_stats")
)]
pub async fn trivia(_ctx: Ctx<'_>) -> Result<(), Error> {
    Ok(())
}

#[derive(Debug, poise::ChoiceParameter)]
pub enum Difficulty {
    #[name = "easy"]
    Easy,
    #[name = "medium"]
    Medium,
    #[name = "hard"]
    Hard,
}

/// Ask a trivia question.
#[poise::command(slash_command, rename = "play", guild_only)]
pub async fn trivia_play(
    ctx: Ctx<'_>,
    #[description = "Difficulty"] difficulty: Option<Difficulty>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;

    // Questions seeded globally (guild_id 0) plus any the server added.
    let row: Option<(i64, String, String, String)> = match &difficulty {
        Some(difficulty) => {
            let key = match difficulty {
                Difficulty::Easy => "easy",
                Difficulty::Medium => "medium",
                Difficulty::Hard => "hard",
            };
            sqlx::query_as(
                "SELECT id, question, answer, choices FROM trivia_questions
                 WHERE (guild_id = 0 OR guild_id = ?) AND difficulty = ?
                 ORDER BY RANDOM() LIMIT 1",
            )
            .bind(guild_id)
            .bind(key)
            .fetch_optional(&ctx.data().db)
            .await?
        }
        None => {
            sqlx::query_as(
                "SELECT id, question, answer, choices FROM trivia_questions
                 WHERE guild_id = 0 OR guild_id = ?
                 ORDER BY RANDOM() LIMIT 1",
            )
            .bind(guild_id)
            .fetch_optional(&ctx.data().db)
            .await?
        }
    };

    let Some((question_id, question, answer, raw_choices)) = row else {
        ctx.send(
            poise::CreateReply::default()
                .embed(embed::err("No questions available at that difficulty.")),
        )
        .await?;
        return Ok(());
    };

    let mut choices: Vec<String> = serde_json::from_str(&raw_choices).unwrap_or_default();
    if choices.len() < 2 {
        choices = vec![answer.clone(), "None of these".to_string()];
    }
    {
        use rand::seq::SliceRandom;
        let mut rng = rand::thread_rng();
        choices.shuffle(&mut rng);
    }
    let correct_index = choices
        .iter()
        .position(|c| *c == answer)
        .unwrap_or(0);

    // The nonce keeps the correct answer out of the button ids.
    let nonce: u64 = {
        use rand::Rng;
        rand::thread_rng().gen()
    };
    db::kv_set(
        &ctx.data().db,
        guild_id,
        &format!("trivia.{nonce}"),
        &format!("{question_id}|{correct_index}|{}", choices.join("\u{1f}")),
    )
    .await?;

    let body = choices
        .iter()
        .enumerate()
        .map(|(i, choice)| format!("{} {choice}", LETTERS.get(i).copied().unwrap_or("▫️")))
        .collect::<Vec<_>>()
        .join("\n");

    let buttons: Vec<CreateButton> = choices
        .iter()
        .enumerate()
        .take(4)
        .map(|(i, _)| {
            CreateButton::new(format!("trivia:{nonce}:{i}"))
                .label(LETTERS.get(i).copied().unwrap_or("?"))
                .style(serenity::ButtonStyle::Secondary)
        })
        .collect();

    ctx.send(
        poise::CreateReply::default()
            .embed(
                embed::titled("🧠 Tech trivia", embed::BRAND)
                    .description(format!("**{question}**\n\n{body}"))
                    .footer(embed::footer(
                        "First answer per person counts. Correct answers build your streak.",
                    )),
            )
            .components(vec![CreateActionRow::Buttons(buttons)]),
    )
    .await?;
    Ok(())
}

/// Trivia leaderboard.
#[poise::command(slash_command, rename = "leaderboard", guild_only)]
pub async fn trivia_board(ctx: Ctx<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let rows: Vec<(i64, i64, i64, i64)> = sqlx::query_as(
        "SELECT user_id, correct, wrong, best_streak FROM trivia_scores
         WHERE guild_id = ? ORDER BY correct DESC LIMIT 15",
    )
    .bind(guild_id)
    .fetch_all(&ctx.data().db)
    .await?;

    let body = if rows.is_empty() {
        "Nobody has played yet. Try `/trivia play`.".to_string()
    } else {
        rows.iter()
            .enumerate()
            .map(|(i, (user_id, correct, wrong, best))| {
                let total = correct + wrong;
                let accuracy = if total > 0 {
                    (*correct as f64 / total as f64 * 100.0).round() as i64
                } else {
                    0
                };
                let medal = match i {
                    0 => "🥇",
                    1 => "🥈",
                    2 => "🥉",
                    _ => "▫️",
                };
                format!(
                    "{medal} **{}.** <@{user_id}> — **{correct}** correct · {accuracy}% · best streak {best}",
                    i + 1
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    ctx.send(
        poise::CreateReply::default()
            .embed(embed::titled("🧠 Trivia leaderboard", embed::BRAND).description(body)),
    )
    .await?;
    Ok(())
}

/// Your trivia record.
#[poise::command(slash_command, rename = "stats", guild_only)]
pub async fn trivia_stats(
    ctx: Ctx<'_>,
    #[description = "Whose stats, defaults to you"] user: Option<serenity::User>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let target = user.unwrap_or_else(|| ctx.author().clone());

    let row: Option<(i64, i64, i64, i64, i64)> = sqlx::query_as(
        "SELECT correct, wrong, streak, best_streak, points FROM trivia_scores
         WHERE guild_id = ? AND user_id = ?",
    )
    .bind(guild_id)
    .bind(target.id.get() as i64)
    .fetch_optional(&ctx.data().db)
    .await?;

    let (correct, wrong, streak, best, points) = row.unwrap_or((0, 0, 0, 0, 0));
    let total = correct + wrong;
    let accuracy = if total > 0 {
        format!(
            "{}%",
            (correct as f64 / total as f64 * 100.0).round() as i64
        )
    } else {
        "n/a".to_string()
    };

    ctx.send(
        poise::CreateReply::default().embed(
            embed::titled(format!("Trivia · {}", target.tag()), embed::BRAND)
                .field("Correct", correct.to_string(), true)
                .field("Wrong", wrong.to_string(), true)
                .field("Accuracy", accuracy, true)
                .field("Current streak", streak.to_string(), true)
                .field("Best streak", best.to_string(), true)
                .field("Points", points.to_string(), true)
                .thumbnail(target.face()),
        ),
    )
    .await?;
    Ok(())
}

/// Add your own trivia question.
#[poise::command(slash_command, rename = "add", guild_only, check = "perms::staff_only")]
pub async fn trivia_add(
    ctx: Ctx<'_>,
    #[description = "The question"] question: String,
    #[description = "The correct answer"] answer: String,
    #[description = "Three wrong answers, separated by |"] wrong_answers: String,
    #[description = "Difficulty"] difficulty: Option<Difficulty>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;

    let mut choices = vec![answer.clone()];
    choices.extend(
        wrong_answers
            .split('|')
            .map(|w| w.trim().to_string())
            .filter(|w| !w.is_empty())
            .take(3),
    );

    if choices.len() < 2 {
        ctx.send(poise::CreateReply::default().embed(embed::err(
            "Give me at least one wrong answer, separated by `|`.",
        )))
        .await?;
        return Ok(());
    }

    let key = match difficulty.unwrap_or(Difficulty::Medium) {
        Difficulty::Easy => "easy",
        Difficulty::Medium => "medium",
        Difficulty::Hard => "hard",
    };

    sqlx::query(
        "INSERT INTO trivia_questions
            (guild_id, question, answer, choices, category, difficulty, added_by)
         VALUES (?, ?, ?, ?, 'custom', ?, ?)",
    )
    .bind(guild_id)
    .bind(&question)
    .bind(&answer)
    .bind(serde_json::to_string(&choices)?)
    .bind(key)
    .bind(ctx.author().id.get() as i64)
    .execute(&ctx.data().db)
    .await?;

    ctx.send(
        poise::CreateReply::default()
            .embed(embed::ok("Question added to this server's trivia pool."))
            .ephemeral(true),
    )
    .await?;
    Ok(())
}

/// Handle a trivia answer button.
pub async fn handle_component(
    sctx: &serenity::Context,
    data: &Data,
    interaction: &ComponentInteraction,
) -> Result<bool, Error> {
    let Some(rest) = interaction.data.custom_id.strip_prefix("trivia:") else {
        return Ok(false);
    };
    let Some(guild_id) = interaction.guild_id else {
        return Ok(true);
    };
    let guild_i64 = guild_id.get() as i64;

    let mut parts = rest.split(':');
    let nonce = parts.next().unwrap_or("");
    let chosen: usize = parts.next().and_then(|s| s.parse().ok()).unwrap_or(99);

    let Some(stored) = db::kv_get(&data.db, guild_i64, &format!("trivia.{nonce}")).await else {
        answer(sctx, interaction, "That question has expired.").await?;
        return Ok(true);
    };

    let mut fields = stored.split('|');
    let _question_id = fields.next().unwrap_or("0");
    let correct_index: usize = fields.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let choices: Vec<String> = fields
        .next()
        .map(|c| c.split('\u{1f}').map(|s| s.to_string()).collect())
        .unwrap_or_default();

    // One answer per person per question.
    let answered_key = format!("trivia.{nonce}.{}", interaction.user.id.get());
    if db::kv_get(&data.db, guild_i64, &answered_key).await.is_some() {
        answer(sctx, interaction, "You have already answered this one.").await?;
        return Ok(true);
    }
    db::kv_set(&data.db, guild_i64, &answered_key, "1").await?;

    let user_i64 = interaction.user.id.get() as i64;
    let was_right = chosen == correct_index;

    sqlx::query("INSERT OR IGNORE INTO trivia_scores (guild_id, user_id) VALUES (?, ?)")
        .bind(guild_i64)
        .bind(user_i64)
        .execute(&data.db)
        .await?;

    if was_right {
        sqlx::query(
            "UPDATE trivia_scores
             SET correct = correct + 1,
                 streak = streak + 1,
                 best_streak = MAX(best_streak, streak + 1),
                 points = points + 10
             WHERE guild_id = ? AND user_id = ?",
        )
        .bind(guild_i64)
        .bind(user_i64)
        .execute(&data.db)
        .await?;

        // Correct answers are worth a little XP too.
        let _ = crate::modules::leveling::add_xp(&data.db, guild_i64, user_i64, 15).await;
        let _ = crate::modules::leveling::check_achievements(&data.db, guild_i64, user_i64).await;
    } else {
        sqlx::query(
            "UPDATE trivia_scores SET wrong = wrong + 1, streak = 0
             WHERE guild_id = ? AND user_id = ?",
        )
        .bind(guild_i64)
        .bind(user_i64)
        .execute(&data.db)
        .await?;
    }

    let streak: i64 = sqlx::query_scalar(
        "SELECT streak FROM trivia_scores WHERE guild_id = ? AND user_id = ?",
    )
    .bind(guild_i64)
    .bind(user_i64)
    .fetch_optional(&data.db)
    .await?
    .unwrap_or(0);

    let correct_text = choices
        .get(correct_index)
        .cloned()
        .unwrap_or_else(|| "unknown".to_string());

    let body = if was_right {
        format!("✅ Correct! **{correct_text}**\nStreak: **{streak}** · +15 XP")
    } else {
        format!("❌ Not quite. The answer was **{correct_text}**.")
    };

    answer(sctx, interaction, &body).await?;
    Ok(true)
}

async fn answer(
    sctx: &serenity::Context,
    interaction: &ComponentInteraction,
    body: &str,
) -> Result<(), Error> {
    interaction
        .create_response(
            &sctx.http,
            CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content(body)
                    .ephemeral(true),
            ),
        )
        .await?;
    Ok(())
}

// ===========================================================================
// Counting
// ===========================================================================

/// Set up the counting channel.
#[poise::command(
    slash_command,
    guild_only,
    category = "Games",
    subcommands("counting_setup", "counting_status", "counting_off"),
    check = "perms::admin_only"
)]
pub async fn counting(_ctx: Ctx<'_>) -> Result<(), Error> {
    Ok(())
}

#[poise::command(slash_command, rename = "setup", guild_only)]
pub async fn counting_setup(
    ctx: Ctx<'_>,
    #[description = "Channel to count in"] channel: GuildChannel,
    #[description = "Allow the same person to count twice in a row"] allow_repeat: Option<bool>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    sqlx::query(
        "INSERT INTO counting (guild_id, channel_id, current, allow_repeat) VALUES (?, ?, 0, ?)
         ON CONFLICT(guild_id, channel_id) DO UPDATE SET allow_repeat = excluded.allow_repeat",
    )
    .bind(guild_id)
    .bind(channel.id.get() as i64)
    .bind(i64::from(allow_repeat.unwrap_or(false)))
    .execute(&ctx.data().db)
    .await?;

    ctx.send(poise::CreateReply::default().embed(embed::ok(format!(
        "Counting is live in <#{}>. Start at **1**.",
        channel.id.get()
    ))))
    .await?;
    Ok(())
}

#[poise::command(slash_command, rename = "status", guild_only)]
pub async fn counting_status(ctx: Ctx<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let rows: Vec<(i64, i64, i64)> =
        sqlx::query_as("SELECT channel_id, current, best FROM counting WHERE guild_id = ?")
            .bind(guild_id)
            .fetch_all(&ctx.data().db)
            .await?;

    let body = if rows.is_empty() {
        "Counting is not set up.".to_string()
    } else {
        rows.iter()
            .map(|(channel, current, best)| {
                format!("<#{channel}> — at **{current}**, record **{best}**")
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    ctx.send(
        poise::CreateReply::default()
            .embed(embed::titled("🔢 Counting", embed::INFO).description(body)),
    )
    .await?;
    Ok(())
}

#[poise::command(slash_command, rename = "off", guild_only)]
pub async fn counting_off(
    ctx: Ctx<'_>,
    #[description = "Channel to stop counting in"] channel: GuildChannel,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    sqlx::query("DELETE FROM counting WHERE guild_id = ? AND channel_id = ?")
        .bind(guild_id)
        .bind(channel.id.get() as i64)
        .execute(&ctx.data().db)
        .await?;
    ctx.send(poise::CreateReply::default().embed(embed::ok("Counting switched off there.")))
        .await?;
    Ok(())
}

/// Message handler for the counting channel. Returns true when handled.
pub async fn handle_counting(
    sctx: &serenity::Context,
    data: &Data,
    message: &Message,
) -> Result<bool, Error> {
    let Some(guild_id) = message.guild_id else {
        return Ok(false);
    };

    let row: Option<(i64, Option<i64>, i64, i64)> = sqlx::query_as(
        "SELECT current, last_user, best, allow_repeat FROM counting
         WHERE guild_id = ? AND channel_id = ?",
    )
    .bind(guild_id.get() as i64)
    .bind(message.channel_id.get() as i64)
    .fetch_optional(&data.db)
    .await?;

    let Some((current, last_user, best, allow_repeat)) = row else {
        return Ok(false);
    };

    let Some(number) = text::parse_leading_number(&message.content) else {
        // Non-numeric chatter is simply ignored, not punished.
        return Ok(false);
    };

    let author = message.author.id.get() as i64;
    let expected = current + 1;

    let repeated = allow_repeat == 0 && last_user == Some(author);

    if number == expected && !repeated {
        let new_best = best.max(number);
        sqlx::query(
            "UPDATE counting SET current = ?, last_user = ?, best = ?
             WHERE guild_id = ? AND channel_id = ?",
        )
        .bind(number)
        .bind(author)
        .bind(new_best)
        .bind(guild_id.get() as i64)
        .bind(message.channel_id.get() as i64)
        .execute(&data.db)
        .await?;

        let emoji = if number % 100 == 0 { '🎉' } else { '✅' };
        let _ = message
            .react(&sctx.http, ReactionType::Unicode(emoji.to_string()))
            .await;

        if number % 100 == 0 {
            let _ = message
                .channel_id
                .send_message(
                    &sctx.http,
                    CreateMessage::new().content(format!(
                        "🎉 **{number}**! Nice going everyone."
                    )),
                )
                .await;
        }
        return Ok(true);
    }

    // Wrong number, or the same person twice: reset.
    sqlx::query(
        "UPDATE counting SET current = 0, last_user = NULL WHERE guild_id = ? AND channel_id = ?",
    )
    .bind(guild_id.get() as i64)
    .bind(message.channel_id.get() as i64)
    .execute(&data.db)
    .await?;

    let _ = message
        .react(&sctx.http, ReactionType::Unicode("❌".to_string()))
        .await;

    let reason = if repeated {
        format!("<@{author}> counted twice in a row")
    } else {
        format!("<@{author}> said **{number}** when it should have been **{expected}**")
    };

    let _ = message
        .channel_id
        .send_message(
            &sctx.http,
            CreateMessage::new().embed(
                embed::titled("💥 Chain broken", embed::DANGER).description(format!(
                    "{reason}.\nBack to **1**. The record is **{best}**."
                )),
            ),
        )
        .await;

    Ok(true)
}

// ===========================================================================
// Word chain
// ===========================================================================

/// Set up the word chain channel.
#[poise::command(
    slash_command,
    guild_only,
    category = "Games",
    subcommands("wordchain_setup", "wordchain_status", "wordchain_off"),
    check = "perms::admin_only"
)]
pub async fn wordchain(_ctx: Ctx<'_>) -> Result<(), Error> {
    Ok(())
}

#[poise::command(slash_command, rename = "setup", guild_only)]
pub async fn wordchain_setup(
    ctx: Ctx<'_>,
    #[description = "Channel for the word chain"] channel: GuildChannel,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    sqlx::query(
        "INSERT INTO wordchain (guild_id, channel_id) VALUES (?, ?)
         ON CONFLICT(guild_id, channel_id) DO NOTHING",
    )
    .bind(guild_id)
    .bind(channel.id.get() as i64)
    .execute(&ctx.data().db)
    .await?;

    ctx.send(
        poise::CreateReply::default().embed(
            embed::ok(format!("Word chain is live in <#{}>.", channel.id.get())).footer(
                embed::footer(
                    "Each word must start with the last letter of the previous one, and no word twice.",
                ),
            ),
        ),
    )
    .await?;
    Ok(())
}

#[poise::command(slash_command, rename = "status", guild_only)]
pub async fn wordchain_status(ctx: Ctx<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let rows: Vec<(i64, String, i64, i64)> = sqlx::query_as(
        "SELECT channel_id, last_word, length, best FROM wordchain WHERE guild_id = ?",
    )
    .bind(guild_id)
    .fetch_all(&ctx.data().db)
    .await?;

    let body = if rows.is_empty() {
        "Word chain is not set up.".to_string()
    } else {
        rows.iter()
            .map(|(channel, last_word, length, best)| {
                format!(
                    "<#{channel}> — last word **{}**, chain **{length}**, record **{best}**",
                    if last_word.is_empty() { "none yet" } else { last_word }
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    ctx.send(
        poise::CreateReply::default()
            .embed(embed::titled("🔗 Word chain", embed::INFO).description(body)),
    )
    .await?;
    Ok(())
}

#[poise::command(slash_command, rename = "off", guild_only)]
pub async fn wordchain_off(
    ctx: Ctx<'_>,
    #[description = "Channel"] channel: GuildChannel,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    sqlx::query("DELETE FROM wordchain WHERE guild_id = ? AND channel_id = ?")
        .bind(guild_id)
        .bind(channel.id.get() as i64)
        .execute(&ctx.data().db)
        .await?;
    sqlx::query("DELETE FROM wordchain_used WHERE guild_id = ? AND channel_id = ?")
        .bind(guild_id)
        .bind(channel.id.get() as i64)
        .execute(&ctx.data().db)
        .await?;
    ctx.send(poise::CreateReply::default().embed(embed::ok("Word chain switched off there.")))
        .await?;
    Ok(())
}

/// Message handler for the word chain channel.
pub async fn handle_wordchain(
    sctx: &serenity::Context,
    data: &Data,
    message: &Message,
) -> Result<bool, Error> {
    let Some(guild_id) = message.guild_id else {
        return Ok(false);
    };
    let guild_i64 = guild_id.get() as i64;
    let channel_i64 = message.channel_id.get() as i64;

    let row: Option<(String, Option<i64>, i64, i64)> = sqlx::query_as(
        "SELECT last_word, last_user, length, best FROM wordchain
         WHERE guild_id = ? AND channel_id = ?",
    )
    .bind(guild_i64)
    .bind(channel_i64)
    .fetch_optional(&data.db)
    .await?;

    let Some((last_word, last_user, length, best)) = row else {
        return Ok(false);
    };

    // One single word, letters only.
    let word = message.content.trim().to_lowercase();
    if word.is_empty()
        || word.split_whitespace().count() != 1
        || !word.chars().all(|c| c.is_ascii_alphabetic())
        || word.len() < 2
    {
        return Ok(false);
    }

    let author = message.author.id.get() as i64;

    async fn react(sctx: &serenity::Context, message: &Message, emoji: &str) {
        let _ = message
            .react(&sctx.http, ReactionType::Unicode(emoji.to_string()))
            .await;
    }

    if last_user == Some(author) && !last_word.is_empty() {
        react(sctx, message, "🚫").await;
        let _ = message
            .reply(
                &sctx.http,
                "Wait for someone else to go before adding another word.",
            )
            .await;
        return Ok(true);
    }

    if !last_word.is_empty() {
        let required = last_word.chars().last().unwrap_or('a');
        if word.chars().next() != Some(required) {
            react(sctx, message, "❌").await;
            let _ = message
                .reply(
                    &sctx.http,
                    format!(
                        "**{word}** does not start with `{required}`. The chain is still at **{length}**."
                    ),
                )
                .await;
            return Ok(true);
        }
    }

    let used: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM wordchain_used WHERE guild_id = ? AND channel_id = ? AND word = ?",
    )
    .bind(guild_i64)
    .bind(channel_i64)
    .bind(&word)
    .fetch_one(&data.db)
    .await?;

    if used > 0 {
        react(sctx, message, "♻️").await;
        let _ = message
            .reply(&sctx.http, format!("**{word}** has already been used."))
            .await;
        return Ok(true);
    }

    let new_length = length + 1;
    let new_best = best.max(new_length);

    sqlx::query(
        "UPDATE wordchain SET last_word = ?, last_user = ?, length = ?, best = ?
         WHERE guild_id = ? AND channel_id = ?",
    )
    .bind(&word)
    .bind(author)
    .bind(new_length)
    .bind(new_best)
    .bind(guild_i64)
    .bind(channel_i64)
    .execute(&data.db)
    .await?;

    sqlx::query(
        "INSERT OR IGNORE INTO wordchain_used (guild_id, channel_id, word) VALUES (?, ?, ?)",
    )
    .bind(guild_i64)
    .bind(channel_i64)
    .bind(&word)
    .execute(&data.db)
    .await?;

    react(sctx, message, "✅").await;
    if new_length % 50 == 0 {
        let _ = message
            .channel_id
            .send_message(
                &sctx.http,
                CreateMessage::new().content(format!("🔗 **{new_length}** words in a row!")),
            )
            .await;
    }
    Ok(true)
}

// ===========================================================================
// Daily question
// ===========================================================================

/// Post a conversation starter.
#[poise::command(
    slash_command,
    guild_only,
    category = "Games",
    subcommands("dq_now", "dq_channel", "dq_add")
)]
pub async fn dailyquestion(_ctx: Ctx<'_>) -> Result<(), Error> {
    Ok(())
}

/// Post one right now.
#[poise::command(slash_command, rename = "now", guild_only, check = "perms::staff_only")]
pub async fn dq_now(ctx: Ctx<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    match post_daily_question(ctx.serenity_context(), ctx.data(), guild_id, Some(ctx.channel_id()))
        .await
    {
        true => {
            ctx.send(
                poise::CreateReply::default()
                    .embed(embed::ok("Posted."))
                    .ephemeral(true),
            )
            .await?;
        }
        false => {
            ctx.send(
                poise::CreateReply::default()
                    .embed(embed::err("I have run out of questions. Add some with `/dailyquestion add`.")),
            )
            .await?;
        }
    }
    Ok(())
}

/// Where the daily question is posted each day.
#[poise::command(slash_command, rename = "channel", guild_only, check = "perms::admin_only")]
pub async fn dq_channel(
    ctx: Ctx<'_>,
    #[description = "Channel, or leave empty to switch the daily post off"] channel: Option<
        GuildChannel,
    >,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    let value = channel
        .as_ref()
        .map(|c| c.id.get().to_string())
        .unwrap_or_default();
    db::kv_set(&ctx.data().db, guild_id, "dq_channel", &value).await?;

    let body = match channel {
        Some(c) => format!("A question will be posted in <#{}> each day.", c.id.get()),
        None => "Daily questions are off.".to_string(),
    };
    ctx.send(poise::CreateReply::default().embed(embed::ok(body)))
        .await?;
    Ok(())
}

/// Add a question to the pool.
#[poise::command(slash_command, rename = "add", guild_only, check = "perms::staff_only")]
pub async fn dq_add(
    ctx: Ctx<'_>,
    #[description = "The question"] question: String,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get() as i64;
    sqlx::query("INSERT INTO daily_questions (guild_id, question) VALUES (?, ?)")
        .bind(guild_id)
        .bind(&question)
        .execute(&ctx.data().db)
        .await?;
    ctx.send(
        poise::CreateReply::default()
            .embed(embed::ok("Added to the pool."))
            .ephemeral(true),
    )
    .await?;
    Ok(())
}

/// Pick an unused question and post it. Also used by the daily scheduler.
pub async fn post_daily_question(
    sctx: &serenity::Context,
    data: &Data,
    guild_id: i64,
    override_channel: Option<ChannelId>,
) -> bool {
    let channel = match override_channel {
        Some(channel) => channel,
        None => {
            let Some(raw) = db::kv_get(&data.db, guild_id, "dq_channel").await else {
                return false;
            };
            match raw.parse::<u64>() {
                Ok(id) => ChannelId::new(id),
                Err(_) => return false,
            }
        }
    };

    let row: Option<(i64, String)> = sqlx::query_as(
        "SELECT id, question FROM daily_questions
         WHERE (guild_id = 0 OR guild_id = ?) AND used_at IS NULL
         ORDER BY RANDOM() LIMIT 1",
    )
    .bind(guild_id)
    .fetch_optional(&data.db)
    .await
    .ok()
    .flatten();

    // All used up: start the rotation again rather than going quiet.
    let row = match row {
        Some(row) => Some(row),
        None => {
            let _ = sqlx::query(
                "UPDATE daily_questions SET used_at = NULL WHERE guild_id = 0 OR guild_id = ?",
            )
            .bind(guild_id)
            .execute(&data.db)
            .await;
            sqlx::query_as(
                "SELECT id, question FROM daily_questions
                 WHERE guild_id = 0 OR guild_id = ? ORDER BY RANDOM() LIMIT 1",
            )
            .bind(guild_id)
            .fetch_optional(&data.db)
            .await
            .ok()
            .flatten()
        }
    };

    let Some((question_id, question)) = row else {
        return false;
    };

    let sent = channel
        .send_message(
            &sctx.http,
            CreateMessage::new().embed(
                embed::titled("💭 Question of the day", embed::BRAND)
                    .description(format!("**{question}**"))
                    .footer(embed::footer("Reply in this channel")),
            ),
        )
        .await;

    if sent.is_ok() {
        let _ = sqlx::query("UPDATE daily_questions SET used_at = datetime('now') WHERE id = ?")
            .bind(question_id)
            .execute(&data.db)
            .await;
        return true;
    }
    false
}

pub fn commands() -> Vec<poise::Command<Data, Error>> {
    vec![trivia(), counting(), wordchain(), dailyquestion()]
}
