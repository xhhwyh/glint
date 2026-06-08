use std::f32::consts::PI;

use ratatui::{
    style::{Color, Style},
    text::Span,
};

pub const STAR_WIDTH: usize = 25;
pub const STAR_HEIGHT: usize = 14;

#[derive(Clone, Copy)]
struct Pt {
    x: f32,
    y: f32,
}

pub fn glint_star_rows() -> Vec<Vec<Span<'static>>> {
    let cx = STAR_WIDTH as f32 / 2.0;
    let cy = STAR_HEIGHT as f32 / 2.0 + 0.5;

    let outer = 5.2;
    let inner = 2.40;
    let poly = star_vertices(outer, inner);

    (0..STAR_HEIGHT)
        .map(|row| star_row(row, cx, cy, outer, &poly))
        .collect()
}

fn star_vertices(outer: f32, inner: f32) -> Vec<Pt> {
    let mut v = Vec::new();
    for i in 0..10 {
        let r = if i % 2 == 0 { outer } else { inner };
        let a = -PI / 2.0 + i as f32 * PI / 5.0;
        v.push(Pt {
            x: r * a.cos(),
            y: r * a.sin(),
        });
    }
    v
}

fn star_row(row: usize, cx: f32, cy: f32, outer: f32, poly: &[Pt]) -> Vec<Span<'static>> {
    let mut spans = Vec::new();

    for col in 0..STAR_WIDTH {
        // Fine-tune horizontal compression to preserve a solid five-point shape in a narrow canvas.
        let x = (col as f32 - cx) * 0.44;
        let y = row as f32 - cy;
        let p = Pt { x, y };

        let inside = inside_polygon(p, poly);
        let d = distance_to_polygon(p, poly);

        // Compact offset for the lower-right 3D shadow.
        let shadow_p = Pt {
            x: x - 0.55,
            y: y - 0.42,
        };
        let shadow = inside_polygon(shadow_p, poly);

        if inside {
            // Upper-left light source.
            let light_dir = (-0.75 * x - 0.95 * y) / outer;
            // Smooth the lighting by raising the base brightness and reducing contrast.
            let mut shade = 0.66 + 0.16 * light_dir;

            // Local highlight adjustments relative to the star radius.
            if y < -0.1 * outer && x.abs() < 0.18 * outer {
                shade += 0.18;
            }
            if x < -0.07 * outer && y < 0.14 * outer {
                shade += 0.08;
            }
            if x > 0.21 * outer && y > -0.07 * outer {
                shade -= 0.10; // Reduce the dark-side penalty to preserve detail.
            }
            if y > 0.4 * outer {
                shade -= 0.12;
            }

            let edge = d < 0.45;
            if edge {
                shade -= 0.10; // Keep edge transitions soft.
            }

            shade = shade.clamp(0.25, 1.0); // Raise the minimum brightness floor.

            // Blue color ramp: blue-green base plus bright ice-blue highlights.
            let r = (55.0 + 65.0 * shade) as u8;
            let g = (140.0 + 90.0 * shade) as u8;
            let b = (215.0 + 40.0 * shade) as u8;
            let ch = pick_char(shade, edge);

            spans.push(Span::styled(
                ch.to_string(),
                Style::default().fg(Color::Rgb(r, g, b)),
            ));
        } else if shadow && d < 1.4 {
            // Brighter mid-blue shadow keeps the lower-right 3D depth visible.
            let ch = match d {
                x if x < 0.5 => '#',
                x if x < 1.0 => '=',
                _ => '.',
            };
            spans.push(Span::styled(
                ch.to_string(),
                Style::default().fg(Color::Rgb(40, 90, 160)),
            ));
        } else if d < 0.85 {
            // Subtle outer aurora-blue glow.
            let glow = (1.0 - d / 0.85).clamp(0.0, 1.0);
            let b = (110.0 + 85.0 * glow) as u8;
            let ch = if glow > 0.55 { '.' } else { '`' };
            spans.push(Span::styled(
                ch.to_string(),
                Style::default().fg(Color::Rgb(20, 60, b)),
            ));
        } else {
            spans.push(Span::raw(" "));
        }
    }

    spans
}

fn inside_polygon(p: Pt, poly: &[Pt]) -> bool {
    let mut inside = false;
    let n = poly.len();
    for i in 0..n {
        let a = poly[i];
        let b = poly[(i + 1) % n];
        if (a.y > p.y) != (b.y > p.y) {
            let x_intersect = (b.x - a.x) * (p.y - a.y) / (b.y - a.y + 1e-6) + a.x;
            if p.x < x_intersect {
                inside = !inside;
            }
        }
    }
    inside
}

fn dist_to_segment(p: Pt, a: Pt, b: Pt) -> f32 {
    let vx = b.x - a.x;
    let vy = b.y - a.y;
    let wx = p.x - a.x;
    let wy = p.y - a.y;
    let c1 = vx * wx + vy * wy;
    let c2 = vx * vx + vy * vy;
    let t = (c1 / (c2 + 1e-6)).clamp(0.0, 1.0);
    let px = a.x + t * vx;
    let py = a.y + t * vy;
    ((p.x - px).powi(2) + (p.y - py).powi(2)).sqrt()
}

fn distance_to_polygon(p: Pt, poly: &[Pt]) -> f32 {
    let mut d = f32::MAX;
    for i in 0..poly.len() {
        let a = poly[i];
        let b = poly[(i + 1) % poly.len()];
        d = d.min(dist_to_segment(p, a, b));
    }
    d
}

fn pick_char(v: f32, edge: bool) -> char {
    if edge {
        return match v {
            x if x < 0.35 => '.',
            x if x < 0.50 => ':',
            x if x < 0.65 => '=',
            _ => '#',
        };
    }
    match v {
        x if x < 0.22 => '`',
        x if x < 0.32 => '.',
        x if x < 0.42 => ':',
        x if x < 0.52 => '-',
        x if x < 0.65 => '=',
        x if x < 0.78 => '+',
        x if x < 0.88 => '*',
        _ => '#',
    }
}
