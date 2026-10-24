// SPDX-License-Identifier: GPL-3.0-or-later
//! The Lighting page's big preview: the 3D render of the white M1, with the
//! effect running on its real light zones.
//!
//! The render ships as layers per view (`assets/render/<view>/`, embedded
//! with `include_bytes!`, see the README there):
//! - `base_lights_off.png`: RGBA, transparent background, every LED dark;
//! - `zone_<z>.png`: 8-bit gray, the light zone `z` adds (emitter, bounce on
//!   the shell, pool on the desk, halo);
//! - `zone_<z>_direct.png`: 8-bit gray, coverage of the emitter itself.
//!
//! Per pixel, in linear light (the render project's reference compositor):
//!
//! ```text
//! surface = lin(bg) * (1 - A) + lin(base) * A
//! hide    = 1 - HIDE * max_z(direct_z * on_z)
//! light_z = lin(zone_z) + SPILL * (0.9 * blur(zone_z, 3.5 % W) + 1.6 * blur(zone_z, 9 % W))
//! out     = srgb(min(surface * hide + sum_z tint_z * light_z, 1))
//! ```
//!
//! `hide` keeps a red LED red where the milky light pipe (white when off)
//! is visible. `on_z` is the zone's intensity (0..1), so a breathing zone
//! fades its own off-state look smoothly instead of switching it. The spill
//! is the wide glow on the desk from the approved mock-up (`SPILL = 1.0`):
//! it is blurred once per view at startup, on a quarter-size grid padded
//! so the glow can spread past the render's own frame, never per frame.
//!
//! The picture fills the whole preview panel: the render is placed in the
//! middle and the glow spreads over the panel, fading out before its edge.
//! Everything that depends on the panel size and background is built once
//! into a [`Scene`]. A frame is then a multiply-add per lit zone and pixel
//! (zones are cropped to where they have light), one bilinear upsample of
//! the colored spill, and a table lookup with an ordered dither for the
//! sRGB encode (the dither keeps the faint outer glow free of banding).

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use iced::mouse;
use iced::widget::canvas::{self, Frame, Geometry};
use iced::widget::image::{FilterMethod, Handle};
use iced::{Color, Rectangle, Renderer, Theme};
use serde::{Deserialize, Serialize};

/// How much a lit emitter hides its own off-state look (render project: 0.85).
pub const HIDE: f32 = 0.85;
/// Glow spill amount; 1.0 is the approved look.
pub const SPILL: f32 = 1.0;
/// The two spill blurs: (sigma as a fraction of the image width, weight).
const SPILL_BLURS: [(f32, f32); 2] = [(0.035, 0.9), (0.09, 1.6)];
/// The spill is computed on a grid this many times smaller than the asset.
const SPILL_GRID: usize = 4;
/// Spill fades to zero over this fraction of the image at each edge, so the
/// picture has no visible rectangle on the panel.
const EDGE_FADE: f32 = 0.12;
/// The render's own layers (shell, shadow, zone light) fade over a much
/// narrower edge: the frames keep the mouse itself clear of it, so only
/// faint shadow and halo are touched.
const LAYER_FADE: f32 = 0.03;
/// The same for the render's own frame (fraction of its shorter side).
const ASSET_FADE: f32 = 0.06;

/// Which pre-rendered camera the preview shows. New views only need an
/// entry here, in [`View::dir`]'s files, and their assets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum View {
    /// 3/4 view from the front left (800x600).
    #[default]
    Hero,
    /// Orthographic top view, front up (700x700).
    Top,
}

impl View {
    pub const ALL: [View; 2] = [View::Hero, View::Top];

    pub fn label(self) -> &'static str {
        match self {
            View::Hero => "3/4 view",
            View::Top => "Top view",
        }
    }

    fn index(self) -> usize {
        match self {
            View::Hero => 0,
            View::Top => 1,
        }
    }

    /// The part of the render the preview shows, in asset pixels
    /// (x0, y0, x1, y1): the mouse, its glow and the pool on the desk,
    /// without the empty studio margin (the 3/4 render is framed loosely).
    fn frame(self) -> (usize, usize, usize, usize) {
        match self {
            View::Hero => (40, 125, 760, 600),
            View::Top => (0, 0, 700, 700),
        }
    }
}

/// Which side of the mouse a zone is on (left/right as the user sees it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Side {
    Left,
    Center,
    Right,
}

/// One light zone of the render.
#[derive(Debug, Clone, Copy)]
pub struct Zone {
    pub name: &'static str,
    /// Position along the mouse, 0 = rear-most .. 1 = front-most
    /// (`hotspots.json` of the render project).
    pub along: f32,
    pub side: Side,
}

pub const ZONE_COUNT: usize = 15;

macro_rules! zone_table {
    ($(($z:literal, $along:expr, $side:ident)),* $(,)?) => {
        /// The render's zones, in the order of the layer files.
        pub const ZONES: [Zone; ZONE_COUNT] = [$(Zone { name: $z, along: $along, side: Side::$side }),*];

        type Files = (&'static [u8], [(&'static [u8], &'static [u8]); ZONE_COUNT]);

        /// The embedded PNGs of a view: base, then (zone, direct) per zone.
        fn files(view: View) -> Files {
            match view {
                View::Hero => (
                    &include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/render/hero/base_lights_off.png"))[..],
                    [$((
                        &include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/render/hero/zone_", $z, ".png"))[..],
                        &include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/render/hero/zone_", $z, "_direct.png"))[..],
                    )),*],
                ),
                View::Top => (
                    &include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/render/top/base_lights_off.png"))[..],
                    [$((
                        &include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/render/top/zone_", $z, ".png"))[..],
                        &include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/render/top/zone_", $z, "_direct.png"))[..],
                    )),*],
                ),
            }
        }
    };
}

zone_table![
    ("wheel", 0.8, Center),
    ("palm", 0.278, Center),
    ("buttons_left", 0.661, Left),
    ("buttons_right", 0.661, Right),
    ("side_left", 0.457, Left),
    ("side_right", 0.457, Right),
    ("dpi", 0.635, Right),
    ("strip_left_1", 0.812, Left),
    ("strip_left_2", 0.656, Left),
    ("strip_left_3", 0.517, Left),
    ("strip_left_4", 0.43, Left),
    ("strip_right_1", 0.812, Right),
    ("strip_right_2", 0.656, Right),
    ("strip_right_3", 0.517, Right),
    ("strip_right_4", 0.43, Right),
];

/// Where each zone sits on the effect's virtual LED strip: `pos` in 0..1
/// ordered front to back, ties left to right (snake, swirl), and `row`, the
/// index of its front-to-back row (left/right twins share one; Christmas).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Slot {
    pub pos: f32,
    pub row: usize,
}

pub fn slots() -> [Slot; ZONE_COUNT] {
    let mut order: Vec<usize> = (0..ZONE_COUNT).collect();
    order.sort_by(|&a, &b| {
        let (za, zb) = (&ZONES[a], &ZONES[b]);
        zb.along
            .total_cmp(&za.along)
            .then(za.side.cmp(&zb.side))
            .then(a.cmp(&b))
    });
    let mut out = [Slot { pos: 0.0, row: 0 }; ZONE_COUNT];
    let mut row = 0;
    for (rank, &z) in order.iter().enumerate() {
        if rank > 0 && ZONES[order[rank - 1]].along != ZONES[z].along {
            row += 1;
        }
        out[z] = Slot {
            pos: rank as f32 / ZONE_COUNT as f32,
            row,
        };
    }
    out
}

/// What one zone shows in a frame.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ZoneLight {
    /// Linear RGB light, already scaled by the zone's intensity.
    pub rgb: [f32; 3],
    /// How lit the emitter is, 0..1 (drives the hide term).
    pub on: f32,
}

impl ZoneLight {
    /// From a display color (sRGB 0..1) and an intensity (0..1).
    pub fn new(color: Color, intensity: f32) -> ZoneLight {
        let k = intensity.clamp(0.0, 1.0);
        let rgb = [color.r, color.g, color.b].map(|c| srgb_to_linear(c) * k);
        let on = if rgb.iter().any(|&c| c > 0.0) { k } else { 0.0 };
        ZoneLight { rgb, on }
    }
}

// ----- color math ---------------------------------------------------------

pub fn srgb_to_linear(v: f32) -> f32 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

pub fn linear_to_srgb(v: f32) -> f32 {
    if v <= 0.003_130_8 {
        v * 12.92
    } else {
        1.055 * v.max(0.0).powf(1.0 / 2.4) - 0.055
    }
}

fn lin8() -> &'static [f32; 256] {
    static T: OnceLock<[f32; 256]> = OnceLock::new();
    T.get_or_init(|| std::array::from_fn(|i| srgb_to_linear(i as f32 / 255.0)))
}

const ENC_STEPS: usize = 1 << 16;

/// Linear 0..1 → sRGB 0..255 (float), sampled finely enough that the
/// darkest codes are resolved (one step is 0.05 of a code near black).
fn enc_table() -> &'static [f32] {
    static T: OnceLock<Vec<f32>> = OnceLock::new();
    T.get_or_init(|| {
        (0..=ENC_STEPS)
            .map(|i| linear_to_srgb(i as f32 / ENC_STEPS as f32) * 255.0)
            .collect()
    })
}

/// 8x8 Bayer matrix, as thresholds in 0..1 (centred on each cell).
const BAYER: [[u8; 8]; 8] = [
    [0, 32, 8, 40, 2, 34, 10, 42],
    [48, 16, 56, 24, 50, 18, 58, 26],
    [12, 44, 4, 36, 14, 46, 6, 38],
    [60, 28, 52, 20, 62, 30, 54, 22],
    [3, 35, 11, 43, 1, 33, 9, 41],
    [51, 19, 59, 27, 49, 17, 57, 25],
    [15, 47, 7, 39, 13, 45, 5, 37],
    [63, 31, 55, 23, 61, 29, 53, 21],
];

/// Linear value → 8-bit sRGB code with dither threshold `t` (0..1; 0.5 rounds).
#[inline]
pub fn encode8(lin: f32, t: f32) -> u8 {
    let table = enc_table();
    let i = (lin.clamp(0.0, 1.0) * ENC_STEPS as f32) as usize;
    (table[i] + t).min(255.0) as u8
}

// ----- layers -------------------------------------------------------------

/// A view's decoded layers at asset size, and its spill grids.
pub struct Layers {
    pub width: usize,
    pub height: usize,
    /// The part the preview frames (see [`View::frame`]).
    frame: (usize, usize, usize, usize),
    /// RGBA, straight alpha.
    base: Vec<u8>,
    zone: Vec<Vec<u8>>,
    direct: Vec<Vec<u8>>,
    /// Spill per zone (sum of the weighted blurs) on a grid of SPILL_GRID
    /// asset pixels per cell, padded by `spill_pad` cells on every side.
    spill: Vec<Vec<f32>>,
    spill_w: usize,
    spill_h: usize,
    spill_pad: usize,
}

fn decode(bytes: &[u8], rgba: bool) -> Result<(usize, usize, Vec<u8>), String> {
    let img = image::load_from_memory_with_format(bytes, image::ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    let (w, h) = (img.width() as usize, img.height() as usize);
    let data = if rgba {
        img.to_rgba8().into_raw()
    } else {
        img.to_luma8().into_raw()
    };
    Ok((w, h, data))
}

impl Layers {
    fn load(view: View) -> Result<Layers, String> {
        let (base_png, zone_pngs) = files(view);
        let (width, height, base) = decode(base_png, true)?;
        let mut zone = Vec::with_capacity(ZONE_COUNT);
        let mut direct = Vec::with_capacity(ZONE_COUNT);
        for (i, (z, d)) in zone_pngs.iter().enumerate() {
            for (bytes, out) in [(z, &mut zone), (d, &mut direct)] {
                let (w, h, data) = decode(bytes, false)?;
                if (w, h) != (width, height) {
                    return Err(format!(
                        "{view:?} zone {}: {w}x{h}, base is {width}x{height}",
                        ZONES[i].name
                    ));
                }
                out.push(data);
            }
        }
        let (x0, y0, x1, y1) = view.frame();
        if x1 > width || y1 > height || x0 + 8 >= x1 || y0 + 8 >= y1 {
            return Err(format!(
                "{view:?}: frame outside the {width}x{height} render"
            ));
        }
        // Spill: blur the 8-bit (sRGB-coded) light values, like the
        // approved mock-up, on a padded quarter-size grid.
        let (gw, gh) = ((width / SPILL_GRID).max(1), (height / SPILL_GRID).max(1));
        let widest = SPILL_BLURS.iter().map(|b| b.0).fold(0.0, f32::max);
        let pad = (2.5 * widest * gw as f32).ceil() as usize;
        let (pw, ph) = (gw + 2 * pad, gh + 2 * pad);
        let spill = zone
            .iter()
            .map(|z| {
                let coded: Vec<f32> = z.iter().map(|&v| v as f32 / 255.0).collect();
                let small = resize(&coded, width, height, gw, gh);
                let mut padded = vec![0.0f32; pw * ph];
                for y in 0..gh {
                    padded[(y + pad) * pw + pad..][..gw].copy_from_slice(&small[y * gw..][..gw]);
                }
                let mut acc = vec![0.0f32; pw * ph];
                for (frac, weight) in SPILL_BLURS {
                    let b = gaussian_blur(&padded, pw, ph, frac * gw as f32);
                    for (a, v) in acc.iter_mut().zip(b) {
                        *a += weight * v;
                    }
                }
                acc
            })
            .collect();
        Ok(Layers {
            width,
            height,
            frame: (x0, y0, x1, y1),
            base,
            zone,
            direct,
            spill,
            spill_w: pw,
            spill_h: ph,
            spill_pad: pad,
        })
    }

    /// Size of the framed part, in asset pixels.
    pub fn frame_size(&self) -> (usize, usize) {
        (self.frame.2 - self.frame.0, self.frame.3 - self.frame.1)
    }
}

/// The decoded layers of `view` (decoded once, on first use), or why they
/// could not be (the page then falls back to the vector drawing).
pub fn layers(view: View) -> Result<&'static Layers, &'static str> {
    static CELLS: [OnceLock<Result<Layers, String>>; 2] = [OnceLock::new(), OnceLock::new()];
    CELLS[view.index()]
        .get_or_init(|| {
            let t0 = Instant::now();
            let layers = Layers::load(view);
            if stats_enabled() {
                eprintln!("render: {view:?} layers decoded in {:.1?}", t0.elapsed());
            }
            layers
        })
        .as_ref()
        .map_err(|e| e.as_str())
}

/// Decodes the views on a background thread, so the Lighting page does not
/// wait for them on its first frame.
pub fn preload() {
    std::thread::spawn(|| {
        for view in View::ALL {
            let _ = layers(view);
        }
    });
}

// ----- resampling and blur --------------------------------------------------

/// Tent-filter taps for one axis: for each output index, (source index,
/// weight) pairs. The filter widens with the downscale factor, so a shrink
/// averages every source pixel (no aliasing in the slot pattern).
fn axis_weights(src: usize, dst: usize) -> Vec<Vec<(usize, f32)>> {
    let scale = src as f32 / dst as f32;
    let radius = scale.max(1.0);
    (0..dst)
        .map(|o| {
            let c = (o as f32 + 0.5) * scale - 0.5;
            let lo = (c - radius).floor() as isize + 1;
            let hi = (c + radius).ceil() as isize - 1;
            let mut taps: Vec<(usize, f32)> = Vec::new();
            for i in lo..=hi {
                let w = (1.0 - (i as f32 - c).abs() / radius).max(0.0);
                if w <= 0.0 {
                    continue;
                }
                // Taps past the edge fold into the edge pixel.
                let idx = i.clamp(0, src as isize - 1) as usize;
                match taps.iter_mut().find(|t| t.0 == idx) {
                    Some(t) => t.1 += w,
                    None => taps.push((idx, w)),
                }
            }
            let sum: f32 = taps.iter().map(|t| t.1).sum();
            taps.iter_mut().for_each(|t| t.1 /= sum);
            taps
        })
        .collect()
}

/// Resizes a single-channel plane (separable tent filter).
fn resize(src: &[f32], sw: usize, sh: usize, dw: usize, dh: usize) -> Vec<f32> {
    if (sw, sh) == (dw, dh) {
        return src.to_vec();
    }
    let wx = axis_weights(sw, dw);
    let wy = axis_weights(sh, dh);
    let mut tmp = vec![0.0f32; dw * sh];
    for y in 0..sh {
        let row = &src[y * sw..(y + 1) * sw];
        for (x, taps) in wx.iter().enumerate() {
            tmp[y * dw + x] = taps.iter().map(|&(i, w)| row[i] * w).sum();
        }
    }
    let mut out = vec![0.0f32; dw * dh];
    for (y, taps) in wy.iter().enumerate() {
        let dst = &mut out[y * dw..(y + 1) * dw];
        for &(i, w) in taps {
            let src_row = &tmp[i * dw..(i + 1) * dw];
            for (d, s) in dst.iter_mut().zip(src_row) {
                *d += s * w;
            }
        }
    }
    out
}

/// Gaussian blur approximated by three box blurs (edges clamped).
fn gaussian_blur(src: &[f32], w: usize, h: usize, sigma: f32) -> Vec<f32> {
    // Box sizes for three passes (W. Jarosz, "Fast image convolutions").
    let n = 3.0;
    let ideal = (12.0 * sigma * sigma / n + 1.0).sqrt();
    let mut wl = ideal.floor() as i32;
    if wl % 2 == 0 {
        wl -= 1;
    }
    let wu = wl + 2;
    let m = ((12.0 * sigma * sigma - n * (wl * wl) as f32 - 4.0 * n * wl as f32 - 3.0 * n)
        / (-4.0 * wl as f32 - 4.0))
        .round() as i32;
    let mut cur = src.to_vec();
    for pass in 0..3 {
        let r = (if pass < m { wl } else { wu }).max(1) as usize / 2;
        cur = box_blur_h(&cur, w, h, r);
        cur = box_blur_v(&cur, w, h, r);
    }
    cur
}

fn box_blur_h(src: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    let mut out = vec![0.0; src.len()];
    let norm = 1.0 / (2 * r + 1) as f32;
    for y in 0..h {
        let row = &src[y * w..(y + 1) * w];
        let at = |i: isize| row[i.clamp(0, w as isize - 1) as usize];
        let mut acc: f32 = (-(r as isize)..=r as isize).map(at).sum();
        for x in 0..w {
            out[y * w + x] = acc * norm;
            acc += at(x as isize + r as isize + 1) - at(x as isize - r as isize);
        }
    }
    out
}

fn box_blur_v(src: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    let mut out = vec![0.0; src.len()];
    let norm = 1.0 / (2 * r + 1) as f32;
    for x in 0..w {
        let at = |i: isize| src[i.clamp(0, h as isize - 1) as usize * w + x];
        let mut acc: f32 = (-(r as isize)..=r as isize).map(at).sum();
        for y in 0..h {
            out[y * w + x] = acc * norm;
            acc += at(y as isize + r as isize + 1) - at(y as isize - r as isize);
        }
    }
    out
}

/// Bilinear sample of a plane at fractional pixel coordinates (pixel
/// centres at integer + 0.5); zero outside.
fn sample(plane: &[f32], w: usize, h: usize, x: f32, y: f32) -> f32 {
    let (fx, fy) = (x - 0.5, y - 0.5);
    let (x0, y0) = (fx.floor(), fy.floor());
    let (tx, ty) = (fx - x0, fy - y0);
    let at = |xi: f32, yi: f32| {
        if xi < 0.0 || yi < 0.0 || xi >= w as f32 || yi >= h as f32 {
            0.0
        } else {
            plane[yi as usize * w + xi as usize]
        }
    };
    let top = at(x0, y0) * (1.0 - tx) + at(x0 + 1.0, y0) * tx;
    let bottom = at(x0, y0 + 1.0) * (1.0 - tx) + at(x0 + 1.0, y0 + 1.0) * tx;
    top * (1.0 - ty) + bottom * ty
}

// ----- scene ----------------------------------------------------------------

/// A plane cropped to the box where it is not (nearly) zero.
struct Sparse {
    x0: usize,
    y0: usize,
    w: usize,
    h: usize,
    data: Vec<f32>,
}

impl Sparse {
    fn new(full: &[f32], width: usize, height: usize, threshold: f32) -> Sparse {
        let (mut x0, mut y0, mut x1, mut y1) = (width, height, 0, 0);
        for y in 0..height {
            for x in 0..width {
                if full[y * width + x] > threshold {
                    x0 = x0.min(x);
                    x1 = x1.max(x + 1);
                    y0 = y0.min(y);
                    y1 = y1.max(y + 1);
                }
            }
        }
        if x1 <= x0 {
            return Sparse {
                x0: 0,
                y0: 0,
                w: 0,
                h: 0,
                data: Vec::new(),
            };
        }
        let (w, h) = (x1 - x0, y1 - y0);
        let mut data = Vec::with_capacity(w * h);
        for y in y0..y1 {
            data.extend_from_slice(&full[y * width + x0..y * width + x1]);
        }
        Sparse { x0, y0, w, h, data }
    }
}

/// Where the render sits in the output: asset pixel (ax, ay) lands on
/// output pixel (ox + ax * k, oy + ay * k).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    pub k: f32,
    pub ox: f32,
    pub oy: f32,
}

impl Placement {
    /// Fits the view's frame into a `w` x `h` output, centred, at most
    /// `max_k` output pixels per asset pixel.
    pub fn fit(layers: &Layers, w: usize, h: usize, max_k: f32) -> Placement {
        let (fw, fh) = layers.frame_size();
        let k = (w as f32 / fw as f32).min(h as f32 / fh as f32).min(max_k);
        Placement {
            k,
            ox: (w as f32 - fw as f32 * k) / 2.0 - layers.frame.0 as f32 * k,
            oy: (h as f32 - fh as f32 * k) / 2.0 - layers.frame.1 as f32 * k,
        }
    }
}

/// Spill is mixed on a grid of this many output pixels per cell, then
/// upsampled (it is a wide, smooth glow).
const OUT_GRID: usize = 4;

/// One view at one output size and placement over one background.
pub struct Scene {
    pub view: View,
    pub width: usize,
    pub height: usize,
    pub background: [u8; 3],
    /// Base over the background, linear RGB, 3 floats per pixel.
    surface: Vec<f32>,
    direct: Vec<Sparse>,
    /// The render's own zone light (emitter, bounce, halo, pool).
    light: Vec<Sparse>,
    /// Sum of every zone's light (all zones one color: one pass).
    light_all: Vec<f32>,
    /// Spill per zone on the output grid (`grid_w` x `grid_h`), faded.
    spill: Vec<Vec<f32>>,
    spill_all: Vec<f32>,
    grid_w: usize,
    grid_h: usize,
}

impl Scene {
    pub fn new(
        layers: &Layers,
        view: View,
        width: usize,
        height: usize,
        placement: Placement,
        bg: [u8; 3],
    ) -> Scene {
        Self::with_spill(layers, view, width, height, placement, bg, SPILL)
    }

    #[allow(clippy::too_many_arguments)]
    fn with_spill(
        layers: &Layers,
        view: View,
        width: usize,
        height: usize,
        placement: Placement,
        bg: [u8; 3],
        spill: f32,
    ) -> Scene {
        let (sw, sh) = (layers.width, layers.height);
        let (w, h) = (width.max(1), height.max(1));
        let Placement { k, ox, oy } = placement;
        let lin = lin8();
        let bgl = bg.map(|c| lin[c as usize]);

        // The asset resized to k, and where it lands (may hang over the
        // output's edges, which simply cut it).
        let (aw, ah) = (
            ((sw as f32 * k).round() as usize).max(1),
            ((sh as f32 * k).round() as usize).max(1),
        );
        let (ax, ay) = (ox.round() as isize, oy.round() as isize);
        let fade = edge_fade_len(w, h, LAYER_FADE * w.min(h) as f32);
        // The render's own edges fade too: its faint shadow and halo reach
        // them, and must not end in a line where the panel is larger.
        let asset_fade = edge_fade_len(aw, ah, ASSET_FADE * aw.min(ah) as f32);
        // Resize an asset plane to k and paste it into the output.
        let place = |plane: &[f32], faded: bool| -> Vec<f32> {
            let mut r = resize(plane, sw, sh, aw, ah);
            if faded {
                r.iter_mut().zip(&asset_fade).for_each(|(v, f)| *v *= f);
            }
            let mut out = vec![0.0f32; w * h];
            for y in 0..ah {
                let oy = y as isize + ay;
                if oy < 0 || oy >= h as isize {
                    continue;
                }
                for x in 0..aw {
                    let ox = x as isize + ax;
                    if ox < 0 || ox >= w as isize {
                        continue;
                    }
                    let i = oy as usize * w + ox as usize;
                    out[i] = r[y * aw + x] * if faded { fade[i] } else { 1.0 };
                }
            }
            out
        };

        // Base: premultiplied linear color and alpha, then over the
        // background. Alpha fades out at the edge with the light (the
        // faint contact shadow must not end in a line either).
        let n = sw * sh;
        let alpha: Vec<f32> = (0..n)
            .map(|i| layers.base[i * 4 + 3] as f32 / 255.0)
            .collect();
        let a = place(&alpha, true);
        let mut surface = vec![0.0f32; w * h * 3];
        for c in 0..3 {
            let pre: Vec<f32> = (0..n)
                .map(|i| lin[layers.base[i * 4 + c] as usize] * alpha[i])
                .collect();
            let p = place(&pre, true);
            for i in 0..w * h {
                surface[i * 3 + c] = bgl[c] * (1.0 - a[i]) + p[i];
            }
        }

        let mut light_all = vec![0.0f32; w * h];
        let mut light = Vec::with_capacity(ZONE_COUNT);
        let mut direct = Vec::with_capacity(ZONE_COUNT);
        for z in 0..ZONE_COUNT {
            let d: Vec<f32> = layers.direct[z].iter().map(|&v| v as f32 / 255.0).collect();
            direct.push(Sparse::new(&place(&d, false), w, h, 1e-3));
            let zl: Vec<f32> = layers.zone[z].iter().map(|&v| lin[v as usize]).collect();
            let l = place(&zl, true);
            for (acc, v) in light_all.iter_mut().zip(&l) {
                *acc += v;
            }
            light.push(Sparse::new(&l, w, h, 1e-4));
        }

        // Spill: sample the padded asset grid at each output grid cell.
        let (gw, gh) = (w.div_ceil(OUT_GRID) + 1, h.div_ceil(OUT_GRID) + 1);
        let gfade = edge_fade_at(w, h, gw, gh);
        let cell = SPILL_GRID as f32;
        let pad = layers.spill_pad as f32;
        let mut spill_all = vec![0.0f32; gw * gh];
        let spills = (0..ZONE_COUNT)
            .map(|z| {
                let mut g = vec![0.0f32; gw * gh];
                if spill > 0.0 {
                    for gy in 0..gh {
                        let py = (gy * OUT_GRID) as f32;
                        let sy = ((py - oy) / k) / cell + pad;
                        for gx in 0..gw {
                            let px = (gx * OUT_GRID) as f32;
                            let sx = ((px - ox) / k) / cell + pad;
                            let v =
                                sample(&layers.spill[z], layers.spill_w, layers.spill_h, sx, sy);
                            g[gy * gw + gx] = spill * v * gfade[gy * gw + gx];
                        }
                    }
                }
                for (a, v) in spill_all.iter_mut().zip(&g) {
                    *a += v;
                }
                g
            })
            .collect();

        Scene {
            view,
            width: w,
            height: h,
            background: bg,
            surface,
            direct,
            light,
            light_all,
            spill: spills,
            spill_all,
            grid_w: gw,
            grid_h: gh,
        }
    }

    /// Mixes the zones' colored spill on the grid (3 floats per cell).
    fn spill_grid(&self, lights: &[ZoneLight; ZONE_COUNT], uniform: bool) -> Vec<f32> {
        let mut grid = vec![0.0f32; self.grid_w * self.grid_h * 3];
        let mut mix = |plane: &[f32], c: [f32; 3]| {
            for (g, v) in grid.as_chunks_mut::<3>().0.iter_mut().zip(plane) {
                g[0] += c[0] * v;
                g[1] += c[1] * v;
                g[2] += c[2] * v;
            }
        };
        if uniform {
            mix(&self.spill_all, lights[0].rgb);
        } else {
            for (plane, l) in self.spill.iter().zip(lights) {
                if !dark(&l.rgb) {
                    mix(plane, l.rgb);
                }
            }
        }
        grid
    }

    /// One output row in linear light: `row` gets 3 floats per pixel;
    /// `lit` is scratch of one float per pixel.
    fn row(&self, f: &FrameInput, y: usize, row: &mut [f32], lit: &mut [f32]) {
        let w = self.width;
        row.copy_from_slice(&self.surface[y * w * 3..][..w * 3]);
        let lights = f.lights;
        // Hide the off-state look of lit emitters.
        if f.any_on {
            lit.fill(0.0);
            let mut touched = false;
            for (d, l) in self.direct.iter().zip(lights) {
                if l.on <= 0.0 || y < d.y0 || y >= d.y0 + d.h {
                    continue;
                }
                touched = true;
                let src = &d.data[(y - d.y0) * d.w..][..d.w];
                for (m, v) in lit[d.x0..d.x0 + d.w].iter_mut().zip(src) {
                    *m = m.max(v * l.on);
                }
            }
            if touched {
                for (px, m) in row.as_chunks_mut::<3>().0.iter_mut().zip(lit.iter()) {
                    let k = 1.0 - HIDE * m;
                    px[0] *= k;
                    px[1] *= k;
                    px[2] *= k;
                }
            }
        }
        if f.all_dark {
            return;
        }
        // The render's own light, zone by zone (or all at once).
        if f.uniform {
            let c = lights[0].rgb;
            for (px, v) in row
                .as_chunks_mut::<3>()
                .0
                .iter_mut()
                .zip(&self.light_all[y * w..][..w])
            {
                px[0] += c[0] * v;
                px[1] += c[1] * v;
                px[2] += c[2] * v;
            }
        } else {
            for (s, l) in self.light.iter().zip(lights) {
                let c = l.rgb;
                if dark(&c) || y < s.y0 || y >= s.y0 + s.h {
                    continue;
                }
                let src = &s.data[(y - s.y0) * s.w..][..s.w];
                let dst = &mut row[s.x0 * 3..(s.x0 + s.w) * 3];
                for (px, v) in dst.as_chunks_mut::<3>().0.iter_mut().zip(src) {
                    px[0] += c[0] * v;
                    px[1] += c[1] * v;
                    px[2] += c[2] * v;
                }
            }
        }
        // Spill, upsampled bilinearly from the grid.
        let gw = self.grid_w;
        let fy = y as f32 / OUT_GRID as f32;
        let j = (fy as usize).min(self.grid_h - 2);
        let ty = fy - j as f32;
        let (r0, r1) = (
            &f.grid[j * gw * 3..][..gw * 3],
            &f.grid[(j + 1) * gw * 3..][..gw * 3],
        );
        for (px, &(i, tx)) in row.as_chunks_mut::<3>().0.iter_mut().zip(&f.xs) {
            for c in 0..3 {
                let a = r0[i * 3 + c] + (r0[i * 3 + 3 + c] - r0[i * 3 + c]) * tx;
                let b = r1[i * 3 + c] + (r1[i * 3 + 3 + c] - r1[i * 3 + c]) * tx;
                px[c] += a + (b - a) * ty;
            }
        }
    }

    fn frame_input<'a>(&self, lights: &'a [ZoneLight; ZONE_COUNT]) -> FrameInput<'a> {
        let first = lights[0].rgb;
        let uniform = lights.iter().all(|l| l.rgb == first);
        let all_dark = lights.iter().all(|l| dark(&l.rgb));
        let grid = if all_dark {
            Vec::new()
        } else {
            self.spill_grid(lights, uniform)
        };
        let gw = self.grid_w;
        let xs = (0..self.width)
            .map(|x| {
                let f = x as f32 / OUT_GRID as f32;
                let i = (f as usize).min(gw - 2);
                (i, f - i as f32)
            })
            .collect();
        FrameInput {
            lights,
            uniform,
            all_dark,
            any_on: lights.iter().any(|l| l.on > 0.0),
            grid,
            xs,
        }
    }

    /// The frame in linear light (3 floats per pixel, not clamped).
    #[cfg(test)]
    pub fn accumulate(&self, lights: &[ZoneLight; ZONE_COUNT]) -> Vec<f32> {
        let f = self.frame_input(lights);
        let w = self.width;
        let mut out = vec![0.0f32; w * self.height * 3];
        let mut lit = vec![0.0f32; w];
        for (y, row) in out.chunks_exact_mut(w * 3).enumerate() {
            self.row(&f, y, row, &mut lit);
        }
        out
    }

    /// The frame as RGBA8 (sRGB, ordered dither, opaque), in a few bands
    /// on worker threads.
    pub fn composite(&self, lights: &[ZoneLight; ZONE_COUNT]) -> Vec<u8> {
        let f = self.frame_input(lights);
        let (w, h) = (self.width, self.height);
        let mut out = vec![255u8; w * h * 4];
        let threads = std::thread::available_parallelism()
            .map_or(1, |n| n.get())
            .clamp(1, MAX_THREADS);
        let band = h.div_ceil(threads).max(1);
        let work = |y0: usize, chunk: &mut [u8]| {
            let mut row = vec![0.0f32; w * 3];
            let mut lit = vec![0.0f32; w];
            for (dy, orow) in chunk.chunks_exact_mut(w * 4).enumerate() {
                let y = y0 + dy;
                self.row(&f, y, &mut row, &mut lit);
                let bayer = &BAYER[y & 7];
                for (x, (px, o)) in row
                    .as_chunks::<3>()
                    .0
                    .iter()
                    .zip(orow.as_chunks_mut::<4>().0.iter_mut())
                    .enumerate()
                {
                    let t = (bayer[x & 7] as f32 + 0.5) / 64.0;
                    o[0] = encode8(px[0], t);
                    o[1] = encode8(px[1], t);
                    o[2] = encode8(px[2], t);
                }
            }
        };
        if threads == 1 {
            work(0, &mut out);
        } else {
            std::thread::scope(|scope| {
                for (b, chunk) in out.chunks_mut(band * w * 4).enumerate() {
                    let work = &work;
                    scope.spawn(move || work(b * band, chunk));
                }
            });
        }
        out
    }
}

/// Worker threads for one frame (the frame is small; more only adds
/// start-up cost).
const MAX_THREADS: usize = 4;

/// What every row of one frame shares.
struct FrameInput<'a> {
    lights: &'a [ZoneLight; ZONE_COUNT],
    uniform: bool,
    all_dark: bool,
    any_on: bool,
    /// Colored spill on the grid (empty when every zone is dark).
    grid: Vec<f32>,
    /// Per output column: grid cell and fraction.
    xs: Vec<(usize, f32)>,
}

fn dark(c: &[f32; 3]) -> bool {
    c.iter().all(|&v| v <= 0.0)
}

/// The spill fades out over this many output pixels at the picture's edge.
fn fade_px(w: usize, h: usize) -> f32 {
    (EDGE_FADE * w.min(h) as f32).max(2.0)
}

fn ramp(d: f32, len: f32) -> f32 {
    let t = (d / len).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// 1 inside, smoothly 0 over `len` pixels at the border.
fn edge_fade_len(w: usize, h: usize, len: f32) -> Vec<f32> {
    let len = len.max(1.0);
    let along = |i: usize, n: usize| {
        let c = i as f32 + 0.5;
        ramp(c.min(n as f32 - c), len)
    };
    let fx: Vec<f32> = (0..w).map(|x| along(x, w)).collect();
    let mut out = Vec::with_capacity(w * h);
    for y in 0..h {
        let fy = along(y, h);
        out.extend(fx.iter().map(|f| f * fy));
    }
    out
}

/// The same fade at the output grid's sample points (x = gx * OUT_GRID).
fn edge_fade_at(w: usize, h: usize, gw: usize, gh: usize) -> Vec<f32> {
    let len = fade_px(w, h);
    let along = |p: f32, n: usize| ramp(p.min(n as f32 - p).max(0.0), len);
    let mut out = Vec::with_capacity(gw * gh);
    for gy in 0..gh {
        let fy = along((gy * OUT_GRID) as f32, h);
        for gx in 0..gw {
            out.push(along((gx * OUT_GRID) as f32, w) * fy);
        }
    }
    out
}

// ----- canvas widget --------------------------------------------------------

/// Built scenes plus frame-time statistics, shared by the canvas draws.
/// Two scenes are kept: effects with and without a color card give the
/// panel two heights, so switching effects does not rebuild.
#[derive(Default)]
pub struct Cache {
    scenes: Vec<Scene>,
    stats: Stats,
}

const KEPT_SCENES: usize = 2;

#[derive(Default)]
struct Stats {
    frames: u32,
    total: Duration,
    worst: Duration,
    built: Option<Duration>,
}

impl Cache {
    fn scene(&mut self, layers: &Layers, view: View, w: usize, h: usize, bg: [u8; 3]) -> &Scene {
        let key = (view, w, h, bg);
        match self
            .scenes
            .iter()
            .position(|s| (s.view, s.width, s.height, s.background) == key)
        {
            Some(i) => {
                let scene = self.scenes.remove(i);
                self.scenes.insert(0, scene);
            }
            None => {
                let t0 = Instant::now();
                let placement = Placement::fit(layers, w, h, MAX_ZOOM);
                self.scenes
                    .insert(0, Scene::new(layers, view, w, h, placement, bg));
                self.scenes.truncate(KEPT_SCENES);
                self.stats.built = Some(t0.elapsed());
            }
        }
        &self.scenes[0]
    }
}

/// `TAILGLOW_RENDER_STATS=1`: frame times on stderr every 50 frames.
fn stats_enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("TAILGLOW_RENDER_STATS").is_some())
}

/// The canvas program for the big preview.
pub struct RenderPreview<'a> {
    pub cache: &'a Mutex<Cache>,
    pub view: View,
    pub lights: [ZoneLight; ZONE_COUNT],
    /// The panel color behind the picture (the glow lands on it).
    pub background: Color,
    /// Window scale factor: the picture is composited at device pixels.
    pub scale: f32,
}

/// Inset from the panel's edge (keeps clear of its rounded corners).
const MARGIN: f32 = 8.0;
/// Largest picture composited per frame, in pixels; a bigger panel gets a
/// smaller composite that the renderer scales up (the glow is soft anyway).
const MAX_PIXELS: f32 = 420_000.0;
/// At most this many output pixels per asset pixel (beyond it the render
/// would only get blurrier, so it stays at this size).
const MAX_ZOOM: f32 = 1.25;

impl<Message> canvas::Program<Message> for RenderPreview<'_> {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let Ok(layers) = layers(self.view) else {
            return vec![frame.into_geometry()];
        };
        let rect = Rectangle {
            x: MARGIN,
            y: MARGIN,
            width: (bounds.width - 2.0 * MARGIN).max(1.0),
            height: (bounds.height - 2.0 * MARGIN).max(1.0),
        };
        // Device pixels, capped at MAX_PIXELS; rounded to 4 px so a resize
        // does not rebuild the scene for every pixel.
        let mut scale = self.scale.max(0.5);
        let area = rect.width * rect.height * scale * scale;
        if area > MAX_PIXELS {
            scale *= (MAX_PIXELS / area).sqrt();
        }
        let px = |v: f32| ((v * scale) as usize / 4 * 4).max(8);
        let (w, h) = (px(rect.width), px(rect.height));
        let bg = self.background.into_rgba8();
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        let t0 = Instant::now();
        let rgba = cache
            .scene(layers, self.view, w, h, [bg[0], bg[1], bg[2]])
            .composite(&self.lights);
        let took = t0.elapsed();
        if stats_enabled() {
            let s = &mut cache.stats;
            if let Some(b) = s.built.take() {
                eprintln!("render: scene {:?} {w}x{h} built in {b:.1?}", self.view);
                *s = Stats::default();
            } else {
                s.frames += 1;
                s.total += took;
                s.worst = s.worst.max(took);
                if s.frames == 50 {
                    eprintln!(
                        "render: {:?} {w}x{h}: composite avg {:.2?}, worst {:.2?} over 50 frames",
                        self.view,
                        s.total / 50,
                        s.worst
                    );
                    *s = Stats::default();
                }
            }
        }
        let image = canvas::Image::new(Handle::from_rgba(w as u32, h as u32, rgba))
            .filter_method(FilterMethod::Linear);
        frame.draw_image(rect, image);
        vec![frame.into_geometry()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DARK: [u8; 3] = [0x22, 0x24, 0x28];
    const LIGHT: [u8; 3] = [0xee, 0xf0, 0xf3];

    fn off() -> [ZoneLight; ZONE_COUNT] {
        [ZoneLight::default(); ZONE_COUNT]
    }

    #[test]
    fn every_view_decodes_with_all_zones() {
        for view in View::ALL {
            let l = layers(view).expect("embedded render decodes");
            assert_eq!(l.zone.len(), ZONE_COUNT);
            assert_eq!(l.base.len(), l.width * l.height * 4);
            assert!(l.zone.iter().all(|z| z.len() == l.width * l.height));
            // Every zone has some light in every view.
            assert!(l.zone.iter().all(|z| z.iter().any(|&v| v > 0)), "{view:?}");
        }
        assert_eq!(layers(View::Hero).unwrap().width, 800);
        assert_eq!(layers(View::Top).unwrap().width, 700);
    }

    #[test]
    fn srgb_round_trip_is_exact_for_every_code() {
        for c in 0..=255u8 {
            let lin = srgb_to_linear(c as f32 / 255.0);
            assert_eq!(encode8(lin, 0.5), c, "code {c}");
            let back = linear_to_srgb(lin) * 255.0;
            assert!((back - c as f32).abs() < 1e-3, "code {c}: {back}");
        }
        assert_eq!(encode8(-1.0, 0.5), 0);
        assert_eq!(encode8(7.0, 0.99), 255);
    }

    #[test]
    fn zero_brightness_gives_the_base_image_over_the_background() {
        // At k = 1 nothing is resampled, so inside the edge fade the frame
        // must be exactly base-over-background (within the ±1 of the dither).
        let l = layers(View::Hero).unwrap();
        let (w, h) = (l.width, l.height);
        let at_one = Placement {
            k: 1.0,
            ox: 0.0,
            oy: 0.0,
        };
        let margin = fade_px(w, h) as usize + 1;
        for bg in [DARK, LIGHT] {
            let scene = Scene::new(l, View::Hero, w, h, at_one, bg);
            let mut lights = off();
            // Lit colors at zero intensity count as off.
            lights[3] = ZoneLight::new(Color::from_rgb(1.0, 0.0, 0.0), 0.0);
            let out = scene.composite(&lights);
            let lin = lin8();
            let mut worst = 0i32;
            for y in margin..h - margin {
                for x in margin..w - margin {
                    let i = y * w + x;
                    let a = l.base[i * 4 + 3] as f32 / 255.0;
                    for c in 0..3 {
                        let v =
                            lin[bg[c] as usize] * (1.0 - a) + lin[l.base[i * 4 + c] as usize] * a;
                        let want = (linear_to_srgb(v) * 255.0).round() as i32;
                        worst = worst.max((out[i * 4 + c] as i32 - want).abs());
                    }
                }
            }
            assert!(worst <= 1, "off frame differs from base by {worst}");
        }
    }

    #[test]
    fn zone_light_adds_linearly() {
        let l = layers(View::Top).unwrap();
        let scene = Scene::new(
            l,
            View::Top,
            140,
            120,
            Placement::fit(l, 140, 120, 1.0),
            DARK,
        );
        let surface = scene.accumulate(&off());
        // `on` = 0: only the additive part, so it must be exactly additive.
        let mut a = off();
        a[0] = ZoneLight {
            rgb: [0.6, 0.1, 0.0],
            on: 0.0,
        };
        a[7] = ZoneLight {
            rgb: [0.0, 0.2, 0.3],
            on: 0.0,
        };
        let mut b = off();
        b[0] = ZoneLight {
            rgb: [0.1, 0.0, 0.4],
            on: 0.0,
        };
        b[1] = ZoneLight {
            rgb: [0.5, 0.5, 0.5],
            on: 0.0,
        };
        let mut ab = off();
        for z in 0..ZONE_COUNT {
            for c in 0..3 {
                ab[z].rgb[c] = a[z].rgb[c] + b[z].rgb[c];
            }
        }
        let (fa, fb, fab) = (
            scene.accumulate(&a),
            scene.accumulate(&b),
            scene.accumulate(&ab),
        );
        let mut lit = 0;
        for i in 0..fa.len() {
            let sum = (fa[i] - surface[i]) + (fb[i] - surface[i]);
            assert!((fab[i] - surface[i] - sum).abs() < 1e-4, "pixel {i}");
            if sum > 0.01 {
                lit += 1;
            }
        }
        assert!(lit > 100, "the zones light something ({lit} px)");
        // Doubling the light doubles the glow.
        let mut a2 = a;
        a2.iter_mut().for_each(|z| z.rgb = z.rgb.map(|c| c * 2.0));
        let fa2 = scene.accumulate(&a2);
        for i in 0..fa.len() {
            assert!(((fa2[i] - surface[i]) - 2.0 * (fa[i] - surface[i])).abs() < 1e-4);
        }
    }

    #[test]
    fn a_lit_emitter_hides_its_off_look_and_spill_reaches_the_desk() {
        let l = layers(View::Hero).unwrap();
        let red = [ZoneLight::new(Color::from_rgb(1.0, 0.0, 0.0), 1.0); ZONE_COUNT];
        let at = Placement::fit(l, 400, 300, 1.0);
        let with = Scene::new(l, View::Hero, 400, 300, at, DARK);
        let without = Scene::with_spill(l, View::Hero, 400, 300, at, DARK, 0.0);
        let (fw, fo) = (with.accumulate(&red), without.accumulate(&red));
        let surface = without.accumulate(&off());
        // Somewhere on an emitter the green channel drops (red stays red).
        assert!(fo
            .as_chunks::<3>()
            .0
            .iter()
            .zip(surface.as_chunks::<3>().0.iter())
            .any(|(f, s)| f[1] < s[1] * 0.5 && s[1] > 0.05));
        // The spill only adds red, and adds it in a wide area.
        let mut spilled = 0;
        for (a, b) in fw
            .as_chunks::<3>()
            .0
            .iter()
            .zip(fo.as_chunks::<3>().0.iter())
        {
            assert!(a[0] >= b[0] - 1e-6 && (a[1] - b[1]).abs() < 1e-6);
            if a[0] - b[0] > 0.002 {
                spilled += 1;
            }
        }
        assert!(spilled > 400 * 300 / 4, "spill covers {spilled} px");
        // And it fades out at the picture's edge (no visible rectangle).
        for x in 0..400 {
            for y in [0, 299] {
                let i = (y * 400 + x) * 3;
                assert!((fw[i] - fo[i]).abs() < 1e-3, "edge x={x} y={y}");
            }
        }
    }

    #[test]
    fn slots_run_front_to_back_then_left_to_right() {
        let s = slots();
        let pos = |name: &str| s[ZONES.iter().position(|z| z.name == name).unwrap()];
        assert_eq!(pos("strip_left_1").pos, 0.0);
        assert!(pos("strip_left_1").pos < pos("strip_right_1").pos);
        assert_eq!(pos("strip_left_1").row, pos("strip_right_1").row);
        assert!(pos("strip_right_1").pos < pos("wheel").pos);
        assert!(pos("wheel").pos < pos("buttons_left").pos);
        assert!(pos("strip_left_4").pos < pos("palm").pos);
        let mut all: Vec<f32> = s.iter().map(|s| s.pos).collect();
        all.sort_by(f32::total_cmp);
        all.dedup();
        assert_eq!(all.len(), ZONE_COUNT, "every zone has its own place");
    }

    #[test]
    fn blur_keeps_the_total_and_spreads() {
        let (w, h) = (64, 48);
        let mut img = vec![0.0f32; w * h];
        img[24 * w + 32] = 1.0;
        let b = gaussian_blur(&img, w, h, 4.0);
        let total: f32 = b.iter().sum();
        assert!((total - 1.0).abs() < 1e-3, "{total}");
        assert!(b[24 * w + 32] < 0.05 && b[24 * w + 36] > 0.001);
    }
}
