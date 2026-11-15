pub mod automation;
pub mod automod;
pub mod community;
pub mod leveling;
pub mod moderation;
pub mod settings;
pub mod utility;
pub mod voice;
pub mod welcome;

use crate::state::{Data, Error};

/// Every slash and prefix command the bot exposes.
pub fn all() -> Vec<poise::Command<Data, Error>> {
    let mut commands = Vec::new();
    commands.extend(moderation::commands());
    commands.extend(automod::commands());
    commands.extend(welcome::commands());
    commands.extend(leveling::commands());
    commands.extend(community::commands());
    commands.extend(voice::commands());
    commands.extend(automation::commands());
    commands.extend(utility::commands());
    commands.extend(settings::commands());
    commands
}
