use std::time::Instant;

use zeta_core::stream::VideoFormat;
use zeta_vrt::video::{SMOOTH, THUMB, encode_frame, encode_png, gray8_of_rgb8, profile, scale_to};

fn gradient(w: u32, h: u32) -> Vec<u8> {
    (0..h)
        .flat_map(|y| (0..w).map(move |x| ((x * 255 / w.max(1) + y) % 256) as u8))
        .collect()
}

fn decode(png_bytes: &[u8]) -> (png::OutputInfo, Vec<u8>) {
    let mut r = png::Decoder::new(png_bytes).read_info().unwrap();
    let mut buf = vec![0; r.output_buffer_size()];
    let info = r.next_frame(&mut buf).unwrap();
    buf.truncate(info.buffer_size());
    (info, buf)
}

fn mean(b: &[u8]) -> f64 {
    b.iter().map(|&v| f64::from(v)).sum::<f64>() / b.len() as f64
}

#[test]
fn scaling_720_by_288_to_360_by_288_keeps_the_mean() {
    let src = gradient(720, 288);
    let (out, w, h) = scale_to(&src, 720, 288, 1, 360, 288);
    assert_eq!((w, h), (360, 288));
    assert_eq!(out.len(), 360 * 288);
    assert!((mean(&src) - mean(&out)).abs() < 1.0);
    let (same, w, h) = scale_to(&src, 720, 288, 1, 1000, 1000);
    assert_eq!((w, h, same), (720, 288, src));
}

#[test]
fn smooth_keeps_every_line_of_a_field_and_thumb_a_third() {
    let src = gradient(720, 288);
    let dims = |p| {
        let (png_bytes, w, h) = encode_frame(720, 288, VideoFormat::Gray8, &src, p).unwrap();
        let (info, _) = decode(&png_bytes);
        assert_eq!((info.width, info.height), (w, h));
        (w, h)
    };
    assert_eq!(dims(&SMOOTH), (360, 288));
    assert_eq!(dims(&THUMB), (120, 96));
    // NTSC's 240 lines.
    let ntsc = gradient(720, 240);
    let (_, w, h) = encode_frame(720, 240, VideoFormat::Gray8, &ntsc, &THUMB).unwrap();
    assert_eq!((w, h), (120, 80));
}

#[test]
fn png_of_a_gray8_field_decodes_back() {
    let src = gradient(64, 32);
    let png_bytes = encode_png(&src, 64, 32, 1).unwrap();
    assert_eq!(&png_bytes[..4], b"\x89PNG");
    let (info, back) = decode(&png_bytes);
    assert_eq!((info.width, info.height), (64, 32));
    assert_eq!(back, src);
}

#[test]
fn a_png_body_passes_through_unscaled() {
    let png_bytes = encode_png(&gradient(64, 32), 64, 32, 1).unwrap();
    let (out, w, h) = encode_frame(64, 32, VideoFormat::Png, &png_bytes, &SMOOTH).unwrap();
    assert_eq!((w, h), (64, 32));
    assert_eq!(out, png_bytes);
    let (out, w, h) = encode_frame(1000, 500, VideoFormat::Png, &png_bytes, &SMOOTH).unwrap();
    assert_eq!((w, h, out), (1000, 500, png_bytes));
}

#[test]
fn rgb8_scales_per_channel() {
    let mut rgb = Vec::new();
    for _ in 0..(8 * 4) {
        rgb.extend_from_slice(&[200, 100, 10]);
    }
    let (out, w, h) = scale_to(&rgb, 8, 4, 3, 4, 2);
    assert_eq!((w, h), (4, 2));
    assert!(out.chunks(3).all(|p| p == [200, 100, 10]));
    let g = gray8_of_rgb8(&[255, 255, 255, 0, 0, 0, 255, 0, 0]);
    assert_eq!(g, vec![255, 0, 76]);
    let (png_bytes, w, h) = encode_frame(8, 4, VideoFormat::Rgb8, &rgb, &SMOOTH).unwrap();
    assert_eq!((w, h), (8, 4));
    let (info, back) = decode(&png_bytes);
    assert_eq!(info.color_type, png::ColorType::Rgb);
    assert_eq!(back, rgb);
}

#[test]
fn smooth_encode_of_a_360_by_288_field_is_fast_and_small() {
    let src = gradient(360, 288);
    let t = Instant::now();
    let mut size = 0;
    for _ in 0..100 {
        size = encode_png(&src, 360, 288, 1).unwrap().len();
    }
    let ms = t.elapsed().as_secs_f64() * 10.0;
    eprintln!("smooth encode: {ms:.2} ms mean, {size} bytes");
    assert!(ms < 10.0, "mean {ms} ms");
    assert!(size < 60_000, "{size} bytes");
}

#[test]
fn profiles_by_name() {
    assert_eq!(profile("thumb").unwrap().width, Some(120));
    assert_eq!(profile("thumb").unwrap().height_div, 3);
    assert_eq!(profile("smooth").unwrap().height_div, 1);
    assert_eq!(profile("smooth").unwrap().min_interval_ms, 0);
    assert_eq!(profile("sharp").unwrap().width, None);
    assert!(profile("x").is_none());
}
