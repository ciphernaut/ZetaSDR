//! Video frames for the websocket: a box-filter downscaler, a PNG encoder and the three
//! delivery profiles. No raw frame goes over the wire; every video body is a PNG.

use zeta_core::stream::VideoFormat;

/// A delivery profile: the width a frame is scaled down to (never up) and the shortest
/// interval between frames sent.
#[derive(Debug, PartialEq, Eq)]
pub struct VideoProfile {
    pub name: &'static str,
    /// `None` keeps the frame's own width.
    pub width: Option<u32>,
    pub min_interval_ms: u64,
}

pub const SMOOTH: VideoProfile = VideoProfile {
    name: "smooth",
    width: Some(360),
    min_interval_ms: 0,
};
pub const SHARP: VideoProfile = VideoProfile {
    name: "sharp",
    width: None,
    min_interval_ms: 200,
};
pub const THUMB: VideoProfile = VideoProfile {
    name: "thumb",
    width: Some(120),
    min_interval_ms: 500,
};

/// The profile called `name`: `smooth`, `sharp` or `thumb`.
pub fn profile(name: &str) -> Option<&'static VideoProfile> {
    [&SMOOTH, &SHARP, &THUMB]
        .into_iter()
        .find(|p| p.name == name)
}

/// Rec. 601 luma of packed RGB, one byte per pixel.
pub fn gray8_of_rgb8(rgb: &[u8]) -> Vec<u8> {
    rgb.chunks_exact(3)
        .map(|p| {
            let y = 299 * u32::from(p[0]) + 587 * u32::from(p[1]) + 114 * u32::from(p[2]);
            ((y + 500) / 1000) as u8
        })
        .collect()
}

/// Box-filter downscale to `out_width`, keeping the aspect: `out_height` is
/// `max(1, height * out_width / width)`. `channels` is 1 (gray8) or 3 (rgb8). A target
/// at least as wide as the source returns it unchanged.
pub fn scale_to_width(
    src: &[u8],
    width: u32,
    height: u32,
    channels: u8,
    out_width: u32,
) -> (Vec<u8>, u32, u32) {
    if out_width >= width || width == 0 || height == 0 || out_width == 0 {
        return (src.to_vec(), width, height);
    }
    let ch = usize::from(channels);
    let out_height = ((u64::from(height) * u64::from(out_width) / u64::from(width)) as u32).max(1);
    let (w, h, ow, oh) = (
        width as usize,
        height as usize,
        out_width as usize,
        out_height as usize,
    );
    let mut out = Vec::with_capacity(ow * oh * ch);
    let mut sum = vec![0u32; ch];
    for oy in 0..oh {
        let (y0, y1) = (
            oy * h / oh,
            ((oy + 1) * h).div_ceil(oh).max(oy * h / oh + 1),
        );
        for ox in 0..ow {
            let (x0, x1) = (
                ox * w / ow,
                ((ox + 1) * w).div_ceil(ow).max(ox * w / ow + 1),
            );
            sum.fill(0);
            for y in y0..y1.min(h) {
                let row = &src[(y * w) * ch..((y + 1) * w) * ch];
                for x in x0..x1.min(w) {
                    for (c, s) in sum.iter_mut().enumerate() {
                        *s += u32::from(row[x * ch + c]);
                    }
                }
            }
            let n = ((y1.min(h) - y0) * (x1.min(w) - x0)) as u32;
            out.extend(sum.iter().map(|&s| ((s + n / 2) / n) as u8));
        }
    }
    (out, out_width, out_height)
}

/// Encode 8-bit gray (`channels` 1) or RGB (3) as a PNG, fast compression.
pub fn encode_png(bytes: &[u8], width: u32, height: u32, channels: u8) -> Result<Vec<u8>, String> {
    let color = match channels {
        1 => png::ColorType::Grayscale,
        3 => png::ColorType::Rgb,
        n => return Err(format!("{n} channels: expected 1 or 3")),
    };
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, width, height);
    enc.set_color(color);
    enc.set_depth(png::BitDepth::Eight);
    enc.set_compression(png::Compression::Fast);
    let mut w = enc.write_header().map_err(|e| e.to_string())?;
    w.write_image_data(bytes).map_err(|e| e.to_string())?;
    w.finish().map_err(|e| e.to_string())?;
    Ok(out)
}

/// A frame as a PNG body for `profile`: a png input passes through unscaled; gray8 and
/// rgb8 are scaled to the profile's width (when smaller) and encoded. Returns the body and
/// its dimensions.
pub fn encode_frame(
    width: u32,
    height: u32,
    format: VideoFormat,
    body: &[u8],
    profile: &VideoProfile,
) -> Result<(Vec<u8>, u32, u32), String> {
    format.check_body(width, height, body.len())?;
    let channels = match format {
        VideoFormat::Png => return Ok((body.to_vec(), width, height)),
        VideoFormat::Gray8 => 1,
        VideoFormat::Rgb8 => 3,
    };
    let (px, w, h) = match profile.width {
        Some(ow) => scale_to_width(body, width, height, channels, ow),
        None => (body.to_vec(), width, height),
    };
    Ok((encode_png(&px, w, h, channels)?, w, h))
}
