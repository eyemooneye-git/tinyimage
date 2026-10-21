//! 图片压缩核心逻辑。
//!
//! 目标：把几 MB 的 PNG / JPG 压到 50 KB 左右。
//! 可接受区间 35–65 KB，硬上限 100 KB。
//!
//! 策略：
//! - JPG：优先保留分辨率，二分搜索 JPEG 质量；如果质量太低仍超标，再按比例缩小尺寸。
//! - PNG：继续输出 PNG（保留透明通道），用调色板量化 + 必要缩放控制体积。

use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use color_quant::NeuQuant;
use image::imageops::{self, FilterType};
use image::metadata::Orientation;
use image::{DynamicImage, GenericImageView, ImageDecoder, ImageFormat, ImageReader, RgbaImage};
use jpeg_encoder::{ColorType as JpegColorType, Encoder as JpegEncoder, SamplingFactor};

/// 理想目标体积。
pub const TARGET_BYTES: u64 = 50 * 1024;
/// 可接受的下限（比目标小 30%）。
pub const ACCEPT_LOW: u64 = 35 * 1024;
/// 可接受的上限（比目标大 30%）。
pub const ACCEPT_HIGH: u64 = 65 * 1024;
/// 绝对不允许超过的上限。
pub const HARD_MAX: u64 = 100 * 1024;

const QUALITY_PROBE: u8 = 72;
const QUALITY_MIN: u8 = 35;
const QUALITY_MAX: u8 = 95;

/// 正常情况下的最小短边；避免把图片压成看不清的缩略图。
const MIN_SHORT_EDGE: u32 = 200;
/// 兜底时的最小短边。
const FALLBACK_SHORT_EDGE: u32 = 96;

#[derive(Debug, Clone)]
pub struct CompressionResult {
    pub path: PathBuf,
    pub backup: Option<PathBuf>,
    pub original_bytes: u64,
    pub output_bytes: u64,
    pub original_width: u32,
    pub original_height: u32,
    pub output_width: u32,
    pub output_height: u32,
    /// 例如 "JPEG 质量 78"、"PNG 128 色"。
    pub note: String,
    /// 原图本来就在目标范围内，未修改任何文件。
    pub skipped: bool,
}

impl CompressionResult {
    pub fn saved_percent(&self) -> f64 {
        if self.original_bytes == 0 {
            return 0.0;
        }
        (1.0 - self.output_bytes as f64 / self.original_bytes as f64) * 100.0
    }
}

/// 压缩一张图片。
///
/// 成功压缩时：
/// 1. 原文件重命名为 `<名字>_bak.<扩展名>`（重名时自动加序号）；
/// 2. 压缩结果写到原文件名。
///
/// 如果原图已经不超过 65 KB，则直接返回 `skipped = true`，不改动任何文件。
pub fn compress_file(path: &Path) -> Result<CompressionResult, String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .ok_or_else(|| "文件没有扩展名，只支持 .png / .jpg / .jpeg".to_string())?;

    if !matches!(ext.as_str(), "png" | "jpg" | "jpeg") {
        return Err("只支持 PNG 和 JPG/JPEG 图片".to_string());
    }

    let original = fs::read(path).map_err(|e| format!("读取文件失败：{e}"))?;
    let original_bytes = original.len() as u64;

    let (img, format) = decode_image(&original)?;
    let (original_width, original_height) = img.dimensions();

    match (format, ext.as_str()) {
        (ImageFormat::Jpeg, "jpg" | "jpeg") | (ImageFormat::Png, "png") => {}
        _ => {
            return Err("文件扩展名与实际图片格式不一致".to_string());
        }
    }

    if original_bytes <= ACCEPT_HIGH {
        return Ok(CompressionResult {
            path: path.to_path_buf(),
            backup: None,
            original_bytes,
            output_bytes: original_bytes,
            original_width,
            original_height,
            output_width: original_width,
            output_height: original_height,
            note: "原图已在 65 KB 以内，无需压缩".to_string(),
            skipped: true,
        });
    }

    let (out, output_width, output_height, note) = match format {
        ImageFormat::Jpeg => compress_jpeg(&img)?,
        ImageFormat::Png => compress_png(&img)?,
        _ => unreachable!(),
    };

    if out.len() as u64 > HARD_MAX {
        return Err(format!(
            "压缩结果仍有 {} KB，超过 100 KB 上限，已取消写入",
            out.len() / 1024
        ));
    }

    let backup = backup_path(path);
    let tmp = temp_path(path);

    fs::write(&tmp, &out).map_err(|e| format!("写入临时文件失败：{e}"))?;

    if let Err(e) = fs::rename(path, &backup) {
        let _ = fs::remove_file(&tmp);
        return Err(format!("备份原图失败（文件可能被占用）：{e}"));
    }

    if let Err(e) = fs::rename(&tmp, path) {
        // 尽量恢复原文件，避免用户丢失图片。
        let _ = fs::rename(&backup, path);
        let _ = fs::remove_file(&tmp);
        return Err(format!("写入压缩结果失败：{e}"));
    }

    Ok(CompressionResult {
        path: path.to_path_buf(),
        backup: Some(backup),
        original_bytes,
        output_bytes: out.len() as u64,
        original_width,
        original_height,
        output_width,
        output_height,
        note,
        skipped: false,
    })
}

fn decode_image(bytes: &[u8]) -> Result<(DynamicImage, ImageFormat), String> {
    let reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| format!("读取图片失败：{e}"))?;

    let format = reader
        .format()
        .ok_or_else(|| "无法识别图片格式".to_string())?;

    if !matches!(format, ImageFormat::Jpeg | ImageFormat::Png) {
        return Err("只支持 PNG 和 JPG/JPEG 图片".to_string());
    }

    let mut decoder = reader
        .into_decoder()
        .map_err(|e| format!("解析图片失败：{e}"))?;

    let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    let mut img = DynamicImage::from_decoder(decoder).map_err(|e| format!("解码图片失败：{e}"))?;
    img.apply_orientation(orientation);

    Ok((img, format))
}

// ---------------------------------------------------------------------------
// JPEG
// ---------------------------------------------------------------------------

fn compress_jpeg(base: &DynamicImage) -> Result<(Vec<u8>, u32, u32, String), String> {
    let (width, height) = base.dimensions();
    let min_scale = min_scale_for(width, height, MIN_SHORT_EDGE);

    // 找到“质量 72 能压进 65 KB”的最大尺寸。
    let scale = find_scale(min_scale, |s| {
        encode_jpeg_at(base, s, QUALITY_PROBE)
            .map(|bytes| bytes.len() as u64 <= ACCEPT_HIGH)
            .unwrap_or(false)
    });

    let mut img = resize_dynamic(base, scale);
    let (mut bytes, mut quality) = best_quality_at(|q| encode_jpeg(&img, q))?
        .ok_or_else(|| "无法在目标体积内完成 JPEG 压缩".to_string())?;

    // 极少见的兜底：继续缩小，直到满足可接受上限。
    let mut current_scale = scale;
    while bytes.len() as u64 > ACCEPT_HIGH && current_scale > 0.03 {
        current_scale *= 0.8;
        img = resize_dynamic(base, current_scale);
        if let Some((b, q)) = best_quality_at(|q| encode_jpeg(&img, q))? {
            bytes = b;
            quality = q;
        }
    }

    // 如果压出来远小于目标，说明还有体积预算，可以换更大的分辨率。
    if (bytes.len() as u64) < ACCEPT_LOW && current_scale < 0.999 {
        let mut s = current_scale;
        for _ in 0..3 {
            s = (s * 1.15).min(1.0);
            if s <= current_scale + 0.005 {
                break;
            }
            let bigger = resize_dynamic(base, s);
            match best_quality_at(|q| encode_jpeg(&bigger, q))? {
                Some((b, q)) if q >= 50 && b.len() as u64 > bytes.len() as u64 => {
                    bytes = b;
                    quality = q;
                    current_scale = s;
                    img = bigger;
                    if bytes.len() as u64 >= ACCEPT_LOW {
                        break;
                    }
                }
                _ => break,
            }
        }
    }

    if bytes.len() as u64 > HARD_MAX {
        return Err(format!(
            "压缩结果仍有 {} KB，超过 100 KB 上限",
            bytes.len() / 1024
        ));
    }

    let (width, height) = img.dimensions();
    Ok((bytes, width, height, format!("JPEG 质量 {quality}")))
}

fn encode_jpeg_at(base: &DynamicImage, scale: f64, quality: u8) -> Result<Vec<u8>, String> {
    let img = resize_dynamic(base, scale);
    encode_jpeg(&img, quality)
}

fn encode_jpeg(img: &DynamicImage, quality: u8) -> Result<Vec<u8>, String> {
    let (width, height) = img.dimensions();
    if width == 0 || height == 0 {
        return Err("图片尺寸为空".to_string());
    }
    if width > u16::MAX as u32 || height > u16::MAX as u32 {
        return Err("图片尺寸超出 JPEG 编码器限制".to_string());
    }

    let mut out = Vec::new();
    let mut encoder = JpegEncoder::new(&mut out, quality);
    // 4:2:0 色度抽样 + 渐进式，在相同观感下体积更小。
    encoder.set_sampling_factor(SamplingFactor::R_4_2_0);
    encoder.set_progressive(true);

    let result = match img {
        DynamicImage::ImageLuma8(gray) => encoder.encode(
            gray.as_raw(),
            width as u16,
            height as u16,
            JpegColorType::Luma,
        ),
        DynamicImage::ImageLumaA8(_) => {
            let gray = img.to_luma8();
            encoder.encode(
                gray.as_raw(),
                width as u16,
                height as u16,
                JpegColorType::Luma,
            )
        }
        _ => {
            let rgb = img.to_rgb8();
            encoder.encode(
                rgb.as_raw(),
                width as u16,
                height as u16,
                JpegColorType::Rgb,
            )
        }
    };

    result.map_err(|e| format!("JPEG 编码失败：{e}"))?;
    Ok(out)
}

// ---------------------------------------------------------------------------
// PNG
// ---------------------------------------------------------------------------

fn compress_png(base: &DynamicImage) -> Result<(Vec<u8>, u32, u32, String), String> {
    let (width, height) = base.dimensions();
    let base_rgba = base.to_rgba8();
    let min_scale = min_scale_for(width, height, MIN_SHORT_EDGE);

    // 找到“256 色 PNG 能压进 65 KB”的最大尺寸。
    let scale = find_scale(min_scale, |s| {
        encode_png_at(&base_rgba, s, 256)
            .map(|(bytes, _)| bytes.len() as u64 <= ACCEPT_HIGH)
            .unwrap_or(false)
    });

    let mut rgba = resize_rgba(&base_rgba, scale, 1);
    let (mut bytes, mut used_colors) = encode_png_palette(&rgba, 256)?;

    if bytes.len() as u64 > ACCEPT_HIGH {
        // 尺寸已经是合适的，尝试减少颜色数。
        let mut chosen = None;
        for colors in [256usize, 192, 128, 96, 64, 48, 32, 24, 16, 12, 8, 6, 4, 2] {
            if colors > 256 {
                continue;
            }
            let (candidate, used) = encode_png_palette(&rgba, colors)?;
            if candidate.len() as u64 <= ACCEPT_HIGH {
                chosen = Some((candidate, used));
                break;
            }
        }
        if let Some((candidate, used)) = chosen {
            bytes = candidate;
            used_colors = used;
        }
    }

    // 极端兜底：继续缩小，直到回到可接受上限。
    let mut current_scale = scale;
    while bytes.len() as u64 > ACCEPT_HIGH && current_scale > 0.03 {
        current_scale *= 0.75;
        rgba = resize_rgba(&base_rgba, current_scale, FALLBACK_SHORT_EDGE);
        let (candidate, used) = encode_png_palette(&rgba, 64)?;
        bytes = candidate;
        used_colors = used;
    }

    if bytes.len() as u64 > HARD_MAX {
        return Err(format!(
            "压缩结果仍有 {} KB，超过 100 KB 上限",
            bytes.len() / 1024
        ));
    }

    let (width, height) = rgba.dimensions();
    Ok((bytes, width, height, format!("PNG {used_colors} 色")))
}

fn encode_png_at(base: &RgbaImage, scale: f64, colors: usize) -> Result<(Vec<u8>, usize), String> {
    let rgba = resize_rgba(base, scale, 1);
    encode_png_palette(&rgba, colors)
}

/// 把 RGBA 图片量化成 8-bit 调色板 PNG。
///
/// 返回 `(PNG 字节, 实际使用的颜色数)`。
fn encode_png_palette(img: &RgbaImage, colors: usize) -> Result<(Vec<u8>, usize), String> {
    let (width, height) = img.dimensions();
    if width == 0 || height == 0 {
        return Err("图片尺寸为空".to_string());
    }

    let pixel_count = width as usize * height as usize;
    let colors = colors.clamp(2, 256).min(pixel_count.max(2));

    // 小图用更密的采样，大图适当采样以控制耗时。
    let sample_factor = if pixel_count > 400_000 { 10 } else { 1 };
    let quantizer = NeuQuant::new(sample_factor, colors, img.as_raw());

    // 直接按调色板量化每个像素，避免依赖 image 内部的 color_quant 版本。
    let mut indices = Vec::with_capacity(pixel_count);
    for pixel in img.pixels() {
        indices.push(quantizer.index_of(&pixel.0) as u8);
    }

    let max_index = indices.iter().copied().max().unwrap_or(0) as usize;
    let used_colors = (max_index + 1).min(colors).max(1);

    let map = quantizer.color_map_rgba();
    let mut palette = Vec::with_capacity(used_colors * 3);
    let mut transparency = Vec::with_capacity(used_colors);
    let mut has_alpha = false;

    for i in 0..used_colors {
        let offset = i * 4;
        palette.extend_from_slice(&map[offset..offset + 3]);
        let alpha = map[offset + 3];
        transparency.push(alpha);
        if alpha < 255 {
            has_alpha = true;
        }
    }

    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(png::ColorType::Indexed);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_palette(palette);
        if has_alpha {
            encoder.set_trns(transparency);
        }
        encoder.set_compression(png::Compression::High);

        let mut writer = encoder
            .write_header()
            .map_err(|e| format!("PNG 编码失败：{e}"))?;
        writer
            .write_image_data(&indices)
            .map_err(|e| format!("PNG 编码失败：{e}"))?;
    }

    Ok((out, used_colors))
}

// ---------------------------------------------------------------------------
// 通用工具
// ---------------------------------------------------------------------------

/// 二分搜索最大的可用缩放比例。
fn find_scale<F>(min_scale: f64, mut fits: F) -> f64
where
    F: FnMut(f64) -> bool,
{
    let min_scale = min_scale.clamp(0.02, 1.0);
    if fits(1.0) {
        return 1.0;
    }
    if !fits(min_scale) {
        return min_scale;
    }

    let mut low = min_scale;
    let mut high = 1.0;
    for _ in 0..7 {
        let mid = (low + high) / 2.0;
        if fits(mid) {
            low = mid;
        } else {
            high = mid;
        }
    }
    low
}

/// 在固定尺寸下，二分搜索能压进 65 KB 的最高质量。
fn best_quality_at<F>(mut encode: F) -> Result<Option<(Vec<u8>, u8)>, String>
where
    F: FnMut(u8) -> Result<Vec<u8>, String>,
{
    let highest = encode(QUALITY_MAX)?;
    if highest.len() as u64 <= ACCEPT_HIGH {
        return Ok(Some((highest, QUALITY_MAX)));
    }

    let lowest = encode(QUALITY_MIN)?;
    if lowest.len() as u64 > ACCEPT_HIGH {
        return Ok(None);
    }

    let mut low = QUALITY_MIN;
    let mut high = QUALITY_MAX;
    let mut best = lowest;

    while low + 1 < high {
        let mid = (low + high) / 2;
        let candidate = encode(mid)?;
        if candidate.len() as u64 <= ACCEPT_HIGH {
            low = mid;
            best = candidate;
        } else {
            high = mid;
        }
    }

    Ok(Some((best, low)))
}

fn min_scale_for(width: u32, height: u32, min_short_edge: u32) -> f64 {
    let short_edge = width.min(height).max(1);
    (min_short_edge as f64 / short_edge as f64).clamp(0.02, 1.0)
}

fn resize_dynamic(base: &DynamicImage, scale: f64) -> DynamicImage {
    let (width, height) = base.dimensions();
    let max_dimension_scale = (65_000.0 / width.max(height).max(1) as f64).min(1.0);
    let scale = scale.clamp(0.01, max_dimension_scale);
    let new_width = ((width as f64 * scale).round() as u32).max(1);
    let new_height = ((height as f64 * scale).round() as u32).max(1);

    if new_width == width && new_height == height {
        base.clone()
    } else {
        base.resize_exact(new_width, new_height, FilterType::Lanczos3)
    }
}

fn resize_rgba(base: &RgbaImage, scale: f64, min_short_edge: u32) -> RgbaImage {
    let (width, height) = base.dimensions();
    let short_edge = width.min(height).max(1);
    let floor_scale = if scale < 0.25 {
        (min_short_edge as f64 / short_edge as f64).min(1.0)
    } else {
        0.0
    };
    let scale = scale.clamp(floor_scale.max(0.01), 1.0);
    let new_width = ((width as f64 * scale).round() as u32).max(1);
    let new_height = ((height as f64 * scale).round() as u32).max(1);

    if new_width == width && new_height == height {
        return base.clone();
    }

    // 先预乘 alpha 再缩放，避免透明背景的 RGB 在插值时产生黑边。
    let mut premultiplied = RgbaImage::new(width, height);
    for (source, target) in base.pixels().zip(premultiplied.pixels_mut()) {
        let alpha = source[3] as u32;
        target[0] = ((source[0] as u32 * alpha + 127) / 255) as u8;
        target[1] = ((source[1] as u32 * alpha + 127) / 255) as u8;
        target[2] = ((source[2] as u32 * alpha + 127) / 255) as u8;
        target[3] = source[3];
    }

    let mut resized = imageops::resize(&premultiplied, new_width, new_height, FilterType::Lanczos3);

    for pixel in resized.pixels_mut() {
        let alpha = pixel[3] as u32;
        if alpha == 0 {
            pixel[0] = 0;
            pixel[1] = 0;
            pixel[2] = 0;
        } else {
            for channel in 0..3 {
                let value = (pixel[channel] as u32).min(alpha);
                pixel[channel] = ((value * 255 + alpha / 2) / alpha).min(255) as u8;
            }
        }
    }

    resized
}

fn backup_path(path: &Path) -> PathBuf {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "image".to_string());
    let ext = path.extension().map(|e| e.to_string_lossy().to_string());

    for n in 0..10_000 {
        let name = if n == 0 {
            format!("{stem}_bak")
        } else {
            format!("{stem}_bak_{}", n + 1)
        };
        let candidate = match &ext {
            Some(ext) => parent.join(format!("{name}.{ext}")),
            None => parent.join(name),
        };
        if !candidate.exists() {
            return candidate;
        }
    }

    parent.join(format!("{stem}_bak_{}", std::process::id()))
}

fn temp_path(path: &Path) -> PathBuf {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "image".to_string());

    for n in 0..10_000 {
        let candidate = parent.join(format!(".{stem}.tinyimage.{}.{n}.tmp", std::process::id()));
        if !candidate.exists() {
            return candidate;
        }
    }

    parent.join(format!(".{stem}.tinyimage.tmp"))
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb, RgbImage, Rgba, RgbaImage};

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("tinyimage_test_{}_{}", std::process::id(), name));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn noisy_rgb(width: u32, height: u32) -> RgbImage {
        let mut img = RgbImage::new(width, height);
        let mut seed = 0x1234_5678u32;
        for pixel in img.pixels_mut() {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let r = (seed >> 16) as u8;
            let g = (seed >> 8) as u8;
            let b = seed as u8;
            *pixel = Rgb([r, g, b]);
        }
        img
    }

    fn write_truecolor_png(path: &Path, img: &RgbaImage) {
        let mut out = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut out, img.width(), img.height());
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(img.as_raw()).unwrap();
        }
        fs::write(path, out).unwrap();
    }

    #[test]
    fn jpeg_reaches_target_and_keeps_backup() {
        let dir = temp_dir("jpg");
        let path = dir.join("photo.jpg");

        let source = DynamicImage::ImageRgb8(noisy_rgb(1800, 1200));
        let bytes = encode_jpeg(&source, 95).unwrap();
        assert!(bytes.len() as u64 > ACCEPT_HIGH);
        fs::write(&path, &bytes).unwrap();

        let result = compress_file(&path).unwrap();
        println!(
            "jpg: {} -> {} bytes, {}x{} -> {}x{}, {}",
            result.original_bytes,
            result.output_bytes,
            result.original_width,
            result.original_height,
            result.output_width,
            result.output_height,
            result.note
        );
        assert!(!result.skipped);
        assert!(
            result.output_bytes <= ACCEPT_HIGH,
            "output={}",
            result.output_bytes
        );
        assert!(result.output_bytes < result.original_bytes);
        let backup = result.backup.as_ref().unwrap();
        assert!(backup.exists());
        assert_eq!(backup.file_name().unwrap(), "photo_bak.jpg");
        assert!(path.exists());
        assert!(result.output_width < result.original_width);

        let written = fs::read(&path).unwrap();
        assert_eq!(image::guess_format(&written).unwrap(), ImageFormat::Jpeg);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn png_stays_png_and_preserves_alpha() {
        let dir = temp_dir("png");
        let path = dir.join("logo.png");

        let mut img = RgbaImage::new(1600, 1100);
        let mut seed = 0x9E37_79B9u32;
        for (x, _y, pixel) in img.enumerate_pixels_mut() {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let r = (seed >> 16) as u8;
            let g = (seed >> 8) as u8;
            let b = seed as u8;
            let alpha = if x < 250 { 0 } else { 255 };
            *pixel = Rgba([r, g, b, alpha]);
        }
        write_truecolor_png(&path, &img);
        assert!(fs::metadata(&path).unwrap().len() > ACCEPT_HIGH);

        let result = compress_file(&path).unwrap();
        println!(
            "png: {} -> {} bytes, {}x{} -> {}x{}, {}",
            result.original_bytes,
            result.output_bytes,
            result.original_width,
            result.original_height,
            result.output_width,
            result.output_height,
            result.note
        );
        assert!(!result.skipped);
        assert!(
            result.output_bytes <= ACCEPT_HIGH,
            "output={}",
            result.output_bytes
        );

        let written = fs::read(&path).unwrap();
        assert_eq!(image::guess_format(&written).unwrap(), ImageFormat::Png);

        let decoded = image::load_from_memory(&written).unwrap().to_rgba8();
        assert!(decoded.get_pixel(10, 10)[3] < 128);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn small_file_is_not_modified() {
        let dir = temp_dir("skip");
        let path = dir.join("small.jpg");

        let source = DynamicImage::ImageRgb8(noisy_rgb(320, 200));
        let bytes = encode_jpeg(&source, 55).unwrap();
        assert!(bytes.len() as u64 <= ACCEPT_HIGH);
        fs::write(&path, &bytes).unwrap();

        let result = compress_file(&path).unwrap();
        assert!(result.skipped);
        assert!(result.backup.is_none());
        assert_eq!(fs::read(&path).unwrap(), bytes);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn backup_name_gets_suffix_when_taken() {
        let dir = temp_dir("backup");
        let path = dir.join("photo.jpg");
        fs::write(&path, b"x").unwrap();
        fs::write(dir.join("photo_bak.jpg"), b"old").unwrap();

        let backup = backup_path(&path);
        assert_eq!(backup.file_name().unwrap(), "photo_bak_2.jpg");

        let _ = fs::remove_dir_all(&dir);
    }
}
