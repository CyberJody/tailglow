// SPDX-License-Identifier: GPL-3.0-or-later
//! Light / dark theme: the user's choice (saved in `gui.toml`), what the
//! desktop prefers, and the palettes.
//!
//! "System" follows the freedesktop appearance portal
//! (`org.freedesktop.portal.Settings`, key `org.freedesktop.appearance
//! color-scheme`: 0 = no preference, 1 = prefer dark, 2 = prefer light) and
//! its `SettingChanged` signal, so a desktop theme switch shows up while the
//! app is open. No preference, no portal or no session bus all mean light.
//!
//! We read the portal ourselves (through `mundy`, the crate iced uses for the
//! same job) instead of relying on iced's built-in system theme: we need the
//! value for the "System (dark)" label anyway, and iced 0.14 on X11 resets
//! its system theme to winit's window theme (X11 reports none) when the
//! window opens, so it is right only if the portal's first answer arrives
//! after that (a race, read from iced_winit 0.14.1 lib.rs; it usually wins).

use std::path::{Path, PathBuf};

use iced::{Color, Theme};
use serde::{Deserialize, Serialize};

/// The theme picker's setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
    #[default]
    System,
    Light,
    Dark,
}

impl ThemeMode {
    pub const ALL: [ThemeMode; 3] = [ThemeMode::System, ThemeMode::Light, ThemeMode::Dark];

    pub fn parse(s: &str) -> Option<ThemeMode> {
        match s.trim().to_ascii_lowercase().as_str() {
            "system" | "auto" => Some(ThemeMode::System),
            "light" => Some(ThemeMode::Light),
            "dark" => Some(ThemeMode::Dark),
            _ => None,
        }
    }
}

/// What the desktop says about light / dark.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SystemScheme {
    /// Not asked yet (the first portal answer is still on its way).
    #[default]
    Unknown,
    /// The portal answered 0, has no such key, or there is no portal.
    NoPreference,
    Light,
    Dark,
}

impl SystemScheme {
    pub fn is_dark(self) -> bool {
        self == SystemScheme::Dark
    }
}

/// The theme the window gets: `Dark` only when chosen, or when "System" and
/// the desktop prefers dark. Everything else is light.
pub fn resolve(mode: ThemeMode, system: SystemScheme) -> Theme {
    let dark = match mode {
        ThemeMode::Light => false,
        ThemeMode::Dark => true,
        ThemeMode::System => system.is_dark(),
    };
    if dark {
        dark_theme()
    } else {
        light_theme()
    }
}

/// A picker entry; "System" shows what it resolved to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThemeChoice {
    pub mode: ThemeMode,
    pub system: SystemScheme,
}

impl std::fmt::Display for ThemeChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.mode {
            ThemeMode::System if self.system.is_dark() => f.write_str("System (dark)"),
            ThemeMode::System => f.write_str("System (light)"),
            ThemeMode::Light => f.write_str("Light"),
            ThemeMode::Dark => f.write_str("Dark"),
        }
    }
}

/// A line under the picker when "System" needs explaining (the label
/// already says light or dark when the desktop has a preference).
pub fn system_note(system: SystemScheme) -> Option<&'static str> {
    match system {
        SystemScheme::Unknown => Some("Asking the desktop…"),
        SystemScheme::NoPreference => {
            Some("The desktop sets no light/dark preference, so light is used.")
        }
        SystemScheme::Light | SystemScheme::Dark => None,
    }
}

const fn rgb8(r: u8, g: u8, b: u8) -> Color {
    Color::from_rgb8(r, g, b)
}

/// The light palette. iced's generated light surfaces get darker with a
/// blue cast (its `darken` bumps chroma), so panels and tiles came out
/// blue-gray and *darker* than the page, which reads as an inverted dark
/// theme. Here the surfaces are set by hand: a neutral light-gray page,
/// white raised panels/tiles/sidebar, gray borders; a deeper blue primary
/// so white text on it passes WCAG AA.
pub fn light_theme() -> Theme {
    static LIGHT: std::sync::LazyLock<Theme> = std::sync::LazyLock::new(|| {
        Theme::custom_with_fn(
            "Tailglow light",
            iced::theme::Palette {
                background: rgb8(0xee, 0xf0, 0xf3),
                text: rgb8(0x1d, 0x21, 0x27),
                primary: rgb8(0x2f, 0x5f, 0xd0),
                success: rgb8(0x1e, 0x8a, 0x47),
                warning: rgb8(0xb7, 0x79, 0x1f),
                danger: rgb8(0xc4, 0x2b, 0x2b),
            },
            |p| {
                use iced::theme::palette::{Background, Extended, Pair, Secondary};
                let t = p.text;
                let pair = |c: Color| Pair::new(c, t);
                let mut e = Extended::generate(p);
                e.background = Background {
                    base: pair(p.background),              // page
                    weakest: pair(rgb8(0xf4, 0xf5, 0xf7)), // hover on white
                    weaker: pair(rgb8(0xf9, 0xfa, 0xfb)),
                    weak: pair(Color::WHITE), // panels, sidebar, lists
                    neutral: pair(rgb8(0xe3, 0xe6, 0xea)),
                    strong: pair(rgb8(0xc3, 0xc9, 0xd2)), // borders, rails
                    stronger: pair(rgb8(0xb4, 0xbb, 0xc5)),
                    strongest: pair(rgb8(0x8a, 0x93, 0x9f)),
                };
                e.secondary = Secondary {
                    base: pair(rgb8(0xe6, 0xe9, 0xee)),
                    weak: pair(rgb8(0xf0, 0xf2, 0xf5)),
                    strong: pair(rgb8(0xd5, 0xda, 0xe1)),
                };
                e
            },
        )
    });
    LIGHT.clone()
}

pub fn is_dark(theme: &Theme) -> bool {
    theme.extended_palette().is_dark
}

/// The dark palette: iced's built-in Dark (unchanged look).
pub fn dark_theme() -> Theme {
    Theme::Dark
}

/// Colors that carry meaning (status lines, battery, warnings), picked per
/// theme so they keep contrast: the bright ones read well on dark gray but
/// are too pale on white.
#[derive(Debug, Clone, Copy)]
pub struct Signals {
    pub ok: Color,
    pub warn: Color,
    pub error: Color,
}

pub fn signals(theme: &Theme) -> Signals {
    if theme.extended_palette().is_dark {
        Signals {
            ok: Color::from_rgb(0.25, 0.65, 0.35),
            warn: Color::from_rgb(0.9, 0.35, 0.2),
            error: Color::from_rgb(0.9, 0.3, 0.2),
        }
    } else {
        Signals {
            ok: Color::from_rgb8(0x1e, 0x7a, 0x3e),
            warn: Color::from_rgb8(0xb4, 0x3c, 0x12),
            error: Color::from_rgb8(0xb3, 0x26, 0x1e),
        }
    }
}

/// Initial read, with a short timeout so a missing portal never delays the
/// window (and a dark desktop does not flash light first).
#[cfg(target_os = "linux")]
pub fn read_system_scheme() -> SystemScheme {
    mundy::Preferences::once_blocking(
        mundy::Interest::ColorScheme,
        std::time::Duration::from_millis(300),
    )
    .map(|p| from_mundy(p.color_scheme))
    .unwrap_or(SystemScheme::NoPreference)
}

#[cfg(not(target_os = "linux"))]
pub fn read_system_scheme() -> SystemScheme {
    SystemScheme::Unknown
}

#[cfg(target_os = "linux")]
fn from_mundy(c: mundy::ColorScheme) -> SystemScheme {
    match c {
        mundy::ColorScheme::Dark => SystemScheme::Dark,
        mundy::ColorScheme::Light => SystemScheme::Light,
        mundy::ColorScheme::NoPreference => SystemScheme::NoPreference,
    }
}

/// The portal's current value, then one item per `SettingChanged` for
/// `color-scheme`. Without a portal it yields `NoPreference` once and ends.
#[cfg(target_os = "linux")]
pub fn system_scheme_stream() -> impl iced::futures::Stream<Item = SystemScheme> {
    use iced::futures::StreamExt;
    mundy::Preferences::stream(mundy::Interest::ColorScheme).map(|p| from_mundy(p.color_scheme))
}

/// Subscription that keeps [`SystemScheme`] current.
#[cfg(target_os = "linux")]
pub fn system_scheme_changes() -> iced::Subscription<SystemScheme> {
    iced::Subscription::run(system_scheme_stream)
}

/// Elsewhere winit knows the OS theme, so iced's own subscription works.
#[cfg(not(target_os = "linux"))]
pub fn system_scheme_changes() -> iced::Subscription<SystemScheme> {
    iced::system::theme_changes().map(|m| match m {
        iced::theme::Mode::Dark => SystemScheme::Dark,
        iced::theme::Mode::Light => SystemScheme::Light,
        iced::theme::Mode::None => SystemScheme::NoPreference,
    })
}

/// The GUI's own settings (the mouse settings live in `state.toml`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GuiConfig {
    pub theme: ThemeMode,
    /// The Lighting preview's render view.
    pub preview: crate::render::View,
}

impl GuiConfig {
    /// Missing or unreadable file: defaults (System).
    pub fn load(path: &Path) -> GuiConfig {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|t| toml::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        let text = toml::to_string(self).map_err(|e| e.to_string())?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        std::fs::write(path, text).map_err(|e| e.to_string())
    }
}

/// `$TAILGLOW_GUI_CONFIG`, else `gui.toml` in the app's config dir
/// (`$XDG_CONFIG_HOME/tailglow/gui.toml`).
pub fn config_path(config_dir: PathBuf) -> PathBuf {
    if let Some(p) = std::env::var_os("TAILGLOW_GUI_CONFIG") {
        return PathBuf::from(p);
    }
    config_dir.join("gui.toml")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_resolves_from_the_portal_and_defaults_to_light() {
        let dark = |t: Theme| t.extended_palette().is_dark;
        assert!(dark(resolve(ThemeMode::System, SystemScheme::Dark)));
        assert!(!dark(resolve(ThemeMode::System, SystemScheme::Light)));
        assert!(!dark(resolve(
            ThemeMode::System,
            SystemScheme::NoPreference
        )));
        assert!(!dark(resolve(ThemeMode::System, SystemScheme::Unknown)));
        // Manual choices ignore the desktop.
        assert!(!dark(resolve(ThemeMode::Light, SystemScheme::Dark)));
        assert!(dark(resolve(ThemeMode::Dark, SystemScheme::Light)));
    }

    #[test]
    fn picker_shows_what_system_resolved_to() {
        let c = |mode, system| ThemeChoice { mode, system }.to_string();
        assert_eq!(c(ThemeMode::System, SystemScheme::Dark), "System (dark)");
        assert_eq!(c(ThemeMode::System, SystemScheme::Light), "System (light)");
        assert_eq!(
            c(ThemeMode::System, SystemScheme::NoPreference),
            "System (light)"
        );
        assert_eq!(c(ThemeMode::Dark, SystemScheme::Light), "Dark");
    }

    #[test]
    fn config_round_trips_and_tolerates_junk() {
        let dir = std::env::temp_dir().join(format!("tailglow-gui-cfg-{}", std::process::id()));
        let path = dir.join("sub").join("gui.toml");
        assert_eq!(GuiConfig::load(&path).theme, ThemeMode::System, "missing");
        assert_eq!(GuiConfig::load(&path).preview, crate::render::View::Hero);
        GuiConfig {
            theme: ThemeMode::Dark,
            preview: crate::render::View::Top,
        }
        .save(&path)
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "theme = \"dark\"\npreview = \"top\"\n"
        );
        assert_eq!(GuiConfig::load(&path).theme, ThemeMode::Dark);
        assert_eq!(GuiConfig::load(&path).preview, crate::render::View::Top);
        // A file from before the preview setting still loads.
        std::fs::write(&path, "theme = \"light\"\n").unwrap();
        assert_eq!(GuiConfig::load(&path).theme, ThemeMode::Light);
        assert_eq!(GuiConfig::load(&path).preview, crate::render::View::Hero);
        std::fs::write(&path, "theme = \"purple\"\n").unwrap();
        assert_eq!(GuiConfig::load(&path).theme, ThemeMode::System, "junk");
        std::fs::write(&path, "other = 1\n").unwrap();
        assert_eq!(GuiConfig::load(&path).theme, ThemeMode::System, "no key");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn theme_mode_parse() {
        assert_eq!(ThemeMode::parse("Dark"), Some(ThemeMode::Dark));
        assert_eq!(ThemeMode::parse("light"), Some(ThemeMode::Light));
        assert_eq!(ThemeMode::parse("system"), Some(ThemeMode::System));
        assert_eq!(ThemeMode::parse("blue"), None);
    }

    /// Light status colors keep at least WCAG AA (4.5:1) on the light panel.
    #[test]
    fn light_signals_have_contrast() {
        let t = light_theme();
        let bg = t.extended_palette().background.weak.color;
        let s = signals(&t);
        for c in [s.ok, s.warn, s.error, t.palette().text] {
            assert!(
                contrast(c, bg) >= 4.5,
                "{c:?} on {bg:?}: {}",
                contrast(c, bg)
            );
        }
        // White on the primary color (selected tiles, primary buttons).
        let p = t.extended_palette().primary.base;
        assert!(
            contrast(p.text, p.color) >= 4.5,
            "{}",
            contrast(p.text, p.color)
        );
    }

    pub(crate) fn contrast(a: Color, b: Color) -> f32 {
        fn lum(c: Color) -> f32 {
            let f = |v: f32| {
                if v <= 0.03928 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * f(c.r) + 0.7152 * f(c.g) + 0.0722 * f(c.b)
        }
        let (x, y) = (lum(a), lum(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }
}
