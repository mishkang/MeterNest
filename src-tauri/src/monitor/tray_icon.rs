// tray-icon.js: a rounded badge with a transparent M cutout, centered in an
// 18pt canvas. The same geometry drives the application icon.
const M_POINTS: [(f64, f64); 5] = [(5.0, 12.0), (5.0, 6.0), (9.0, 10.0), (13.0, 6.0), (13.0, 12.0)];

fn segments() -> Vec<(f64, f64, f64, f64, f64)> {
    let mut result = Vec::new();
    for index in 0..M_POINTS.len() - 1 {
        let (x, y) = M_POINTS[index];
        let (ex, ey) = M_POINTS[index + 1];
        let dx = ex - x;
        let dy = ey - y;
        result.push((x, y, dx, dy, dx * dx + dy * dy));
    }
    result
}

fn inside_badge(x: f64, y: f64) -> bool {
    let qx = (x - 9.0).abs() - 3.5;
    let qy = (y - 9.0).abs() - 3.5;
    let distance = hypot(qx.max(0.0), qy.max(0.0)) + qx.max(qy).min(0.0);
    let inside = distance <= 4.0;
    let cutout = segments().iter().any(|(sx, sy, dx, dy, length)| {
        let dot = (x - sx) * dx + (y - sy) * dy;
        let t = (dot / length).clamp(0.0, 1.0);
        let px = x - sx - t * dx;
        let py = y - sy - t * dy;
        px * px + py * py <= 0.85 * 0.85
    });
    inside && !cutout
}

fn hypot(a: f64, b: f64) -> f64 {
    (a * a + b * b).sqrt()
}

/// RGBA pixels for the M badge at 1x/2x/3x scale, 18pt logical size.
pub fn rasterize(scale: u32) -> Vec<u8> {
    assert!((1..=3).contains(&scale), "不支持的托盘图标倍率");
    let size = 18 * scale as usize;
    let mut pixels = vec![0u8; size * size * 4];
    for y in 0..size {
        for x in 0..size {
            let mut coverage = 0u32;
            for sy in 0..4 {
                for sx in 0..4 {
                    let px = (x as f64 + (sx as f64 + 0.5) / 4.0) * 18.0 / size as f64;
                    let py = (y as f64 + (sy as f64 + 0.5) / 4.0) * 18.0 / size as f64;
                    if inside_badge(px, py) {
                        coverage += 1;
                    }
                }
            }
            pixels[(y * size + x) * 4 + 3] = (255.0 * coverage as f64 / 16.0).round() as u8;
        }
    }
    pixels
}

/// Encode RGBA pixels as PNG bytes (for NSBitmapImageRep).
pub fn encode_png(pixels: &[u8], width: u32, height: u32) -> Vec<u8> {
    let mut buffer = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut buffer, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().expect("png header");
        writer.write_image_data(pixels).expect("png data");
    }
    buffer
}
