/// Renders the universal Markdown `M↓` badge icon into an RGBA8 buffer of `size × size` pixels.
///
/// Uses a 64×64 logical coordinate space with 4×4 supersampling (16 sub-pixel samples per pixel)
/// and premultiplied alpha compositing so edges stay crisp from 16×16 up to 256×256.
pub fn render_app_icon_rgba(size: usize) -> Vec<u8> {
    if size == 0 {
        return Vec::new();
    }

    const SS: usize = 4;
    const SAMPLES: f32 = (SS * SS) as f32;
    let mut rgba = vec![0u8; size * size * 4];
    let scale = 64.0 / (size as f32);

    for y in 0..size {
        for x in 0..size {
            let mut sum_r = 0.0_f32;
            let mut sum_g = 0.0_f32;
            let mut sum_b = 0.0_f32;
            let mut sum_a = 0.0_f32;

            for sy in 0..SS {
                let fy = (y as f32 + (sy as f32 + 0.5) / (SS as f32)) * scale;
                for sx in 0..SS {
                    let fx = (x as f32 + (sx as f32 + 0.5) / (SS as f32)) * scale;
                    let (pr, pg, pb, pa) = sample_icon_pixel(fx, fy);
                    sum_r += pr;
                    sum_g += pg;
                    sum_b += pb;
                    sum_a += pa;
                }
            }

            let avg_a = sum_a / SAMPLES;
            let idx = (y * size + x) * 4;
            if avg_a > 1e-6 {
                rgba[idx] = ((sum_r / SAMPLES) / avg_a * 255.0)
                    .round()
                    .clamp(0.0, 255.0) as u8;
                rgba[idx + 1] = ((sum_g / SAMPLES) / avg_a * 255.0)
                    .round()
                    .clamp(0.0, 255.0) as u8;
                rgba[idx + 2] = ((sum_b / SAMPLES) / avg_a * 255.0)
                    .round()
                    .clamp(0.0, 255.0) as u8;
                rgba[idx + 3] = (avg_a * 255.0).round().clamp(0.0, 255.0) as u8;
            }
        }
    }

    rgba
}

#[inline]
fn composite_premul(
    dst: (f32, f32, f32, f32),
    src_r: f32,
    src_g: f32,
    src_b: f32,
    src_a: f32,
) -> (f32, f32, f32, f32) {
    let inv = 1.0 - src_a;
    (
        src_r * src_a + dst.0 * inv,
        src_g * src_a + dst.1 * inv,
        src_b * src_a + dst.2 * inv,
        src_a + dst.3 * inv,
    )
}

#[inline]
fn badge_sdf(fx: f32, fy: f32) -> f32 {
    const MIN_C: f32 = 3.0;
    const MAX_C: f32 = 61.0;
    const RADIUS: f32 = 13.5;
    const CENTER: f32 = (MIN_C + MAX_C) * 0.5;
    const INNER_HALF: f32 = (MAX_C - MIN_C) * 0.5 - RADIUS;

    let qx = (fx - CENTER).abs() - INNER_HALF;
    let qy = (fy - CENTER).abs() - INNER_HALF;
    qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - RADIUS
}

#[inline]
fn sample_icon_pixel(fx: f32, fy: f32) -> (f32, f32, f32, f32) {
    let sdf = badge_sdf(fx, fy);
    if sdf > 0.0 {
        return (0.0, 0.0, 0.0, 0.0);
    }

    // Diagonal indigo-to-royal-blue gradient: #4F46E5 -> #2563EB -> #1E3A8A
    let span = 61.0 - 3.0;
    let t = (((fx - 3.0) * 0.35 + (fy - 3.0) * 0.65) / span).clamp(0.0, 1.0);
    let (bg_r, bg_g, bg_b) = if t < 0.5 {
        let u = t * 2.0;
        (
            (79.0 + (37.0 - 79.0) * u) / 255.0,
            (70.0 + (99.0 - 70.0) * u) / 255.0,
            (229.0 + (235.0 - 229.0) * u) / 255.0,
        )
    } else {
        let u = (t - 0.5) * 2.0;
        (
            (37.0 + (30.0 - 37.0) * u) / 255.0,
            (99.0 + (58.0 - 99.0) * u) / 255.0,
            (235.0 + (138.0 - 235.0) * u) / 255.0,
        )
    };

    let mut color = (bg_r, bg_g, bg_b, 1.0);

    // Subtle 1.25px translucent white inner border / top rim highlight
    if sdf >= -1.25 {
        let rim_fade = 1.0 - ((fy - 3.0) / span).clamp(0.0, 1.0);
        let rim_alpha = 0.10 + 0.18 * rim_fade;
        color = composite_premul(color, 1.0, 1.0, 1.0, rim_alpha);
    }

    // Subtle dark-indigo drop shadow (+1.5px Y offset) behind the M↓ glyphs
    if in_markdown_glyphs(fx, fy - 1.5) {
        color = composite_premul(color, 15.0 / 255.0, 23.0 / 255.0, 42.0 / 255.0, 0.28);
    }

    // Crisp white M↓ glyphs
    if in_markdown_glyphs(fx, fy) {
        color = composite_premul(color, 1.0, 1.0, 1.0, 1.0);
    }

    color
}

#[inline]
fn dist_to_segment(px: f32, py: f32, ax: f32, ay: f32, bx: f32, by: f32) -> f32 {
    let vx = bx - ax;
    let vy = by - ay;
    let len_sq = vx * vx + vy * vy;
    if len_sq <= 1e-6 {
        return (px - ax).hypot(py - ay);
    }
    let proj = (((px - ax) * vx + (py - ay) * vy) / len_sq).clamp(0.0, 1.0);
    let cx = ax + proj * vx;
    let cy = ay + proj * vy;
    (px - cx).hypot(py - cy)
}

#[inline]
fn point_in_triangle(
    p: (f32, f32),
    a: (f32, f32),
    b: (f32, f32),
    c: (f32, f32),
) -> bool {
    let (px, py) = p;
    let (ax, ay) = a;
    let (bx, by) = b;
    let (cx, cy) = c;
    let d1 = (px - bx) * (ay - by) - (ax - bx) * (py - by);
    let d2 = (px - cx) * (by - cy) - (bx - cx) * (py - cy);
    let d3 = (px - ax) * (cy - ay) - (cx - ax) * (py - ay);
    let has_neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let has_pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(has_neg && has_pos)
}

#[inline]
fn in_markdown_glyphs(fx: f32, fy: f32) -> bool {
    // Bold geometric 'M' monogram on the left (stroke radius 2.35 -> ~4.7px width)
    const M_RADIUS: f32 = 2.35;
    if dist_to_segment(fx, fy, 13.5, 43.0, 13.5, 21.0) <= M_RADIUS
        || dist_to_segment(fx, fy, 13.5, 21.0, 23.5, 34.0) <= M_RADIUS
        || dist_to_segment(fx, fy, 23.5, 34.0, 33.5, 21.0) <= M_RADIUS
        || dist_to_segment(fx, fy, 33.5, 21.0, 33.5, 43.0) <= M_RADIUS
    {
        return true;
    }

    // Crisp downward arrow '↓' on the right
    const ARROW_SHAFT_RADIUS: f32 = 2.25;
    if dist_to_segment(fx, fy, 47.0, 21.0, 47.0, 38.0) <= ARROW_SHAFT_RADIUS {
        return true;
    }

    point_in_triangle((fx, fy), (40.0, 33.5), (54.0, 33.5), (47.0, 43.5))
}

#[cfg(test)]
mod tests {
    use super::render_app_icon_rgba;

    #[test]
    fn renders_expected_sizes_and_transparent_corners() {
        assert!(render_app_icon_rgba(0).is_empty());

        for sz in [16usize, 32, 48, 64, 128, 256] {
            let rgba = render_app_icon_rgba(sz);
            assert_eq!(rgba.len(), sz * sz * 4, "size={sz}");

            for (cx, cy) in [(0, 0), (sz - 1, 0), (0, sz - 1), (sz - 1, sz - 1)] {
                let idx = (cy * sz + cx) * 4;
                assert_eq!(
                    rgba[idx + 3],
                    0,
                    "corner ({cx},{cy}) should be transparent for size={sz}"
                );
            }

            // Top-center inside badge should be opaque indigo/blue
            let ix = sz / 2;
            let iy = (8 * sz / 64).max(2);
            let idx = (iy * sz + ix) * 4;
            assert_eq!(rgba[idx + 3], 255, "badge interior opaque at size={sz}");
            assert!(
                rgba[idx + 2] > rgba[idx],
                "badge interior should be indigo/blue-dominant at size={sz}: rgb=({},{},{})",
                rgba[idx],
                rgba[idx + 1],
                rgba[idx + 2]
            );
        }
    }

    #[test]
    fn renders_crisp_white_m_and_down_arrow_regions() {
        for sz in [16usize, 32, 64] {
            let rgba = render_app_icon_rgba(sz);
            let mut m_white = 0usize;
            let mut arrow_white = 0usize;

            let m_max_x = (36 * sz) / 64;
            let arrow_min_x = (40 * sz) / 64;

            for y in 0..sz {
                for x in 0..sz {
                    let idx = (y * sz + x) * 4;
                    let is_white = rgba[idx] >= 230
                        && rgba[idx + 1] >= 230
                        && rgba[idx + 2] >= 230
                        && rgba[idx + 3] == 255;
                    if is_white {
                        if x <= m_max_x {
                            m_white += 1;
                        } else if x >= arrow_min_x {
                            arrow_white += 1;
                        }
                    }
                }
            }

            assert!(
                m_white > 0,
                "expected opaque white pixels in 'M' region at size={sz}"
            );
            assert!(
                arrow_white > 0,
                "expected opaque white pixels in '↓' region at size={sz}"
            );
        }
    }
}
