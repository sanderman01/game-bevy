//! `ename_image_info` over images built in code.

use ename_xtask::{
    image_info::{image_info, summarize},
    read_asset::{ScanPolicy, read_asset},
};
use image::{DynamicImage, ImageBuffer, ImageFormat, Luma, Rgb, Rgba};
use std::io::Cursor;

fn encode(image: DynamicImage, format: ImageFormat) -> Vec<u8> {
    let mut bytes = Cursor::new(Vec::new());
    image.write_to(&mut bytes, format).unwrap();
    bytes.into_inner()
}

/// 4x1 RGBA: alpha 0, 255, 255 and 128, so a quarter is cut out and a quarter blended.
fn rgba_png() -> Vec<u8> {
    let alphas = [0_u8, 255, 255, 128];
    let image = ImageBuffer::from_fn(4, 1, |x, _| Rgba([255, 0, 51, alphas[x as usize]]));
    encode(DynamicImage::ImageRgba8(image), ImageFormat::Png)
}

#[test]
fn rgba_png_reports_channels_and_alpha_shares() {
    let summary = summarize(&rgba_png()).unwrap();
    assert_eq!(summary.headline(), "png 4x1  rgba8");
    let (name, red) = summary.channels[0];
    assert_eq!((name, red.min, red.max), ('r', 1.0, 1.0));
    assert!((summary.channels[2].1.mean - 0.2).abs() < 1e-6);
    let alpha = summary.alpha.unwrap();
    assert_eq!(
        (alpha.at_zero, alpha.at_one, alpha.between),
        (0.25, 0.5, 0.25)
    );
    assert_eq!(alpha.kind(), "blended");
    assert_eq!(
        summary.detail_lines()[1],
        "alpha: 0.00..1.00 mean 0.63  at 0 25.0 %  at 1 50.0 %  between 25.0 %  (blended)"
    );
}

#[test]
fn gray_16_bit_png_reports_its_native_range() {
    let values = [1000_u16, 2000, 65535];
    let image = ImageBuffer::from_fn(3, 1, |x, _| Luma([values[x as usize]]));
    let summary = summarize(&encode(DynamicImage::ImageLuma16(image), ImageFormat::Png)).unwrap();
    assert_eq!(summary.headline(), "png 3x1  l16");
    assert_eq!(summary.channels.len(), 1);
    assert_eq!(summary.alpha, None);
    assert_eq!(summary.native_16, vec![('l', 1000, 65535)]);
    assert_eq!(summary.detail_lines()[2], "16-bit: l 1000..65535");
}

#[test]
fn jpeg_decodes_without_alpha() {
    let image = ImageBuffer::from_pixel(8, 8, Rgb([200_u8, 100, 50]));
    let summary = summarize(&encode(DynamicImage::ImageRgb8(image), ImageFormat::Jpeg)).unwrap();
    assert_eq!(summary.headline(), "jpeg 8x8  rgb8");
    assert_eq!(summary.detail_lines()[1], "alpha: none");
}

#[test]
fn gpu_containers_are_named_not_decoded() {
    assert_eq!(
        summarize(b"\xABKTX 20\xBB\r\n\x1A\nrest").unwrap_err(),
        "ktx2: unsupported (header only)"
    );
    assert_eq!(
        summarize(b"DDS rest").unwrap_err(),
        "dds: unsupported (header only)"
    );
}

#[test]
fn a_file_block_includes_the_meta_sidecar() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("a.png"), rgba_png()).unwrap();
    std::fs::write(
        root.path().join("a.png.meta"),
        "(asset: Load(loader: \"x\", settings: (is_srgb: false, sampler: Default)))",
    )
    .unwrap();
    std::fs::write(root.path().join("b.png"), rgba_png()).unwrap();
    let policy = ScanPolicy::new(root.path(), &[], None);
    let block = image_info(&read_asset("a.png", &policy).unwrap()).unwrap();
    assert!(block.starts_with("a.png  png 4x1  rgba8  "), "{block}");
    assert!(
        block.ends_with("  .meta: loader \"x\"  is_srgb false  sampler Default\n"),
        "{block}"
    );
    let block = image_info(&read_asset("b.png", &policy).unwrap()).unwrap();
    assert!(
        block.ends_with("  .meta: none (Bevy defaults: is_srgb true, sampler Default)\n"),
        "{block}"
    );
}
