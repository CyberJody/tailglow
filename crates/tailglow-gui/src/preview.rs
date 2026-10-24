// SPDX-License-Identifier: GPL-3.0-or-later
//! Animated previews of the lighting effects, drawn with `canvas`.
//!
//! The drawings are impressions of what the M1's light bar does (as seen on
//! the webcam during session 1), not a simulation of the firmware. The
//! important part is that effect, color, brightness and speed all visibly
//! change the preview, so the user sees what a setting means before it is
//! written to the mouse.

use iced::mouse;
use iced::widget::canvas::{self, Frame, Geometry, Path, Stroke};
use iced::{Color, Point, Rectangle, Renderer, Size, Theme};

use tailglow_core::{Effect, Lighting};

/// One LED strip preview. `t` is the animation clock in seconds.
#[derive(Debug, Clone, Copy)]
pub struct Preview {
    pub lighting: Lighting,
    pub t: f32,
    /// Drawn with a mouse silhouette around the strip (large preview).
    pub silhouette: bool,
}

const SEGMENTS: usize = 12;

impl Preview {
    fn level(&self) -> f32 {
        // 0x0c..=0xff → 0.15..=1.0 so "minimum" still glows a little.
        let raw = self.lighting.brightness.raw() as f32;
        0.15 + 0.85 * ((raw - 12.0) / 243.0).clamp(0.0, 1.0)
    }

    /// Speed byte → cycles per second, from the webcam-measured periods in
    /// PROTOCOL.md §5 (breathing 7.25 s at 0x14 … 0.7 s at 0xff).
    fn rate(&self) -> f32 {
        match self.lighting.speed.period_seconds(self.lighting.effect) {
            Some(p) if p > 0.05 => 1.0 / p,
            _ => 0.27,
        }
    }

    /// Color of segment `i` (0..SEGMENTS) at the current time.
    pub fn segment_color(&self, i: usize) -> Color {
        let l = &self.lighting;
        let base = rgb(l.color.r, l.color.g, l.color.b);
        let level = self.level();
        let phase = self.t * self.rate();
        let frac = i as f32 / SEGMENTS as f32;
        match l.effect {
            Effect::Off => Color::from_rgb(0.08, 0.08, 0.09),
            Effect::Static => scale(base, level),
            Effect::Breathing => {
                let breath = 0.5 + 0.5 * (phase * std::f32::consts::TAU).sin();
                scale(base, level * (0.08 + 0.92 * breath))
            }
            Effect::Rainbow => scale(hsv(phase.fract(), 1.0, 1.0), level),
            Effect::RainbowSwirl => scale(hsv((phase + frac).fract(), 1.0, 1.0), level),
            Effect::Snake => {
                let head = (phase * 2.0 * SEGMENTS as f32) % (SEGMENTS as f32 * 2.0);
                // Bounce back and forth along the strip.
                let head = if head < SEGMENTS as f32 {
                    head
                } else {
                    2.0 * SEGMENTS as f32 - head
                };
                let d = (i as f32 - head).abs();
                let glow = (1.0 - d / 3.5).clamp(0.0, 1.0);
                scale(base, level * (0.06 + 0.94 * glow * glow))
            }
            Effect::Christmas => {
                let flip = ((phase * 2.0) as usize) % 2;
                let red = Color::from_rgb(0.95, 0.1, 0.1);
                let green = Color::from_rgb(0.1, 0.85, 0.2);
                let c = if (i / 2 + flip).is_multiple_of(2) {
                    red
                } else {
                    green
                };
                let twinkle = 0.75 + 0.25 * ((phase * 7.0 + i as f32).sin());
                scale(c, level * twinkle)
            }
        }
    }
}

impl<Message> canvas::Program<Message> for Preview {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let size = frame.size();
        let dark = theme.extended_palette().is_dark;
        let body_color = if dark {
            Color::from_rgb(0.16, 0.16, 0.18)
        } else {
            Color::from_rgb(0.22, 0.22, 0.25)
        };
        let detail = Color::from_rgb(0.1, 0.1, 0.11);
        // Lit LEDs: on dark the halo is the glow; on light it is the colored
        // spill on a white desk, plus a hairline rim so white / yellow LEDs
        // keep an edge against the white panel.
        let (halo_outer, halo_inner) = (0.18, 0.35);
        let rim = Color::from_rgba(0.0, 0.0, 0.0, if dark { 0.0 } else { 0.35 });
        // Unlit LEDs (Lights Off): a gray ring on the body instead of a black
        // dot on a near-black body (invisible on both themes before).
        let off = self.lighting.effect == Effect::Off;
        let off_ring = Color::from_rgb(0.62, 0.63, 0.67);

        if self.silhouette {
            // Top-down mouse with an underglow strip around its lower rim.
            // Leave room below the body for the bottom LED row and its glow.
            // The canvas takes the page's spare height, so cap the body and
            // centre the drawing (body + bottom LED row + glow) vertically.
            let h = (size.height * 0.74).min(330.0);
            let w = (h * 0.6).min(size.width - 60.0);
            let x0 = (size.width - w) / 2.0;
            let r_led = (w * 0.045).max(3.0);
            let y0 = ((size.height - (h + 3.4 * r_led + 2.0)) / 2.0).max(6.0);
            let body = Path::rounded_rectangle(
                Point::new(x0, y0),
                Size::new(w, h),
                iced::border::Radius {
                    top_left: w * 0.5,
                    top_right: w * 0.5,
                    bottom_right: w * 0.42,
                    bottom_left: w * 0.42,
                },
            );
            frame.fill(&body, body_color);
            let wheel = Path::rounded_rectangle(
                Point::new(x0 + w / 2.0 - 4.0, y0 + h * 0.12),
                Size::new(8.0, h * 0.16),
                4.0.into(),
            );
            frame.fill(&wheel, detail);
            let split = Path::rectangle(
                Point::new(x0 + w / 2.0 - 0.5, y0 + 2.0),
                Size::new(1.0, h * 0.36),
            );
            frame.fill(&split, detail);
            let seam = Path::rectangle(
                Point::new(x0 + 6.0, y0 + h * 0.38),
                Size::new(w - 12.0, 1.0),
            );
            frame.fill(&seam, detail);

            // LEDs: SEGMENTS points along the left edge, bottom and right edge.
            let per_side = SEGMENTS / 3;
            let r = r_led;
            for i in 0..SEGMENTS {
                let c = self.segment_color(i);
                let (x, y) = if i < per_side {
                    // down the left edge
                    let f = (i as f32 + 0.5) / per_side as f32;
                    (x0 - r - 2.0, y0 + h * (0.45 + 0.5 * f))
                } else if i < 2 * per_side {
                    // along the bottom, left to right
                    let f = ((i - per_side) as f32 + 0.5) / per_side as f32;
                    (x0 + w * f, y0 + h + r + 2.0)
                } else {
                    // up the right edge
                    let f = ((i - 2 * per_side) as f32 + 0.5) / per_side as f32;
                    (x0 + w + r + 2.0, y0 + h * (0.95 - 0.5 * f))
                };
                let p = Point::new(x, y);
                if off {
                    frame.fill(&Path::circle(p, r), body_color);
                    frame.stroke(
                        &Path::circle(p, r),
                        Stroke::default().with_width(1.5).with_color(off_ring),
                    );
                    continue;
                }
                if dark {
                    frame.fill(&Path::circle(p, r * 2.4), Color { a: halo_outer, ..c });
                    frame.fill(&Path::circle(p, r * 1.6), Color { a: halo_inner, ..c });
                } else {
                    // Light: stacked faint rings = a soft falloff (two flat
                    // discs looked like pastel stickers on white).
                    const RINGS: usize = 6;
                    for k in (1..=RINGS).rev() {
                        let radius = r * (1.0 + 1.7 * k as f32 / RINGS as f32);
                        frame.fill(&Path::circle(p, radius), Color { a: 0.075, ..c });
                    }
                }
                frame.fill(&Path::circle(p, r), c);
                if rim.a > 0.0 {
                    frame.stroke(
                        &Path::circle(p, r),
                        Stroke::default().with_width(1.0).with_color(rim),
                    );
                }
            }
            return vec![frame.into_geometry()];
        }

        // Small card: a rounded plate with the strip along the bottom.
        let margin = 4.0;
        let body = Path::rounded_rectangle(
            Point::new(margin, margin),
            Size::new(size.width - 2.0 * margin, size.height - 2.0 * margin),
            (size.height * 0.35).into(),
        );
        frame.fill(&body, body_color);

        // LED strip along the bottom edge: SEGMENTS pills with a soft glow.
        let strip_y = size.height * 0.72;
        let strip_h = size.height * 0.14;
        let inner_w = size.width - 2.0 * margin - 2.0 * strip_h;
        let seg_w = inner_w / SEGMENTS as f32;
        for i in 0..SEGMENTS {
            let c = self.segment_color(i);
            let x = margin + strip_h + i as f32 * seg_w;
            if off {
                let pill = Path::rounded_rectangle(
                    Point::new(x + 1.5, strip_y + 0.5),
                    Size::new(seg_w - 3.0, strip_h - 1.0),
                    (strip_h / 2.0).into(),
                );
                frame.stroke(
                    &pill,
                    Stroke::default().with_width(1.5).with_color(off_ring),
                );
                continue;
            }
            let glow = Path::rounded_rectangle(
                Point::new(x - 2.0, strip_y - 3.0),
                Size::new(seg_w + 4.0, strip_h + 6.0),
                (strip_h / 2.0 + 3.0).into(),
            );
            frame.fill(&glow, Color { a: 0.35, ..c });
            let pill = Path::rounded_rectangle(
                Point::new(x + 1.0, strip_y),
                Size::new(seg_w - 2.0, strip_h),
                (strip_h / 2.0).into(),
            );
            frame.fill(&pill, c);
        }
        vec![frame.into_geometry()]
    }
}

fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::from_rgb8(r, g, b)
}

fn scale(c: Color, k: f32) -> Color {
    Color::from_rgb(c.r * k, c.g * k, c.b * k)
}

/// HSV (all 0..1) → RGB.
pub fn hsv(h: f32, s: f32, v: f32) -> Color {
    let h6 = (h.rem_euclid(1.0)) * 6.0;
    let i = h6.floor() as i32;
    let f = h6 - i as f32;
    let p = v * (1.0 - s);
    let q = v * (1.0 - s * f);
    let t = v * (1.0 - s * (1.0 - f));
    let (r, g, b) = match i.rem_euclid(6) {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    Color::from_rgb(r, g, b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tailglow_core::{Brightness, Rgb, Speed};

    fn preview(effect: Effect, t: f32) -> Preview {
        Preview {
            lighting: Lighting {
                effect,
                brightness: Brightness::MAX,
                speed: Speed::OBSERVED,
                color: Rgb::new(255, 0, 0),
            },
            t,
            silhouette: false,
        }
    }

    #[test]
    fn static_is_the_chosen_color_and_off_is_dark() {
        let c = preview(Effect::Static, 0.0).segment_color(0);
        assert!(c.r > 0.9 && c.g < 0.05 && c.b < 0.05);
        let off = preview(Effect::Off, 1.0).segment_color(5);
        assert!(off.r < 0.1 && off.g < 0.1 && off.b < 0.1);
    }

    #[test]
    fn animated_effects_change_over_time_and_brightness_dims() {
        for e in [
            Effect::Breathing,
            Effect::Rainbow,
            Effect::RainbowSwirl,
            Effect::Snake,
            Effect::Christmas,
        ] {
            let a = preview(e, 0.0).segment_color(3);
            let b = preview(e, 0.3).segment_color(3);
            assert!(
                (a.r - b.r).abs() + (a.g - b.g).abs() + (a.b - b.b).abs() > 0.05,
                "{e:?} should animate"
            );
        }
        let mut dim = preview(Effect::Static, 0.0);
        dim.lighting.brightness = Brightness::MIN;
        assert!(dim.segment_color(0).r < 0.3);
    }

    #[test]
    fn hsv_primaries() {
        let r = hsv(0.0, 1.0, 1.0);
        assert!((r.r - 1.0).abs() < 1e-6 && r.g.abs() < 1e-6);
        let g = hsv(1.0 / 3.0, 1.0, 1.0);
        assert!((g.g - 1.0).abs() < 1e-6 && g.r.abs() < 1e-5);
        let b = hsv(2.0 / 3.0, 1.0, 1.0);
        assert!((b.b - 1.0).abs() < 1e-6 && b.r.abs() < 1e-5);
    }
}
