//! MrTechTrivia community bot.
//!
//! Layout:
//!   config.rs   process configuration from the environment
//!   state.rs    shared state handed to every command and handler
//!   db.rs       pool, schema and the typed per-guild config rows
//!   compat.rs   wrappers around version-sensitive serenity calls
//!   logs.rs     one place that decides where audit output goes
//!   events.rs   the gateway event router
//!   tasks.rs    background loops
//!   modules/    one module per feature area

mod compat;
mod config;
mod gate;
mod db;
mod events;
mod logs;
mod modules;
mod state;
mod tasks;
mod util;

use std::sync::atomic::Ordering;
use std::sync::Arc;

use poise::serenity_prelude as serenity;

use crate::config::Config;
use crate::state::{BotState, Data, Error};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // `.env` is optional: real deployments usually set the environment directly.
    let _ = dotenvy::dotenv();
    init_tracing();

    let config = Config::from_env()?;
    // New guild rows start with this prefix; `/config prefix` overrides it.
    db::set_default_prefix(&config.prefix);
    tracing::info!("starting up");

    let pool = db::connect(&config.database_url).await?;
    tracing::info!(database = %config.database_url, "database ready");

    let intents = serenity::GatewayIntents::GUILDS
        | serenity::GatewayIntents::GUILD_MEMBERS
        | serenity::GatewayIntents::GUILD_MESSAGES
        | serenity::GatewayIntents::MESSAGE_CONTENT
        | serenity::GatewayIntents::GUILD_VOICE_STATES
        | serenity::GatewayIntents::GUILD_MODERATION
        | serenity::GatewayIntents::DIRECT_MESSAGES;

    let options = poise::FrameworkOptions {
        commands: modules::all(),
        prefix_options: poise::PrefixFrameworkOptions {
            // The env value is the fallback. Each guild's own prefix comes from
            // `dynamic_prefix` below: without it, `/config prefix` changed only
            // which word triggered a custom command, and `!ban` kept answering
            // to the process-wide prefix whatever the server had configured.
            prefix: Some(config.prefix.clone()),
            dynamic_prefix: Some(|ctx: poise::PartialContext<'_, Data, Error>| {
                Box::pin(async move {
                    let Some(guild_id) = ctx.guild_id else {
                        return Ok(None);
                    };
                    // Cached, so this costs nothing per message.
                    Ok(db::guild_config(&ctx.data.db, guild_id.get() as i64)
                        .await
                        .ok()
                        .map(|cfg| cfg.prefix))
                })
            }),
            mention_as_prefix: true,
            case_insensitive_commands: true,
            ..Default::default()
        },
        on_error: |error| Box::pin(on_error(error)),
        event_handler: |ctx, event, framework, data| {
            Box::pin(events::handle(ctx, event, framework, data))
        },
        pre_command: |ctx| {
            Box::pin(async move {
                tracing::debug!(
                    command = %ctx.command().qualified_name,
                    user = %ctx.author().tag(),
                    guild = ?ctx.guild_id().map(|g| g.get()),
                    "command invoked"
                );
            })
        },
        post_command: |ctx| {
            Box::pin(async move {
                ctx.data().commands_run.fetch_add(1, Ordering::Relaxed);
            })
        },
        // One place decides who may run what, including every subcommand.
        command_check: Some(|ctx| Box::pin(gate::check(ctx))),
        ..Default::default()
    };

    let setup_config = config.clone();
    let framework = poise::Framework::builder()
        .options(options)
        .setup(move |ctx, ready, framework| {
            Box::pin(async move {
                let command_count = framework.options().commands.len();

                match setup_config.dev_guild {
                    Some(guild_id) => {
                        let guild = serenity::GuildId::new(guild_id);
                        poise::builtins::register_in_guild(
                            ctx,
                            &framework.options().commands,
                            guild,
                        )
                        .await?;
                        tracing::info!(
                            guild = guild_id,
                            commands = command_count,
                            "registered commands to the development guild"
                        );
                    }
                    None => {
                        poise::builtins::register_globally(ctx, &framework.options().commands)
                            .await?;
                        tracing::info!(
                            commands = command_count,
                            "registered commands globally (can take up to an hour to appear)"
                        );
                    }
                }

                let data: Data = Arc::new(BotState::new(pool, setup_config));
                tracing::info!(user = %ready.user.name, "bot is ready");

                // Load the optional phishing feed, then start the background loops.
                tasks::refresh_phish_list(&data).await;
                tasks::spawn_all(ctx.clone(), data.clone());

                Ok(data)
            })
        })
        .build();

    let mut client = serenity::ClientBuilder::new(&config.token, intents)
        .framework(framework)
        .await?;

    // Stop cleanly on Ctrl-C so SQLite flushes its write-ahead log.
    let shard_manager = client.shard_manager.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            tracing::info!("shutdown requested, closing connections");
            shard_manager.shutdown_all().await;
        }
    });

    client.start().await?;
    Ok(())
}

fn init_tracing() {
    use tracing_subscriber::{fmt, prelude::*, EnvFilter};

    // Default to our own info logs and quiet serenity's chatter.
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new("mrtechtrivia_bot=info,serenity=warn,sqlx=warn,tracing::span=warn")
    });

    tracing_subscriber::registry()
        .with(fmt::layer().with_target(true).compact())
        .with(filter)
        .init();
}

/// Turn framework errors into something the user can act on.
async fn on_error(error: poise::FrameworkError<'_, Data, Error>) {
    use crate::util::embed;

    match error {
        poise::FrameworkError::Setup { error, .. } => {
            tracing::error!("failed to start: {error}");
        }
        poise::FrameworkError::Command { error, ctx, .. } => {
            tracing::error!(
                command = %ctx.command().qualified_name,
                "command failed: {error}"
            );

            let _ = ctx
                .send(
                    poise::CreateReply::default()
                        .embed(embed::err(format!(
                            "That command did not finish.\n```\n{}\n```\nIf it keeps happening, \
                             check that I have the permissions I need with `/permissions`.",
                            crate::util::text::truncate(&error.to_string(), 500)
                        )))
                        .ephemeral(true),
                )
                .await;
        }
        poise::FrameworkError::ArgumentParse { error, input, ctx, .. } => {
            let detail = match input {
                Some(input) => format!("I could not read `{input}`: {error}"),
                None => format!("I could not read that: {error}"),
            };
            let _ = ctx
                .send(
                    poise::CreateReply::default()
                        .embed(embed::err(detail))
                        .ephemeral(true),
                )
                .await;
        }
        poise::FrameworkError::CommandCheckFailed { ctx, error, .. } => {
            // The check itself normally explains why; only speak up if it did not.
            if let Some(error) = error {
                tracing::warn!("check failed: {error}");
                let _ = ctx
                    .send(
                        poise::CreateReply::default()
                            .embed(embed::err("You cannot use that here."))
                            .ephemeral(true),
                    )
                    .await;
            }
        }
        poise::FrameworkError::MissingBotPermissions {
            missing_permissions,
            ctx,
            ..
        } => {
            let _ = ctx
                .send(
                    poise::CreateReply::default()
                        .embed(embed::err(format!(
                            "I am missing the **{missing_permissions}** permission here."
                        )))
                        .ephemeral(true),
                )
                .await;
        }
        poise::FrameworkError::MissingUserPermissions {
            missing_permissions,
            ctx,
            ..
        } => {
            let detail = match missing_permissions {
                Some(permissions) => {
                    format!("You need the **{permissions}** permission to do that.")
                }
                None => "You do not have permission to do that.".to_string(),
            };
            let _ = ctx
                .send(
                    poise::CreateReply::default()
                        .embed(embed::err(detail))
                        .ephemeral(true),
                )
                .await;
        }
        poise::FrameworkError::GuildOnly { ctx, .. } => {
            let _ = ctx
                .send(
                    poise::CreateReply::default()
                        .embed(embed::err("That command only works inside a server."))
                        .ephemeral(true),
                )
                .await;
        }
        poise::FrameworkError::CooldownHit {
            remaining_cooldown,
            ctx,
            ..
        } => {
            let _ = ctx
                .send(
                    poise::CreateReply::default()
                        .embed(embed::warn(format!(
                            "Slow down a moment - try again in {:.1}s.",
                            remaining_cooldown.as_secs_f32()
                        )))
                        .ephemeral(true),
                )
                .await;
        }
        other => {
            if let Err(why) = poise::builtins::on_error(other).await {
                tracing::error!("error handler itself failed: {why}");
            }
        }
    }
}
