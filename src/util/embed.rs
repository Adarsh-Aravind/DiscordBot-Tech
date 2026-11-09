//! Consistent embed styling for every module.

use poise::serenity_prelude as serenity;
use serenity::{Colour, CreateEmbed, CreateEmbedFooter};

pub const BRAND: u32 = 0x5865F2; // blurple
pub const SUCCESS: u32 = 0x2ECC71;
pub const WARN: u32 = 0xF1C40F;
pub const DANGER: u32 = 0xE74C3C;
pub const INFO: u32 = 0x3498DB;
pub const MUTED: u32 = 0x95A5A6;
pub const XP: u32 = 0x9B59B6;
pub const STAR: u32 = 0xFFD54F;

pub fn colour(value: u32) -> Colour {
    Colour::from(value)
}

pub fn base() -> CreateEmbed {
    CreateEmbed::new().colour(colour(BRAND))
}

pub fn ok(description: impl Into<String>) -> CreateEmbed {
    CreateEmbed::new()
        .colour(colour(SUCCESS))
        .description(format!("✅ {}", description.into()))
}

pub fn err(description: impl Into<String>) -> CreateEmbed {
    CreateEmbed::new()
        .colour(colour(DANGER))
        .description(format!("❌ {}", description.into()))
}

pub fn warn(description: impl Into<String>) -> CreateEmbed {
    CreateEmbed::new()
        .colour(colour(WARN))
        .description(format!("⚠️ {}", description.into()))
}

pub fn info(title: impl Into<String>, description: impl Into<String>) -> CreateEmbed {
    CreateEmbed::new()
        .colour(colour(INFO))
        .title(title.into())
        .description(description.into())
}

pub fn titled(title: impl Into<String>, value: u32) -> CreateEmbed {
    CreateEmbed::new().colour(colour(value)).title(title.into())
}

pub fn footer(text: impl Into<String>) -> CreateEmbedFooter {
    CreateEmbedFooter::new(text.into())
}

/// Parse `#rrggbb`, `rrggbb` or a decimal colour into a `Colour`.
pub fn parse_colour(input: &str) -> Option<Colour> {
    let s = input.trim().trim_start_matches('#');
    if let Ok(v) = u32::from_str_radix(s, 16) {
        if s.len() == 6 {
            return Some(Colour::from(v));
        }
    }
    input.trim().parse::<u32>().ok().map(Colour::from)
}
