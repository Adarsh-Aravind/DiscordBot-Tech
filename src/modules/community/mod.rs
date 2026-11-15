//! Community engagement: polls, anonymous feedback, giveaways, events, games,
//! quotes, challenges and milestones.

pub mod events;
pub mod feedback;
pub mod games;
pub mod giveaways;
pub mod misc;
pub mod polls;

use poise::serenity_prelude as serenity;
use serenity::ComponentInteraction;

use crate::state::{Data, Error};

/// Try every community component handler in turn.
pub async fn handle_component(
    sctx: &serenity::Context,
    data: &Data,
    interaction: &ComponentInteraction,
) -> Result<bool, Error> {
    if polls::handle_component(sctx, data, interaction).await? {
        return Ok(true);
    }
    if giveaways::handle_component(sctx, data, interaction).await? {
        return Ok(true);
    }
    if events::handle_component(sctx, data, interaction).await? {
        return Ok(true);
    }
    if games::handle_component(sctx, data, interaction).await? {
        return Ok(true);
    }
    Ok(false)
}

/// Message handlers for the channel-based games. Returns true when the message
/// was consumed by a game.
pub async fn handle_message(
    sctx: &serenity::Context,
    data: &Data,
    message: &serenity::Message,
) -> Result<bool, Error> {
    if games::handle_counting(sctx, data, message).await? {
        return Ok(true);
    }
    if games::handle_wordchain(sctx, data, message).await? {
        return Ok(true);
    }
    Ok(false)
}

pub fn commands() -> Vec<poise::Command<Data, Error>> {
    let mut all = vec![
        polls::poll(),
        feedback::feedback(),
        feedback::feedbackchannel(),
        giveaways::giveaway(),
        events::event(),
    ];
    all.extend(games::commands());
    all.extend(misc::commands());
    all
}
