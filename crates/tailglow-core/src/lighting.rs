// SPDX-License-Identifier: GPL-3.0-or-later
//! Typed lighting values.
//!
//! Evidence tags: *verified* = seen in `testdata/session1.pcap` and confirmed on
//! the mouse with the webcam; *likely* = consistent with the capture but not
//! independently confirmed; *unknown* = not captured yet.

use crate::protocol::Command;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// A lighting effect. The discriminant is the byte the mouse uses (*verified*
/// for all seven: each was selected in the vendor app during session 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[repr(u8)]
pub enum Effect {
    Off = 0,
    Static = 1,
    Breathing = 2,
    Rainbow = 3,
    RainbowSwirl = 4,
    Snake = 5,
    Christmas = 6,
}

impl Effect {
    /// Every effect, in the order the vendor app lists them.
    pub const ALL: [Effect; 7] = [
        Effect::Off,
        Effect::Static,
        Effect::Breathing,
        Effect::Rainbow,
        Effect::RainbowSwirl,
        Effect::Snake,
        Effect::Christmas,
    ];

    pub fn id(self) -> u8 {
        self as u8
    }

    pub fn from_id(id: u8) -> Option<Effect> {
        Effect::ALL.iter().copied().find(|e| e.id() == id)
    }

    /// Human-readable name (matches the vendor app's labels).
    pub fn label(self) -> &'static str {
        match self {
            Effect::Off => "Lights Off",
            Effect::Static => "Static Light",
            Effect::Breathing => "Breathing",
            Effect::Rainbow => "Rainbow",
            Effect::RainbowSwirl => "Rainbow Swirl",
            Effect::Snake => "Snake",
            Effect::Christmas => "Christmas",
        }
    }

    /// Short CLI/config name (`off`, `static`, `breathing`, `rainbow`,
    /// `rainbow-swirl`, `snake`, `christmas`).
    pub fn name(self) -> &'static str {
        match self {
            Effect::Off => "off",
            Effect::Static => "static",
            Effect::Breathing => "breathing",
            Effect::Rainbow => "rainbow",
            Effect::RainbowSwirl => "rainbow-swirl",
            Effect::Snake => "snake",
            Effect::Christmas => "christmas",
        }
    }

    /// Whether the vendor app sends a color (command 0x28) for this effect
    /// (*verified*: only static, breathing and snake got one in session 1).
    pub fn has_color(self) -> bool {
        matches!(self, Effect::Static | Effect::Breathing | Effect::Snake)
    }

    /// Whether brightness and speed (0x23 / 0x27) are sent for this effect
    /// (*verified*: every effect except Off).
    pub fn has_brightness_and_speed(self) -> bool {
        self != Effect::Off
    }
}

impl fmt::Display for Effect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

impl FromStr for Effect {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let key = s.trim().to_ascii_lowercase().replace([' ', '_'], "-");
        let found = match key.as_str() {
            "off" | "lights-off" | "none" => Effect::Off,
            "static" | "static-light" | "solid" => Effect::Static,
            "breathing" | "breathe" => Effect::Breathing,
            "rainbow" => Effect::Rainbow,
            "rainbow-swirl" | "swirl" => Effect::RainbowSwirl,
            "snake" => Effect::Snake,
            "christmas" | "xmas" => Effect::Christmas,
            _ => {
                return Err(format!(
                    "unknown effect '{s}' (expected one of: {})",
                    Effect::ALL.map(Effect::name).join(", ")
                ))
            }
        };
        Ok(found)
    }
}

/// An RGB color, sent verbatim as command 0x28 (*verified*: `ab 34 56` on the
/// wire for R=171 G=52 B=86 in the app).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Rgb { r, g, b }
    }

    pub const WHITE: Rgb = Rgb::new(0xff, 0xff, 0xff);
    pub const RED: Rgb = Rgb::new(0xff, 0x00, 0x00);
    pub const GREEN: Rgb = Rgb::new(0x00, 0xff, 0x00);
    pub const BLUE: Rgb = Rgb::new(0x00, 0x00, 0xff);

    /// Parses `#rrggbb` or `rrggbb` (case-insensitive).
    pub fn from_hex(s: &str) -> Result<Rgb, String> {
        let hex = s.trim().trim_start_matches('#');
        if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(format!("'{s}' is not an RRGGBB color"));
        }
        let v = u32::from_str_radix(hex, 16).map_err(|e| e.to_string())?;
        Ok(Rgb::new((v >> 16) as u8, (v >> 8) as u8, v as u8))
    }

    /// `#rrggbb`, lowercase.
    pub fn to_hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }

    pub fn to_array(self) -> [u8; 3] {
        [self.r, self.g, self.b]
    }
}

impl fmt::Display for Rgb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl FromStr for Rgb {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Rgb::from_hex(s)
    }
}

impl Serialize for Rgb {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for Rgb {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Rgb::from_hex(&s).map_err(serde::de::Error::custom)
    }
}

/// Brightness byte for command 0x23. The vendor app never sends less than
/// [`Brightness::MIN`] (0x0c): its 0% slider position maps to 0x0c and 100% to
/// 0xff (*verified*). Values are clamped on construction so the mouse never
/// sees an out-of-range byte; settings files deserialize through
/// [`Brightness::new`] too, so `brightness = 0` in a file becomes 0x0c.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct Brightness(u8);

impl<'de> Deserialize<'de> for Brightness {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(Brightness::new(u8::deserialize(d)?))
    }
}

impl Brightness {
    pub const MIN: Brightness = Brightness(0x0c);
    pub const MAX: Brightness = Brightness(0xff);

    /// Clamps into `MIN..=MAX`.
    pub const fn new(raw: u8) -> Brightness {
        if raw < Self::MIN.0 {
            Self::MIN
        } else {
            Brightness(raw)
        }
    }

    /// Linear map from a percentage (0..=100) onto `MIN..=MAX`, the way the
    /// vendor app's slider does (*likely*; 0% → 0x0c and 100% → 0xff verified,
    /// the in-between points were only read approximately).
    pub fn from_percent(percent: u8) -> Brightness {
        let p = percent.min(100) as u32;
        let span = (Self::MAX.0 - Self::MIN.0) as u32;
        Brightness::new((Self::MIN.0 as u32 + (p * span + 50) / 100) as u8)
    }

    pub fn percent(self) -> u8 {
        let span = (Self::MAX.0 - Self::MIN.0) as u32;
        (((self.0 - Self::MIN.0) as u32 * 100 + span / 2) / span) as u8
    }

    pub const fn raw(self) -> u8 {
        self.0
    }
}

impl Default for Brightness {
    fn default() -> Self {
        Brightness::MAX
    }
}

/// Speed byte for command 0x27. The vendor app's slider spans 0x14 (slowest)
/// … 0x89 (middle) … 0xff (fastest); the factory value is 0x80 (*verified*,
/// session 2). Values below [`Speed::MIN`] are clamped, on construction and
/// when read from a settings file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct Speed(u8);

impl<'de> Deserialize<'de> for Speed {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(Speed::new(u8::deserialize(d)?))
    }
}

impl Speed {
    pub const MIN: Speed = Speed(0x14);
    pub const MAX: Speed = Speed(0xff);
    /// Factory value (*verified*: 0x17 reads `80 80 80 80 80 80`).
    pub const DEFAULT: Speed = Speed(0x80);
    /// Kept for older callers.
    pub const OBSERVED: Speed = Speed::DEFAULT;

    /// Clamps into `MIN..=MAX`.
    pub const fn new(raw: u8) -> Speed {
        if raw < Self::MIN.0 {
            Self::MIN
        } else {
            Speed(raw)
        }
    }

    pub const fn raw(self) -> u8 {
        self.0
    }

    /// Linear map from a percentage (0 = slowest) onto `MIN..=MAX`.
    pub fn from_percent(percent: u8) -> Speed {
        let p = percent.min(100) as u32;
        let span = (Self::MAX.0 - Self::MIN.0) as u32;
        Speed::new((Self::MIN.0 as u32 + (p * span + 50) / 100) as u8)
    }

    pub fn percent(self) -> u8 {
        let span = (Self::MAX.0 - Self::MIN.0) as u32;
        (((self.0 - Self::MIN.0) as u32 * 100 + span / 2) / span) as u8
    }

    /// Webcam-measured periods (seconds) at speed bytes 0x14 / 0x4e / 0x80 /
    /// 0xc3 / 0xff for each animated effect (PROTOCOL.md §5, *likely* ±1 frame;
    /// the 0x4e and 0xc3 columns were only measured for breathing, the other
    /// effects interpolate between their measured ends). Returns `None` for
    /// Off and Static.
    pub fn period_seconds(self, effect: Effect) -> Option<f32> {
        const X: [f32; 5] = [
            0x14 as f32,
            0x4e as f32,
            0x80 as f32,
            0xc3 as f32,
            0xff as f32,
        ];
        let y: [f32; 5] = match effect {
            Effect::Breathing => [7.25, 5.9, 3.75, 1.8, 0.7],
            Effect::Rainbow => [7.4, 5.6, 3.8, 2.5, 1.3],
            Effect::RainbowSwirl => [7.5, 5.7, 3.9, 2.3, 0.7],
            Effect::Snake => [2.75, 2.1, 1.4, 0.8, 0.2],
            Effect::Christmas => [3.75, 2.8, 1.9, 1.0, 0.2],
            Effect::Off | Effect::Static => return None,
        };
        let v = self.0 as f32;
        for i in 0..4 {
            if v <= X[i + 1] {
                let t = (v - X[i]) / (X[i + 1] - X[i]);
                return Some(y[i] + t * (y[i + 1] - y[i]));
            }
        }
        Some(y[4])
    }
}

impl Default for Speed {
    fn default() -> Self {
        Speed::DEFAULT
    }
}

/// A complete lighting state, as the vendor app sends it on every change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lighting {
    pub effect: Effect,
    #[serde(default)]
    pub brightness: Brightness,
    #[serde(default)]
    pub speed: Speed,
    /// Only transmitted when [`Effect::has_color`] is true; kept otherwise so
    /// switching back to a color effect restores the user's choice.
    #[serde(default = "default_color")]
    pub color: Rgb,
}

fn default_color() -> Rgb {
    Rgb::WHITE
}

impl Default for Lighting {
    /// The mouse's factory lighting is Rainbow at full brightness, speed 0x80
    /// (*verified*: factory state in PROTOCOL.md §4).
    fn default() -> Self {
        Lighting {
            effect: Effect::Rainbow,
            brightness: Brightness::MAX,
            speed: Speed::DEFAULT,
            color: Rgb::WHITE,
        }
    }
}

impl Lighting {
    pub fn static_color(color: Rgb) -> Self {
        Lighting {
            effect: Effect::Static,
            color,
            ..Default::default()
        }
    }

    /// The write sequence the vendor app sends for this state, in its order:
    /// 0x22 effect, then (unless Off) 0x23 brightness, 0x27 speed, and 0x28
    /// color for the effects that take one (*verified* for all seven effects).
    pub fn commands(&self) -> Vec<Command> {
        let e = self.effect;
        let mut out = vec![Command::SetEffect(e)];
        if e.has_brightness_and_speed() {
            out.push(Command::SetBrightness {
                effect: e,
                brightness: self.brightness,
            });
            out.push(Command::SetSpeed {
                effect: e,
                speed: self.speed,
            });
        }
        if e.has_color() {
            out.push(Command::SetColor {
                effect: e,
                color: self.color,
            });
        }
        out
    }

    /// Encoded reports for [`Lighting::commands`].
    pub fn reports(&self) -> Vec<Vec<u8>> {
        self.commands().iter().map(Command::encode).collect()
    }

    /// The parts of this state the mouse actually shows: Off ignores every
    /// parameter, and effects without a color ignore the color (0x28 is not
    /// sent for them, so the stored color is irrelevant). Comparisons for
    /// "does the mouse already match?" use this.
    pub fn visible(&self) -> Lighting {
        let mut l = *self;
        if l.effect == Effect::Off {
            l.brightness = Brightness::MAX;
            l.speed = Speed::DEFAULT;
        }
        if !l.effect.has_color() {
            l.color = Rgb::WHITE;
        }
        l
    }

    /// True when the mouse would look the same with either state.
    pub fn same_visible(&self, other: &Lighting) -> bool {
        self.visible() == other.visible()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effect_ids_match_capture() {
        assert_eq!(Effect::Off.id(), 0);
        assert_eq!(Effect::Static.id(), 1);
        assert_eq!(Effect::Breathing.id(), 2);
        assert_eq!(Effect::Rainbow.id(), 3);
        assert_eq!(Effect::RainbowSwirl.id(), 4);
        assert_eq!(Effect::Snake.id(), 5);
        assert_eq!(Effect::Christmas.id(), 6);
        for e in Effect::ALL {
            assert_eq!(Effect::from_id(e.id()), Some(e));
            assert_eq!(e.name().parse::<Effect>().unwrap(), e);
            assert_eq!(e.label().parse::<Effect>().unwrap(), e);
        }
        assert_eq!(Effect::from_id(7), None);
        assert!("disco".parse::<Effect>().is_err());
    }

    #[test]
    fn color_effects() {
        let with_color: Vec<_> = Effect::ALL.iter().filter(|e| e.has_color()).collect();
        assert_eq!(
            with_color,
            vec![&Effect::Static, &Effect::Breathing, &Effect::Snake]
        );
        assert!(!Effect::Off.has_brightness_and_speed());
        assert!(Effect::Christmas.has_brightness_and_speed());
    }

    #[test]
    fn rgb_hex_round_trip() {
        let c = Rgb::from_hex("#AB3456").unwrap();
        assert_eq!(c, Rgb::new(0xab, 0x34, 0x56));
        assert_eq!(c.to_hex(), "#ab3456");
        assert_eq!(Rgb::from_hex("00ff06").unwrap(), Rgb::new(0, 0xff, 6));
        assert!(Rgb::from_hex("#fff").is_err());
        assert!(Rgb::from_hex("gg0000").is_err());
    }

    #[test]
    fn brightness_floor() {
        assert_eq!(Brightness::new(0).raw(), 0x0c);
        assert_eq!(Brightness::new(0x0b).raw(), 0x0c);
        assert_eq!(Brightness::new(0x0c).raw(), 0x0c);
        assert_eq!(Brightness::new(0xff).raw(), 0xff);
        assert_eq!(Brightness::from_percent(0).raw(), 0x0c);
        assert_eq!(Brightness::from_percent(100).raw(), 0xff);
        assert_eq!(Brightness::from_percent(200).raw(), 0xff);
        // The app's ~25% and ~50% slider positions produced 0x49 and 0x85;
        // our linear map lands within a couple of counts of both.
        assert!((Brightness::from_percent(25).raw() as i32 - 0x49).abs() <= 2);
        assert!((Brightness::from_percent(50).raw() as i32 - 0x85).abs() <= 2);
        assert_eq!(Brightness::MAX.percent(), 100);
        assert_eq!(Brightness::MIN.percent(), 0);
    }

    #[test]
    fn speed_range_and_periods() {
        assert_eq!(Speed::new(0).raw(), 0x14);
        assert_eq!(Speed::new(0x13).raw(), 0x14);
        assert_eq!(Speed::new(0xff).raw(), 0xff);
        assert_eq!(Speed::from_percent(0).raw(), 0x14);
        assert_eq!(Speed::from_percent(100).raw(), 0xff);
        assert_eq!(Speed::MIN.percent(), 0);
        assert_eq!(Speed::MAX.percent(), 100);
        let p = |raw: u8| Speed::new(raw).period_seconds(Effect::Breathing).unwrap();
        assert!((p(0x14) - 7.25).abs() < 1e-5);
        assert!((p(0x80) - 3.75).abs() < 1e-5);
        assert!((p(0xff) - 0.7).abs() < 1e-5);
        assert!(p(0x4e) > p(0x80) && p(0x80) > p(0xc3));
        assert!(Speed::DEFAULT.period_seconds(Effect::Static).is_none());
        assert!((Speed::new(0x14).period_seconds(Effect::Snake).unwrap() - 2.75).abs() < 1e-5);
    }

    #[test]
    fn serde_names() {
        let l = Lighting::static_color(Rgb::new(0x12, 0x34, 0x56));
        let t = toml::to_string(&l).unwrap();
        assert!(t.contains("effect = \"static\""), "{t}");
        assert!(t.contains("color = \"#123456\""), "{t}");
        let back: Lighting = toml::from_str(&t).unwrap();
        assert_eq!(back, l);
        let swirl: Lighting = toml::from_str("effect = \"rainbow-swirl\"").unwrap();
        assert_eq!(swirl.effect, Effect::RainbowSwirl);
        assert_eq!(swirl.brightness, Brightness::MAX);
        assert_eq!(swirl.speed, Speed::DEFAULT);
    }

    #[test]
    fn files_cannot_carry_bytes_below_the_floors() {
        let l: Lighting =
            toml::from_str("effect = \"breathing\"\nbrightness = 0\nspeed = 3\n").unwrap();
        assert_eq!(l.brightness, Brightness::MIN);
        assert_eq!(l.speed, Speed::MIN);
        assert!(toml::from_str::<Lighting>("effect = \"static\"\nbrightness = 300\n").is_err());
    }

    #[test]
    fn visible_comparison_ignores_what_the_mouse_does_not_show() {
        let rainbow = Lighting::default();
        let red_rainbow = Lighting {
            color: Rgb::RED,
            ..rainbow
        };
        assert!(rainbow.same_visible(&red_rainbow));
        let off_a = Lighting {
            effect: Effect::Off,
            ..rainbow
        };
        let off_b = Lighting {
            effect: Effect::Off,
            brightness: Brightness::new(0x20),
            ..red_rainbow
        };
        assert!(off_a.same_visible(&off_b));
        let green = Lighting::static_color(Rgb::GREEN);
        assert!(!green.same_visible(&Lighting::static_color(Rgb::RED)));
        assert!(!rainbow.same_visible(&Lighting {
            speed: Speed::new(0x40),
            ..rainbow
        }));
    }
}
