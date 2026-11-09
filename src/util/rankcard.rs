//! Rank card rendering. Produces a PNG; callers fall back to an embed when this
//! returns `None` (no font available, avatar fetch failed, ...).

use ab_glyph::{FontVec, PxScale};
use image::{Rgba, RgbaImage};
use imageproc::drawing::{draw_filled_circle_mut, draw_filled_rect_mut, draw_text_mut, text_size};
use imageproc::rect::Rect;

pub struct RankInfo {
    pub name: String,
    pub discriminator: String,
    pub avatar_url: String,
    pub level: i64,
    pub prestige: i64,
    pub rank: i64,
    pub xp_into_level: i64,
    pub xp_needed: i64,
    pub total_xp: i64,
    pub messages: i64,
    pub voice_secs: i64,
    pub streak: i64,
    /// `#rrggbb` accent colour from the member's profile.
    pub accent: String,
}

const WIDTH: u32 = 934;
const HEIGHT: u32 = 282;

const BG: Rgba<u8> = Rgba([27, 29, 36, 255]);
const PANEL: Rgba<u8> = Rgba([35, 38, 47, 255]);
const TRACK: Rgba<u8> = Rgba([58, 62, 74, 255]);
const WHITE: Rgba<u8> = Rgba([245, 246, 250, 255]);
const GREY: Rgba<u8> = Rgba([154, 160, 173, 255]);

pub async fn render(
    http: &reqwest::Client,
    font_path: Option<&str>,
    info: &RankInfo,
) -> Option<Vec<u8>> {
    let font_bytes = std::fs::read(font_path?).ok()?;
    let font = FontVec::try_from_vec(font_bytes).ok()?;
    let accent = parse_hex(&info.accent).unwrap_or(Rgba([88, 101, 242, 255]));

    let mut canvas = RgbaImage::from_pixel(WIDTH, HEIGHT, BG);

    // Rounded inner panel.
    draw_filled_rect_mut(
        &mut canvas,
        Rect::at(14, 14).of_size(WIDTH - 28, HEIGHT - 28),
        PANEL,
    );
    // Accent stripe down the left edge.
    draw_filled_rect_mut(&mut canvas, Rect::at(14, 14).of_size(8, HEIGHT - 28), accent);

    // ---- avatar ----------------------------------------------------------
    let avatar_size: u32 = 168;
    let avatar_x: i32 = 46;
    let avatar_y: i32 = ((HEIGHT - avatar_size) / 2) as i32;
    let centre = (
        avatar_x + (avatar_size / 2) as i32,
        avatar_y + (avatar_size / 2) as i32,
    );

    // Accent ring behind the avatar.
    draw_filled_circle_mut(&mut canvas, centre, (avatar_size / 2 + 5) as i32, accent);

    if let Some(bytes) = fetch(http, &info.avatar_url).await {
        if let Ok(avatar) = image::load_from_memory(&bytes) {
            let avatar = avatar
                .resize_to_fill(
                    avatar_size,
                    avatar_size,
                    image::imageops::FilterType::Lanczos3,
                )
                .to_rgba8();
            blit_circular(&mut canvas, &avatar, avatar_x, avatar_y);
        }
    }

    // ---- text ------------------------------------------------------------
    let name_scale = PxScale::from(44.0);
    let label_scale = PxScale::from(24.0);
    let big_scale = PxScale::from(38.0);
    let small_scale = PxScale::from(21.0);

    let text_x = 252;

    draw_text_mut(
        &mut canvas,
        WHITE,
        text_x,
        52,
        name_scale,
        &font,
        &info.name,
    );
    let (name_w, _) = text_size(name_scale, &font, &info.name);
    if !info.discriminator.is_empty() {
        draw_text_mut(
            &mut canvas,
            GREY,
            text_x + name_w as i32 + 10,
            64,
            label_scale,
            &font,
            &info.discriminator,
        );
    }

    // Rank / level block, right aligned.
    let level_text = if info.prestige > 0 {
        format!("P{} · LVL {}", info.prestige, info.level)
    } else {
        format!("LVL {}", info.level)
    };
    let rank_text = format!("RANK #{}", info.rank);
    let (lvl_w, _) = text_size(big_scale, &font, &level_text);
    let (rank_w, _) = text_size(small_scale, &font, &rank_text);
    draw_text_mut(
        &mut canvas,
        accent,
        (WIDTH - 52) as i32 - lvl_w as i32,
        44,
        big_scale,
        &font,
        &level_text,
    );
    draw_text_mut(
        &mut canvas,
        GREY,
        (WIDTH - 52) as i32 - rank_w as i32,
        92,
        small_scale,
        &font,
        &rank_text,
    );

    // ---- xp bar ----------------------------------------------------------
    let bar_x = text_x;
    let bar_y = 170;
    let bar_w = WIDTH - bar_x as u32 - 52;
    let bar_h = 34u32;

    draw_filled_rect_mut(
        &mut canvas,
        Rect::at(bar_x, bar_y).of_size(bar_w, bar_h),
        TRACK,
    );
    let ratio = if info.xp_needed > 0 {
        (info.xp_into_level as f64 / info.xp_needed as f64).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let filled = ((bar_w as f64) * ratio).round() as u32;
    if filled > 0 {
        draw_filled_rect_mut(
            &mut canvas,
            Rect::at(bar_x, bar_y).of_size(filled.max(2), bar_h),
            accent,
        );
    }

    let xp_text = format!(
        "{} / {} XP   ·   {} total",
        thousands(info.xp_into_level),
        thousands(info.xp_needed),
        thousands(info.total_xp)
    );
    draw_text_mut(&mut canvas, GREY, bar_x, bar_y - 32, small_scale, &font, &xp_text);

    let stats = format!(
        "{} messages   ·   {} in voice   ·   {} day streak",
        thousands(info.messages),
        crate::util::time::humanize(info.voice_secs),
        info.streak
    );
    draw_text_mut(
        &mut canvas,
        GREY,
        bar_x,
        (bar_y + bar_h as i32) + 10,
        small_scale,
        &font,
        &stats,
    );

    encode_png(&canvas)
}

async fn fetch(http: &reqwest::Client, url: &str) -> Option<Vec<u8>> {
    let resp = http.get(url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.bytes().await.ok().map(|b| b.to_vec())
}

/// Copy `src` onto `dst` at (x, y), keeping only the pixels inside a circle.
fn blit_circular(dst: &mut RgbaImage, src: &RgbaImage, x: i32, y: i32) {
    let size = src.width().min(src.height()) as f64;
    let radius = size / 2.0;
    let cx = radius;
    let cy = radius;
    for (sx, sy, pixel) in src.enumerate_pixels() {
        let dx = sx as f64 + 0.5 - cx;
        let dy = sy as f64 + 0.5 - cy;
        if dx * dx + dy * dy > radius * radius {
            continue;
        }
        let tx = x + sx as i32;
        let ty = y + sy as i32;
        if tx < 0 || ty < 0 || tx >= dst.width() as i32 || ty >= dst.height() as i32 {
            continue;
        }
        dst.put_pixel(tx as u32, ty as u32, *pixel);
    }
}

fn encode_png(canvas: &RgbaImage) -> Option<Vec<u8>> {
    let mut buffer = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(canvas.clone())
        .write_to(&mut buffer, image::ImageFormat::Png)
        .ok()?;
    Some(buffer.into_inner())
}

fn parse_hex(input: &str) -> Option<Rgba<u8>> {
    let s = input.trim().trim_start_matches('#');
    if s.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&s[0..2], 16).ok()?;
    let g = u8::from_str_radix(&s[2..4], 16).ok()?;
    let b = u8::from_str_radix(&s[4..6], 16).ok()?;
    Some(Rgba([r, g, b, 255]))
}

/// `1234567` -> `1,234,567`
pub fn thousands(value: i64) -> String {
    let negative = value < 0;
    let digits = value.abs().to_string();
    let mut out = String::new();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    if negative {
        format!("-{out}")
    } else {
        out
    }
}
