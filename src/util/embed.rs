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

pub fn colour(value: u32) -> Colour {
    Colour::from(value)
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

pub fn titled(title: impl Into<String>, value: u32) -> CreateEmbed {
    CreateEmbed::new().colour(colour(value)).title(title.into())
}

pub fn footer(text: impl Into<String>) -> CreateEmbedFooter {
    CreateEmbedFooter::new(text.into())
}

/// The 24-bit value a colour string stands for: `#rrggbb`, `rrggbb`,
/// `0xrrggbb`, the short `#abc` form, or a plain decimal as Discord's own API
/// uses. `None` for anything else.
///
/// Every colour string in the bot goes through this one function, and
/// [`canonical_colour`] turns the result back into the single form we store, so
/// a value that parses once always parses again.
pub fn colour_value(input: &str) -> Option<u32> {
    let raw = input.trim();
    if raw.is_empty() {
        return None;
    }

    let hex = raw
        .strip_prefix('#')
        .or_else(|| raw.strip_prefix("0x"))
        .or_else(|| raw.strip_prefix("0X"))
        .unwrap_or(raw);

    if hex.len() == 6 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return u32::from_str_radix(hex, 16).ok();
    }
    if hex.len() == 3 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
        // #abc is the same colour as #aabbcc.
        let mut expanded = String::with_capacity(6);
        for c in hex.chars() {
            expanded.push(c);
            expanded.push(c);
        }
        return u32::from_str_radix(&expanded, 16).ok();
    }
    if raw.chars().all(|c| c.is_ascii_digit()) {
        let value: u32 = raw.parse().ok()?;
        if value <= 0x00FF_FFFF {
            return Some(value);
        }
    }
    None
}

/// Parse a colour string into a `Colour`.
pub fn parse_colour(input: &str) -> Option<Colour> {
    colour_value(input).map(Colour::from)
}

/// The one form we store colours in, so reading them back never fails.
pub fn canonical_colour(value: u32) -> String {
    format!("#{:06x}", value & 0x00FF_FFFF)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_the_forms_people_type() {
        assert_eq!(colour_value("#5865F2"), Some(0x5865F2));
        assert_eq!(colour_value("5865f2"), Some(0x5865F2));
        assert_eq!(colour_value("0x5865F2"), Some(0x5865F2));
        assert_eq!(colour_value("#abc"), Some(0xAABBCC));
        assert_eq!(colour_value("16711680"), Some(16711680));
        assert_eq!(colour_value("  #ff8800  "), Some(0xFF8800));
    }

    #[test]
    fn rejects_everything_else() {
        for input in ["", "#", "nope", "#12345", "#gggggg", "4294967295", "#ff88 00"] {
            assert_eq!(colour_value(input), None, "{input} was accepted");
        }
    }

    /// Whatever we accept has to survive a round trip through storage, or the
    /// read side has to cope with values the write side invented.
    #[test]
    fn canonical_form_round_trips() {
        for input in ["#5865F2", "5865f2", "#abc", "16711680", "0xFF8800", "123"] {
            let value = colour_value(input).expect(input);
            let stored = canonical_colour(value);
            assert_eq!(colour_value(&stored), Some(value), "{input} -> {stored}");
        }
    }
}
