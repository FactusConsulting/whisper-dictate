use eframe::egui;

pub(super) fn app_icon() -> egui::IconData {
    const SIZE: u32 = 256;
    let mut rgba = vec![0; (SIZE * SIZE * 4) as usize];
    for y in 0..SIZE {
        for x in 0..SIZE {
            let idx = ((y * SIZE + x) * 4) as usize;
            if !inside_rounded_square(x as i32, y as i32, SIZE as i32, 60) {
                rgba[idx + 3] = 0;
                continue;
            }
            let t = (x + y) as f32 / ((SIZE - 1) * 2) as f32;
            rgba[idx] = 255;
            rgba[idx + 1] = (42.0 * (1.0 - t)) as u8;
            rgba[idx + 2] = (42.0 * (1.0 - t) + 16.0 * t) as u8;
            rgba[idx + 3] = 255;
        }
    }

    for (x, y, w, h, r) in [
        (56, 112, 16, 32, 8),
        (84, 92, 16, 72, 8),
        (112, 64, 16, 128, 8),
        (140, 84, 16, 88, 8),
        (168, 104, 16, 48, 8),
        (196, 118, 16, 20, 8),
    ] {
        fill_rounded_rect(&mut rgba, SIZE, x, y, w, h, r, [255, 255, 255, 255]);
    }

    egui::IconData {
        rgba,
        width: SIZE,
        height: SIZE,
    }
}

fn inside_rounded_square(x: i32, y: i32, size: i32, radius: i32) -> bool {
    inside_rounded_rect(x, y, 0, 0, size, size, radius)
}

fn fill_rounded_rect(
    rgba: &mut [u8],
    canvas: u32,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    radius: i32,
    color: [u8; 4],
) {
    for py in y..(y + height) {
        for px in x..(x + width) {
            if inside_rounded_rect(px, py, x, y, width, height, radius) {
                let idx = ((py as u32 * canvas + px as u32) * 4) as usize;
                rgba[idx..idx + 4].copy_from_slice(&color);
            }
        }
    }
}

fn inside_rounded_rect(
    px: i32,
    py: i32,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    radius: i32,
) -> bool {
    if width <= 0 || height <= 0 {
        return false;
    }
    let radius = radius.max(0).min((width - 1) / 2).min((height - 1) / 2);
    let left = x + radius;
    let right = x + width - radius - 1;
    let top = y + radius;
    let bottom = y + height - radius - 1;
    let cx = px.clamp(left, right);
    let cy = py.clamp(top, bottom);
    let dx = px - cx;
    let dy = py - cy;
    dx * dx + dy * dy <= radius * radius
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_icon_builds_valid_rgba_buffer() {
        let icon = app_icon();

        assert_eq!(icon.width, 256);
        assert_eq!(icon.height, 256);
        assert_eq!(icon.rgba.len(), (icon.width * icon.height * 4) as usize);
        assert!(icon
            .rgba
            .chunks_exact(4)
            .any(|px| px == [255, 255, 255, 255]));
    }

    #[test]
    fn rounded_rect_handles_radius_larger_than_half_width() {
        assert!(inside_rounded_rect(15, 72, 8, 56, 16, 32, 8));
    }

    #[test]
    fn rounded_rect_rejects_empty_dimensions_without_panicking() {
        assert!(!inside_rounded_rect(0, 0, 0, 0, 0, 16, 8));
        assert!(!inside_rounded_rect(0, 0, 0, 0, 16, 0, 8));
    }
}
