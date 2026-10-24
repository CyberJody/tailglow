// SPDX-License-Identifier: GPL-3.0-or-later
//! `tailglow-tray`: StatusNotifierItem tray icon for Keychron mice.
//!
//! - Connection (USB cable or Bluetooth) and battery level in the tooltip/menu.
//!   Over the cable the battery is polled every `--poll-secs` (default 60 s);
//!   over Bluetooth it comes from BlueZ (`org.bluez.Battery1`) and updates
//!   live. One desktop notification at 15 % or lower, once per discharge.
//! - Over Bluetooth the mouse takes no settings (Keychron's firmware), so the
//!   menu shows status only and says the cable is needed.
//! - Quick menu: lighting effect, colour, active DPI stage.
//! - Per-application profiles (X11 only): watches `_NET_ACTIVE_WINDOW`, maps the
//!   active window's `WM_CLASS` to a settings file via `~/.config/tailglow/apps.toml`
//!   and writes only the differences (rate limited). On Wayland this part is
//!   disabled and says so.
//!
//! The device is opened for each operation and closed again, so the CLI and
//! the GUI can run alongside. Writes to untested models need `--allow-untested`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::Parser;
use ksni::blocking::TrayMethods;
use ksni::menu::{MenuItem, RadioGroup, RadioItem, StandardItem, SubMenu};
use ksni::ToolTip;
use serde::Deserialize;

use tailglow_core::pointer::Dpi;
use tailglow_core::{
    ApplyOptions, ApplyOutcome, Effect, Lighting, Mouse, Rgb, Settings, Snapshot, Transport,
};
use tailglow_hid::bluez::{BtMouse, BtState, Watcher};

#[derive(Parser)]
#[command(
    name = "tailglow-tray",
    version,
    about = "Tailglow tray icon: battery, connection and quick settings for Keychron mice"
)]
struct Cli {
    /// Allow writes to likely-compatible but untested models.
    #[arg(long)]
    allow_untested: bool,
    /// Battery poll interval in seconds.
    #[arg(long, default_value_t = 60)]
    poll_secs: u64,
    /// Per-application profile map (default: $XDG_CONFIG_HOME/tailglow/apps.toml).
    #[arg(long, value_name = "FILE")]
    apps: Option<PathBuf>,
    /// Disable per-application profiles.
    #[arg(long)]
    no_apps: bool,
    /// Print what the per-app watcher sees (WM_CLASS) without writing.
    #[arg(long)]
    dry_run: bool,
}

/// `apps.toml`:
/// ```toml
/// default = "~/.config/tailglow/profiles/desktop.toml"   # optional
/// [apps]
/// firefox = "~/.config/tailglow/profiles/browser.toml"
/// "org.inkscape.Inkscape" = "~/.config/tailglow/profiles/drawing.toml"
/// ```
#[derive(Debug, Default, Deserialize)]
struct AppsConfig {
    #[serde(default)]
    default: Option<String>,
    #[serde(default)]
    apps: BTreeMap<String, String>,
}

impl AppsConfig {
    fn load(path: &Path) -> Result<AppsConfig> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Ok(toml::from_str(&text)?)
    }

    /// Profile for a window class/instance (case-insensitive), else the default.
    fn profile_for(&self, classes: &[String]) -> Option<PathBuf> {
        for c in classes {
            let lc = c.to_ascii_lowercase();
            if let Some((_, p)) = self.apps.iter().find(|(k, _)| k.to_ascii_lowercase() == lc) {
                return Some(expand(p));
            }
        }
        self.default.as_deref().map(expand)
    }
}

fn expand(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(p)
}

fn config_dir() -> PathBuf {
    tailglow_hid::paths::config_dir().unwrap_or_else(|| PathBuf::from("."))
}

/// State shared between the tray, the battery poller and the app watcher.
struct Shared {
    allow_untested: bool,
    snapshot: Option<Snapshot>,
    error: Option<String>,
    /// Bluetooth side, from BlueZ (status only).
    bt: BtState,
    /// Low-battery notification state.
    low: LowBattery,
    last_apply: Option<Instant>,
    current_profile: Option<PathBuf>,
}

type State = Arc<Mutex<Shared>>;

fn open_mouse(allow_untested: bool) -> Result<Mouse<Box<dyn Transport>>> {
    let transport = tailglow_hid::open(false, None)?;
    let mut mouse = Mouse::new(transport);
    mouse.allow_untested(allow_untested);
    Ok(mouse)
}

/// Opens the mouse, runs `f`, re-reads the snapshot into the shared state.
fn with_mouse<R>(
    state: &State,
    f: impl FnOnce(&mut Mouse<Box<dyn Transport>>, &Snapshot) -> Result<R>,
) -> Result<R> {
    let allow = state.lock().unwrap().allow_untested;
    let result = (|| {
        let mut mouse = open_mouse(allow)?;
        let snap = mouse.snapshot()?;
        let r = f(&mut mouse, &snap)?;
        let after = mouse.snapshot()?;
        Ok((r, after))
    })();
    let mut s = state.lock().unwrap();
    match result {
        Ok((r, after)) => {
            s.snapshot = Some(after);
            s.error = None;
            Ok(r)
        }
        Err(e) => {
            s.error = Some(format!("{e:#}"));
            Err(e)
        }
    }
}

fn refresh(state: &State) {
    let _ = with_mouse(state, |_, _| Ok(()));
}

/// The state file shared with the CLI and GUI (`TAILGLOW_STATE` overrides).
fn state_file() -> PathBuf {
    std::env::var_os("TAILGLOW_STATE")
        .map(PathBuf::from)
        .unwrap_or_else(|| config_dir().join("state.toml"))
}

/// Writes only what differs between `settings` and the mouse (macros the
/// state file says are already on the mouse are skipped), reads the mouse
/// back and compares, then records the result in the state file like the
/// CLI and GUI do. Mismatches end up in the tooltip's error line.
fn apply_settings(state: &State, settings: &Settings) -> Result<ApplyOutcome> {
    let path = state_file();
    let mut remembered = load_settings(&path).unwrap_or_default();
    let opts = ApplyOptions {
        force: false,
        sent_macros: remembered.sent_macros.clone(),
    };
    let out = with_mouse(state, |mouse, _| Ok(mouse.apply_settings(settings, &opts)?))?;
    remembered.record_applied(settings);
    let new_state = Settings::from_snapshot_with_macros(&out.snapshot, &remembered);
    if let (Some(dir), Ok(text)) = (path.parent(), new_state.to_toml()) {
        let _ = std::fs::create_dir_all(dir);
        if let Err(e) = std::fs::write(&path, text) {
            eprintln!("tailglow-tray: writing {}: {e}", path.display());
        }
    }
    if !out.mismatches.is_empty() {
        let msg = format!("read-back mismatch: {}", out.mismatches.join("; "));
        eprintln!("tailglow-tray: {msg}");
        state.lock().unwrap().error = Some(msg);
    }
    Ok(out)
}

/// The app icon when it is installed (the .deb puts it in hicolor), else
/// the theme's generic mouse icon.
fn tray_icon_name() -> String {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(home) = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
    {
        dirs.push(home);
    }
    let sys = std::env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    dirs.extend(sys.split(':').map(PathBuf::from));
    let installed = dirs.iter().any(|d| {
        d.join("icons/hicolor/scalable/apps/io.github.cyberjody.Tailglow.svg")
            .exists()
    });
    if installed {
        "io.github.cyberjody.Tailglow".into()
    } else {
        "input-mouse".into()
    }
}

/// Desktop notification over D-Bus (org.freedesktop.Notifications).
fn notify(summary: &str, body: &str, urgency: u8) -> Result<()> {
    let conn = zbus::blocking::Connection::session()?;
    let mut hints: std::collections::HashMap<&str, zbus::zvariant::Value> =
        std::collections::HashMap::new();
    hints.insert("urgency", zbus::zvariant::Value::U8(urgency));
    conn.call_method(
        Some("org.freedesktop.Notifications"),
        "/org/freedesktop/Notifications",
        Some("org.freedesktop.Notifications"),
        "Notify",
        &(
            "Tailglow",
            0u32,
            tray_icon_name(),
            summary,
            body,
            Vec::<&str>::new(),
            hints,
            10_000i32,
        ),
    )?;
    Ok(())
}

// ----- tray -----------------------------------------------------------------

struct KcTray {
    state: State,
}

const PRESETS: [(&str, Rgb); 8] = [
    ("Red", Rgb::RED),
    ("Green", Rgb::new(0x00, 0xff, 0x06)),
    ("Blue", Rgb::new(0x00, 0x0c, 0xff)),
    ("White", Rgb::WHITE),
    ("Orange", Rgb::new(0xff, 0x48, 0x00)),
    ("Cyan", Rgb::new(0x00, 0xff, 0xfc)),
    ("Magenta", Rgb::new(0xf0, 0x00, 0xff)),
    ("Yellow", Rgb::new(0xff, 0xde, 0x00)),
];

impl KcTray {
    fn set_effect(state: &State, effect: Effect) {
        let _ = with_mouse(state, |mouse, snap| {
            let l = Lighting {
                effect,
                ..snap.lighting.for_effect(effect)
            };
            mouse.apply_lighting(&l)?;
            Ok(())
        });
    }

    fn set_color(state: &State, color: Rgb) {
        let _ = with_mouse(state, |mouse, snap| {
            let cur = snap.lighting.current();
            let effect = if cur.effect.has_color() {
                cur.effect
            } else {
                Effect::Static
            };
            let l = Lighting {
                color,
                ..snap.lighting.for_effect(effect)
            };
            mouse.apply_lighting(&l)?;
            Ok(())
        });
    }

    fn set_dpi_stage(state: &State, stage: u8) {
        let _ = with_mouse(state, |mouse, snap| {
            let d = Dpi {
                active: stage,
                ..snap.info.pointer.dpi
            };
            mouse.apply_dpi(&d)?;
            Ok(())
        });
    }
}

impl ksni::Tray for KcTray {
    const MENU_ON_ACTIVATE: bool = true;

    fn id(&self) -> String {
        "tailglow".into()
    }

    fn title(&self) -> String {
        "Tailglow".into()
    }

    fn icon_name(&self) -> String {
        tray_icon_name()
    }

    fn tool_tip(&self) -> ToolTip {
        let s = self.state.lock().unwrap();
        let bt = s.bt.connected();
        let (title, description) = match (&s.snapshot, &s.error) {
            (None, _) if bt.is_some() => {
                let m = bt.unwrap();
                (
                    format!("{} — Bluetooth — {}", m.model, bt_battery(m)),
                    "Settings can only be changed over the USB cable \
                     (Keychron's firmware doesn't support it over Bluetooth)."
                        .into(),
                )
            }
            (Some(snap), _) => (
                format!(
                    "{} — USB cable — battery {}%",
                    snap.info.model_name(),
                    snap.info.device.battery_percent
                ),
                format!(
                    "{} · {} DPI · {}",
                    snap.lighting.current().effect.label(),
                    snap.info.pointer.dpi.active_dpi(),
                    snap.info
                        .pointer
                        .report_rate()
                        .map(|r| r.to_string())
                        .unwrap_or_default()
                ),
            ),
            (None, Some(e)) => ("Tailglow: no mouse".into(), e.clone()),
            (None, None) => ("Tailglow".into(), "looking for a Keychron mouse…".into()),
        };
        ToolTip {
            icon_name: "input-mouse".into(),
            title,
            description,
            ..Default::default()
        }
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let s = self.state.lock().unwrap();
        if let (None, Some(m)) = (&s.snapshot, s.bt.connected()) {
            return vec![
                StandardItem {
                    label: format!("{} · Bluetooth · {}", m.model, bt_battery(m)),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
                StandardItem {
                    label: "Settings can only be changed over the USB cable".into(),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
                MenuItem::Separator,
                open_gui_item(),
                StandardItem {
                    label: "Refresh".into(),
                    activate: Box::new(|t: &mut Self| refresh(&t.state)),
                    ..Default::default()
                }
                .into(),
                MenuItem::Separator,
                quit_item(),
            ];
        }
        let Some(snap) = &s.snapshot else {
            let msg = s
                .error
                .clone()
                .unwrap_or_else(|| "Looking for a Keychron mouse…".into());
            return vec![
                StandardItem {
                    label: msg,
                    enabled: false,
                    ..Default::default()
                }
                .into(),
                StandardItem {
                    label: "Retry".into(),
                    activate: Box::new(|t: &mut Self| refresh(&t.state)),
                    ..Default::default()
                }
                .into(),
                MenuItem::Separator,
                quit_item(),
            ];
        };
        let cur = snap.lighting.current();
        let effects: Vec<RadioItem> = Effect::ALL
            .iter()
            .map(|e| RadioItem {
                label: e.label().into(),
                ..Default::default()
            })
            .collect();
        let selected_effect = Effect::ALL
            .iter()
            .position(|e| *e == cur.effect)
            .unwrap_or(0);
        let dpi = snap.info.pointer.dpi;
        let stages: Vec<RadioItem> = dpi
            .stages
            .iter()
            .enumerate()
            .map(|(i, v)| RadioItem {
                label: format!("Stage {} — {v} DPI ({})", i + 1, Dpi::INDICATOR_COLORS[i]),
                ..Default::default()
            })
            .collect();
        let colors: Vec<MenuItem<Self>> = PRESETS
            .iter()
            .map(|(name, c)| {
                let c = *c;
                StandardItem {
                    label: format!("{name} {}", c.to_hex()),
                    activate: Box::new(move |t: &mut Self| KcTray::set_color(&t.state, c)),
                    ..Default::default()
                }
                .into()
            })
            .collect();
        let battery = snap.info.device.battery_percent;
        let untested_note = if snap.info.status() == Some(tailglow_core::SupportStatus::Likely)
            && !s.allow_untested
        {
            Some(
                StandardItem {
                    label: "Untested model: writes disabled (run with --allow-untested)".into(),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            )
        } else {
            None
        };
        let profile_note = s.current_profile.as_ref().map(|p| {
            StandardItem {
                label: format!(
                    "Per-app profile: {}",
                    p.file_name()
                        .map(|f| f.to_string_lossy().into_owned())
                        .unwrap_or_default()
                ),
                enabled: false,
                ..Default::default()
            }
            .into()
        });
        let mut items: Vec<MenuItem<Self>> = vec![StandardItem {
            label: format!(
                "{} · USB cable · battery {battery}% · firmware {}",
                snap.info.model_name(),
                snap.info.firmware.version
            ),
            enabled: false,
            ..Default::default()
        }
        .into()];
        if let Some(n) = untested_note {
            items.push(n);
        }
        if let Some(n) = profile_note {
            items.push(n);
        }
        items.extend([
            MenuItem::Separator,
            SubMenu {
                label: "Lighting effect".into(),
                submenu: vec![RadioGroup {
                    selected: selected_effect,
                    select: Box::new(|t: &mut Self, i| {
                        KcTray::set_effect(&t.state, Effect::ALL[i])
                    }),
                    options: effects,
                }
                .into()],
                ..Default::default()
            }
            .into(),
            SubMenu {
                label: "Color".into(),
                submenu: colors,
                ..Default::default()
            }
            .into(),
            SubMenu {
                label: format!("DPI ({} active)", dpi.active_dpi()),
                submenu: vec![RadioGroup {
                    selected: dpi.active as usize,
                    select: Box::new(|t: &mut Self, i| KcTray::set_dpi_stage(&t.state, i as u8)),
                    options: stages,
                }
                .into()],
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            open_gui_item(),
            StandardItem {
                label: "Re-apply saved settings".into(),
                activate: Box::new(|t: &mut Self| {
                    if let Some(settings) = load_settings(&state_file()) {
                        let _ = apply_settings(&t.state, &settings);
                    }
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Refresh".into(),
                activate: Box::new(|t: &mut Self| refresh(&t.state)),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            quit_item(),
        ]);
        items
    }
}

fn open_gui_item() -> MenuItem<KcTray> {
    StandardItem {
        label: "Open Tailglow".into(),
        icon_name: "preferences-desktop".into(),
        activate: Box::new(|_: &mut KcTray| {
            let _ = std::process::Command::new("tailglow-gui").spawn();
        }),
        ..Default::default()
    }
    .into()
}

fn bt_battery(m: &BtMouse) -> String {
    m.battery
        .map(|b| format!("battery {b}%"))
        .unwrap_or_else(|| "battery unknown".into())
}

/// One low-battery notification per discharge: fires the first time the
/// level is at or below [`LowBattery::THRESHOLD`], and re-arms only once the
/// mouse has been charged to [`LowBattery::REARM`] or more.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct LowBattery {
    notified: bool,
}

impl LowBattery {
    const THRESHOLD: u8 = 15;
    const REARM: u8 = 25;

    /// Feeds one reading; true when a notification should be shown now.
    fn update(&mut self, percent: u8) -> bool {
        if percent <= Self::THRESHOLD {
            if !self.notified {
                self.notified = true;
                return true;
            }
        } else if percent >= Self::REARM {
            self.notified = false;
        }
        false
    }
}

/// The battery to watch: the cable's reading if connected, else Bluetooth's.
fn current_battery(s: &Shared) -> Option<u8> {
    s.snapshot
        .as_ref()
        .map(|snap| snap.info.device.battery_percent)
        .or_else(|| s.bt.connected().and_then(|m| m.battery))
}

/// Notifies once per discharge (see [`LowBattery`]).
fn check_low_battery(state: &State) {
    let fire = {
        let mut s = state.lock().unwrap();
        match current_battery(&s) {
            Some(b) => s.low.update(b).then_some(b),
            None => None,
        }
    };
    if let Some(b) = fire {
        let _ = notify(
            &format!("Mouse battery {b}%"),
            "Charge the mouse soon (USB cable).",
            2,
        );
    }
}

/// Follows BlueZ (status only) and refreshes the tray on every change.
fn bt_loop(state: State, handle: ksni::blocking::Handle<KcTray>) {
    let mut watcher = Watcher::from_env();
    loop {
        let st = watcher.wait_change();
        if let BtState::Unavailable(why) = &st {
            eprintln!("tailglow-tray: Bluetooth status unavailable: {why}");
        }
        state.lock().unwrap().bt = st;
        check_low_battery(&state);
        let _ = handle.update(|_| {});
    }
}

fn quit_item() -> MenuItem<KcTray> {
    StandardItem {
        label: "Quit".into(),
        icon_name: "application-exit".into(),
        activate: Box::new(|_: &mut KcTray| std::process::exit(0)),
        ..Default::default()
    }
    .into()
}

fn load_settings(path: &Path) -> Option<Settings> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| Settings::from_toml(&t).ok())
}

// ----- battery poller -----------------------------------------------------------

fn battery_loop(state: State, handle: ksni::blocking::Handle<KcTray>, every: Duration) {
    loop {
        refresh(&state);
        check_low_battery(&state);
        // Push the new tooltip/menu to the host.
        let _ = handle.update(|_| {});
        std::thread::sleep(every);
    }
}

// ----- per-application profiles (X11) --------------------------------------------

/// WM_CLASS of the active window: `[instance, class]`.
fn active_window_classes() -> Result<Vec<String>> {
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{AtomEnum, ConnectionExt};
    let (conn, screen_num) = x11rb::connect(None)?;
    let root = conn.setup().roots[screen_num].root;
    let atom = conn
        .intern_atom(false, b"_NET_ACTIVE_WINDOW")?
        .reply()?
        .atom;
    let active = conn
        .get_property(false, root, atom, AtomEnum::WINDOW, 0, 1)?
        .reply()?;
    let Some(win) = active.value32().and_then(|mut v| v.next()) else {
        return Ok(vec![]);
    };
    if win == 0 {
        return Ok(vec![]);
    }
    let class = conn
        .get_property(false, win, AtomEnum::WM_CLASS, AtomEnum::STRING, 0, 1024)?
        .reply()?;
    Ok(class
        .value
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect())
}

/// Blocks on `_NET_ACTIVE_WINDOW` changes and applies the mapped profile.
fn apps_loop(
    state: State,
    handle: ksni::blocking::Handle<KcTray>,
    cfg_path: PathBuf,
    dry_run: bool,
) {
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{ChangeWindowAttributesAux, ConnectionExt, EventMask};
    use x11rb::protocol::Event;

    let (conn, screen_num) = match x11rb::connect(None) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("tailglow-tray: per-app profiles need X11 ({e}); on Wayland use the GUI/CLI to switch settings files manually");
            return;
        }
    };
    let root = conn.setup().roots[screen_num].root;
    let active_atom = match conn.intern_atom(false, b"_NET_ACTIVE_WINDOW") {
        Ok(c) => match c.reply() {
            Ok(r) => r.atom,
            Err(e) => {
                eprintln!("tailglow-tray: {e}");
                return;
            }
        },
        Err(e) => {
            eprintln!("tailglow-tray: {e}");
            return;
        }
    };
    if let Err(e) = conn.change_window_attributes(
        root,
        &ChangeWindowAttributesAux::new().event_mask(EventMask::PROPERTY_CHANGE),
    ) {
        eprintln!("tailglow-tray: {e}");
        return;
    }
    let _ = conn.flush();
    eprintln!(
        "tailglow-tray: per-app profiles on, map {}",
        cfg_path.display()
    );
    let mut cfg_mtime = None;
    let mut cfg = AppsConfig::default();
    let mut last_classes: Vec<String> = Vec::new();
    loop {
        // Reload the map when it changes.
        let mtime = std::fs::metadata(&cfg_path).and_then(|m| m.modified()).ok();
        if mtime != cfg_mtime {
            cfg_mtime = mtime;
            cfg = match AppsConfig::load(&cfg_path) {
                Ok(c) => c,
                Err(e) => {
                    if cfg_path.exists() {
                        eprintln!("tailglow-tray: {e:#}");
                    }
                    AppsConfig::default()
                }
            };
        }
        match conn.wait_for_event() {
            Ok(Event::PropertyNotify(ev)) if ev.atom == active_atom => {}
            Ok(_) => continue,
            Err(e) => {
                eprintln!("tailglow-tray: X11 connection lost: {e}");
                return;
            }
        }
        let classes = active_window_classes().unwrap_or_default();
        if classes.is_empty() || classes == last_classes {
            continue;
        }
        last_classes = classes.clone();
        let Some(profile) = cfg.profile_for(&classes) else {
            if dry_run {
                eprintln!("tailglow-tray: active {classes:?} → no profile");
            }
            continue;
        };
        if dry_run {
            eprintln!("tailglow-tray: active {classes:?} → {}", profile.display());
            continue;
        }
        {
            let s = state.lock().unwrap();
            if s.current_profile.as_ref() == Some(&profile) {
                continue;
            }
            if let Some(t) = s.last_apply {
                if t.elapsed() < Duration::from_secs(2) {
                    std::thread::sleep(Duration::from_secs(2) - t.elapsed());
                }
            }
        }
        let Some(settings) = load_settings(&profile) else {
            eprintln!("tailglow-tray: cannot read {}", profile.display());
            continue;
        };
        match apply_settings(&state, &settings) {
            Ok(out) => {
                eprintln!(
                    "tailglow-tray: {classes:?} → {} ({} change(s), {} macro(s) already on the mouse)",
                    profile.display(),
                    out.changes,
                    out.macros_skipped.len()
                );
                let mut s = state.lock().unwrap();
                s.current_profile = Some(profile);
                s.last_apply = Some(Instant::now());
            }
            Err(e) => eprintln!("tailglow-tray: apply failed: {e:#}"),
        }
        let _ = handle.update(|_| {});
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let state: State = Arc::new(Mutex::new(Shared {
        allow_untested: cli.allow_untested,
        snapshot: None,
        error: None,
        bt: BtState::Disabled,
        low: LowBattery::default(),
        last_apply: None,
        current_profile: None,
    }));
    refresh(&state);
    if let Some(e) = &state.lock().unwrap().error {
        eprintln!("tailglow-tray: {e} (will keep looking)");
    }
    let tray = KcTray {
        state: state.clone(),
    };
    // Keep running if the panel's StatusNotifier host is not up yet (login
    // ordering); the icon appears when the watcher does.
    let handle = tray
        .assume_sni_available(true)
        .spawn()
        .context("registering the tray icon on the session bus")?;
    if !cli.no_apps {
        let apps_path = cli
            .apps
            .clone()
            .unwrap_or_else(|| config_dir().join("apps.toml"));
        let st = state.clone();
        let h = handle.clone();
        let dry = cli.dry_run;
        std::thread::spawn(move || apps_loop(st, h, apps_path, dry));
    }
    {
        let st = state.clone();
        let h = handle.clone();
        std::thread::spawn(move || bt_loop(st, h));
    }
    battery_loop(state, handle, Duration::from_secs(cli.poll_secs.max(5)));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apps_config_matching() {
        let cfg: AppsConfig = toml::from_str(
            "default = \"/p/default.toml\"\n[apps]\nfirefox = \"/p/web.toml\"\n\"org.inkscape.Inkscape\" = \"/p/draw.toml\"\n",
        )
        .unwrap();
        assert_eq!(
            cfg.profile_for(&["Navigator".into(), "Firefox".into()]),
            Some(PathBuf::from("/p/web.toml"))
        );
        assert_eq!(
            cfg.profile_for(&["org.inkscape.Inkscape".into()]),
            Some(PathBuf::from("/p/draw.toml"))
        );
        assert_eq!(
            cfg.profile_for(&["xterm".into()]),
            Some(PathBuf::from("/p/default.toml"))
        );
        let none: AppsConfig = toml::from_str("").unwrap();
        assert_eq!(none.profile_for(&["xterm".into()]), None);
        std::env::set_var("HOME", "/home/x");
        assert_eq!(expand("~/a.toml"), PathBuf::from("/home/x/a.toml"));
    }

    #[test]
    fn menu_and_tooltip_build_from_a_fake_snapshot() {
        use ksni::Tray;
        let mut mouse = Mouse::new(tailglow_hid::FakeMouse::session2());
        let snap = mouse.snapshot().unwrap();
        let state: State = Arc::new(Mutex::new(Shared {
            allow_untested: false,
            snapshot: Some(snap),
            error: None,
            bt: BtState::Disabled,
            low: LowBattery::default(),
            last_apply: None,
            current_profile: None,
        }));
        let tray = KcTray { state };
        let tip = tray.tool_tip();
        assert_eq!(tip.title, "Keychron M1 — USB cable — battery 100%");
        assert!(tip.description.contains("1600 DPI"));
        let menu = tray.menu();
        assert!(menu.len() >= 8);
        // No mouse: the menu still offers Retry and Quit.
        let tray = KcTray {
            state: Arc::new(Mutex::new(Shared {
                allow_untested: false,
                snapshot: None,
                error: Some("no supported mouse found".into()),
                bt: BtState::Disabled,
                low: LowBattery::default(),
                last_apply: None,
                current_profile: None,
            })),
        };
        assert_eq!(tray.menu().len(), 4);
        assert_eq!(tray.tool_tip().title, "Tailglow: no mouse");
    }

    fn bt_mouse(connected: bool, battery: Option<u8>) -> BtMouse {
        BtMouse {
            path: "/org/bluez/hci0/dev_AA_BB_CC_00_11_22".into(),
            adapter: "/org/bluez/hci0".into(),
            model: "Keychron M1",
            name: "keychron M1".into(),
            address: "AA:BB:CC:00:11:22".into(),
            paired: true,
            connected,
            battery,
        }
    }

    #[test]
    fn bluetooth_only_shows_status_and_no_settings() {
        use ksni::Tray;
        let state: State = Arc::new(Mutex::new(Shared {
            allow_untested: false,
            snapshot: None,
            error: Some("no supported mouse found".into()),
            bt: BtState::Mice(vec![bt_mouse(true, Some(95))]),
            low: LowBattery::default(),
            last_apply: None,
            current_profile: None,
        }));
        let tray = KcTray {
            state: state.clone(),
        };
        let tip = tray.tool_tip();
        assert_eq!(tip.title, "Keychron M1 — Bluetooth — battery 95%");
        assert!(tip
            .description
            .contains("only be changed over the USB cable"));
        let labels: Vec<String> = tray
            .menu()
            .iter()
            .filter_map(|i| match i {
                MenuItem::Standard(s) => Some(s.label.clone()),
                MenuItem::SubMenu(s) => Some(s.label.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(labels[0], "Keychron M1 · Bluetooth · battery 95%");
        assert!(labels.iter().any(|l| l.contains("USB cable")));
        // No lighting/DPI/colour submenus over Bluetooth.
        assert!(!labels
            .iter()
            .any(|l| l.contains("Lighting") || l.contains("DPI")));
        assert_eq!(current_battery(&state.lock().unwrap()), Some(95));
        // A paired but disconnected mouse is not "connected".
        state.lock().unwrap().bt = BtState::Mice(vec![bt_mouse(false, None)]);
        assert_eq!(tray.tool_tip().title, "Tailglow: no mouse");
    }

    #[test]
    fn low_battery_once_per_discharge() {
        let mut low = LowBattery::default();
        let fired: Vec<u8> = [
            100, 50, 16, 15, 14, 15, 16, 15, 10, 3, 20, 15, 30, 80, 15, 12,
        ]
        .into_iter()
        .filter(|b| low.update(*b))
        .collect();
        // 15 → first alert; jitter around 15/16 and charging to 20 do not
        // re-arm; charged to 30 re-arms; the next discharge alerts at 15.
        assert_eq!(fired, vec![15, 15]);
        assert_eq!(LowBattery::THRESHOLD, 15);
    }
}
