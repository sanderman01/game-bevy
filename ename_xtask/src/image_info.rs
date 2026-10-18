//! `ename_image_info` -- what is inside a PNG or JPEG: size, pixel format, per-channel ranges and
//! the alpha distribution, plus the `.meta` settings Bevy loads it with.

use crate::read_asset::{AssetFile, ReadError, human_size};
use image::{ColorType, DynamicImage, ImageFormat};
use std::fmt::Write;

/// One channel's range and mean, normalized to 0..1.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChannelStats {
    pub min: f32,
    pub max: f32,
    pub mean: f32,
}

/// The alpha channel, with how many pixels sit at 0, at 1 and in between.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AlphaStats {
    pub range: ChannelStats,
    /// Shares of all pixels, 0..1.
    pub at_zero: f32,
    pub at_one: f32,
    pub between: f32,
}

impl AlphaStats {
    /// What the alpha channel is used for, judged from the shares.
    pub fn kind(&self) -> &'static str {
        if self.at_one == 1.0 {
            "opaque"
        } else if self.between == 0.0 {
            "cutout"
        } else {
            "blended"
        }
    }
}

/// A decoded image's summary.
#[derive(Debug, Clone, PartialEq)]
pub struct ImageSummary {
    pub format: &'static str,
    pub width: u32,
    pub height: u32,
    pub color: ColorType,
    /// Colour channels only, named (`r`, `g`, `b` or `l`).
    pub channels: Vec<(char, ChannelStats)>,
    pub alpha: Option<AlphaStats>,
    /// For 16-bit images, each channel's native range, alpha included.
    pub native_16: Vec<(char, u16, u16)>,
}

/// Decodes `bytes` and summarizes them, or says why it could not.
pub fn summarize(bytes: &[u8]) -> Result<ImageSummary, String> {
    if let Some(container) = unsupported_container(bytes) {
        return Err(format!("{container}: unsupported (header only)"));
    }
    let format = image::guess_format(bytes).map_err(|err| err.to_string())?;
    let name = match format {
        ImageFormat::Png => "png",
        ImageFormat::Jpeg => "jpeg",
        other => return Err(format!("{other:?}: unsupported")),
    };
    let image = image::load_from_memory_with_format(bytes, format).map_err(|e| e.to_string())?;
    Ok(summarize_image(name, &image))
}

fn summarize_image(format: &'static str, image: &DynamicImage) -> ImageSummary {
    let color = image.color();
    let names: &[char] = match color.channel_count() {
        1 | 2 => &['l'],
        _ => &['r', 'g', 'b'],
    };
    let pixels = image.to_rgba32f();
    let count = f64::from(image.width()) * f64::from(image.height());
    let stats = |channel: usize| {
        let mut min = f32::INFINITY;
        let mut max = f32::NEG_INFINITY;
        let mut sum = 0.0_f64;
        for pixel in pixels.pixels() {
            let value = pixel.0[channel];
            min = min.min(value);
            max = max.max(value);
            sum += f64::from(value);
        }
        if count == 0.0 {
            min = 0.0;
            max = 0.0;
        }
        ChannelStats {
            min,
            max,
            mean: (sum / count.max(1.0)) as f32,
        }
    };
    let channels = names
        .iter()
        .enumerate()
        .map(|(index, name)| (*name, stats(index)))
        .collect();
    let alpha = color.has_alpha().then(|| {
        let (mut at_zero, mut at_one) = (0_u64, 0_u64);
        for pixel in pixels.pixels() {
            let value = pixel.0[3];
            if value <= 0.0 {
                at_zero += 1;
            } else if value >= 1.0 {
                at_one += 1;
            }
        }
        let share = |n: u64| (n as f64 / count.max(1.0)) as f32;
        let between = count as u64 - at_zero - at_one;
        AlphaStats {
            range: stats(3),
            at_zero: share(at_zero),
            at_one: share(at_one),
            between: share(between),
        }
    });
    ImageSummary {
        format,
        width: image.width(),
        height: image.height(),
        color,
        channels,
        alpha,
        native_16: native_16(image),
    }
}

/// Each channel's range in its own 16-bit units, for the formats that have them.
fn native_16(image: &DynamicImage) -> Vec<(char, u16, u16)> {
    let (names, samples): (&[char], &[u16]) = match image {
        DynamicImage::ImageLuma16(image) => (&['l'], image.as_raw()),
        DynamicImage::ImageLumaA16(image) => (&['l', 'a'], image.as_raw()),
        DynamicImage::ImageRgb16(image) => (&['r', 'g', 'b'], image.as_raw()),
        DynamicImage::ImageRgba16(image) => (&['r', 'g', 'b', 'a'], image.as_raw()),
        _ => return Vec::new(),
    };
    names
        .iter()
        .enumerate()
        .map(|(index, name)| {
            let values = samples.iter().skip(index).step_by(names.len());
            let min = values.clone().copied().min().unwrap_or(0);
            let max = values.copied().max().unwrap_or(0);
            (*name, min, max)
        })
        .collect()
}

/// GPU containers Bevy reads with features of their own; named, not decoded.
fn unsupported_container(bytes: &[u8]) -> Option<&'static str> {
    const KTX2: &[u8] = b"\xABKTX 20\xBB\r\n\x1A\n";
    if bytes.starts_with(KTX2) {
        Some("ktx2")
    } else if bytes.starts_with(b"DDS ") {
        Some("dds")
    } else {
        None
    }
}

impl ImageSummary {
    /// `png 2048x2048  rgba8`
    pub fn headline(&self) -> String {
        format!(
            "{} {}x{}  {}",
            self.format,
            self.width,
            self.height,
            color_name(self.color)
        )
    }

    /// The channel and alpha lines, without the headline.
    pub fn detail_lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        let channels = self
            .channels
            .iter()
            .map(|(name, stats)| format!("{name} {}", range(stats)))
            .collect::<Vec<_>>()
            .join("   ");
        lines.push(channels);
        match &self.alpha {
            None => lines.push("alpha: none".to_owned()),
            Some(alpha) => lines.push(format!(
                "alpha: {}  at 0 {}  at 1 {}  between {}  ({})",
                range(&alpha.range),
                percent(alpha.at_zero),
                percent(alpha.at_one),
                percent(alpha.between),
                alpha.kind()
            )),
        }
        if !self.native_16.is_empty() {
            let native = self
                .native_16
                .iter()
                .map(|(name, min, max)| format!("{name} {min}..{max}"))
                .collect::<Vec<_>>()
                .join("   ");
            lines.push(format!("16-bit: {native}"));
        }
        lines
    }
}

/// The block `ename_image_info` prints for one file.
pub fn image_info(file: &AssetFile) -> Result<String, ReadError> {
    let mut out = String::new();
    let size = human_size(file.bytes.len());
    match summarize(&file.bytes) {
        Ok(summary) => {
            let _ = writeln!(out, "{}  {}  {size}", file.name(), summary.headline());
            for line in summary.detail_lines() {
                let _ = writeln!(out, "  {line}");
            }
        }
        Err(err) => {
            let _ = writeln!(out, "{}  {size}", file.name());
            let _ = writeln!(out, "  {err}");
        }
    }
    let meta = match file.meta()? {
        None => "none (Bevy defaults: is_srgb true, sampler Default)".to_owned(),
        Some(bytes) => describe_meta(&String::from_utf8_lossy(&bytes)),
    };
    let _ = writeln!(out, "  .meta: {meta}");
    Ok(out)
}

/// The image settings a `.meta` file sets, or its text collapsed to one line if they are not
/// found.
fn describe_meta(text: &str) -> String {
    let fields: Vec<String> = ["loader", "is_srgb", "sampler", "texture_format"]
        .into_iter()
        .filter_map(|key| Some(format!("{key} {}", ron_field(text, key)?)))
        .collect();
    if fields.is_empty() {
        text.split_whitespace().collect::<Vec<_>>().join(" ")
    } else {
        fields.join("  ")
    }
}

/// The value after `key:` in RON text, up to the next `,` or `)` outside brackets, with
/// whitespace collapsed. Enough for the flat settings Bevy writes; not a RON parser.
fn ron_field(text: &str, key: &str) -> Option<String> {
    let start = text
        .match_indices(key)
        .find(|(at, _)| {
            let before = text[..*at].chars().next_back();
            let after = text[at + key.len()..].trim_start();
            !before.is_some_and(|c| c.is_alphanumeric() || c == '_') && after.starts_with(':')
        })
        .map(|(at, _)| at + key.len())?;
    let rest = text[start..].trim_start().strip_prefix(':')?;
    let mut depth = 0_i32;
    let mut end = rest.len();
    for (at, c) in rest.char_indices() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' if depth == 0 => {
                end = at;
                break;
            }
            ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                end = at;
                break;
            }
            _ => {}
        }
    }
    Some(rest[..end].split_whitespace().collect::<Vec<_>>().join(" "))
}

fn color_name(color: ColorType) -> String {
    format!("{color:?}").to_ascii_lowercase()
}

fn range(stats: &ChannelStats) -> String {
    format!("{:.2}..{:.2} mean {:.2}", stats.min, stats.max, stats.mean)
}

fn percent(share: f32) -> String {
    format!("{:.1} %", share * 100.0)
}

#[cfg(test)]
mod tests {
    use super::{describe_meta, ron_field};

    const META: &str = r#"(
    meta_format_version: "1.0",
    asset: Load(
        loader: "bevy_image::image_loader::ImageLoader",
        settings: (
            format: FromExtension,
            is_srgb: false,
            sampler: Descriptor((
                address_mode_u: Repeat,
            )),
            asset_usage: ("MAIN_WORLD | RENDER_WORLD"),
        ),
    ),
)"#;

    #[test]
    fn meta_fields_are_read_from_ron_text() {
        assert_eq!(ron_field(META, "is_srgb").as_deref(), Some("false"));
        assert_eq!(
            ron_field(META, "sampler").as_deref(),
            Some("Descriptor(( address_mode_u: Repeat, ))")
        );
        assert_eq!(ron_field(META, "texture_format"), None);
        assert_eq!(
            describe_meta(META),
            "loader \"bevy_image::image_loader::ImageLoader\"  is_srgb false  sampler Descriptor(( address_mode_u: Repeat, ))"
        );
    }
}
