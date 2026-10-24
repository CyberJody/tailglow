// SPDX-License-Identifier: GPL-3.0-or-later
//! `tailglow-gui`: desktop app for Keychron mice (iced).
//!
//! Every page shows what the mouse reports (full read-back after each write).
//! Environment / flags:
//! - `TAILGLOW_FAKE=1|session2|none|noperm|<pcap>`: use the fake transport.
//! - `TAILGLOW_STATE=FILE`: the state file shared with the CLI and `tailglow watch`
//!   (default `$XDG_CONFIG_HOME/tailglow/state.toml`); updated after each write.
//! - `--screenshot DIR` / `TAILGLOW_SCREENSHOT_DIR=DIR`: headless smoke test —
//!   walk through the pages, save PNGs, exit.
//! - `--theme system|light|dark`: theme for this run (not saved).
//! - `TAILGLOW_GUI_CONFIG=FILE`: the GUI's own settings (theme), default
//!   `$XDG_CONFIG_HOME/tailglow/gui.toml`.
//! - `TAILGLOW_THEME_LOG=1`: print every theme change to stderr (tests).

mod preview;
mod theme;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use iced::widget::{
    button, canvas, checkbox, column, container, pick_list, row, rule, scrollable, slider, space,
    text, text_input, tooltip, Column, Row,
};
use iced::{Color, Element, Fill, Font, Length, Subscription, Task, Theme};

use preview::Preview;
use tailglow_core::buttons::{
    ConsumerFunction, DpiFunction, Key, LightingFunction, MouseFunction, SystemShortcut,
};
use tailglow_core::pointer::{Debounce, Dpi, LiftOff, ReportRate, ResetKind, SensorSettings};
use tailglow_core::{
    ApplyOptions, Brightness, ButtonAction, Effect, Lighting, Mouse, Rgb, Settings, Snapshot,
    Speed, SupportStatus, Transport,
};
use tailglow_hid::bluez::{Bluez, BtMouse, BtState, UserConfirmed, Watcher};
use theme::{GuiConfig, SystemScheme, ThemeChoice, ThemeMode};

/// Lighting write policy (PROTOCOL.md §9: the vendor app never streams, and
/// each 0x22-0x28 write may be stored in the mouse's flash). The preview is
/// always live and local; the mouse gets a burst only
/// - when a slider is released (a keyboard or Ctrl+wheel change, which has no
///   release, after [`SLIDER_IDLE`]),
/// - on Enter in the hex field, or [`TYPING_IDLE`] after the last keystroke
///   (iced 0.14 has no focus-out event, so the idle timer stands in for it),
/// - right away for discrete clicks (effect tiles, swatches),
///
/// and never two bursts closer than [`MIN_BURST_GAP`] (≤ 2 bursts/s).
const MIN_BURST_GAP: Duration = Duration::from_millis(500);
const TYPING_IDLE: Duration = Duration::from_millis(400);
const SLIDER_IDLE: Duration = Duration::from_millis(1000);
const SCHEDULER_TICK: Duration = Duration::from_millis(100);
const ANIMATION_FRAME: Duration = Duration::from_millis(40);
/// Shown wherever settings would be, when the mouse is on Bluetooth only.
const BT_NOTE: &str = "Settings can only be changed over the USB cable (Keychron's firmware doesn't support it over Bluetooth). Settings you save stay in the mouse.";
const REPORT_HINT: &str = "If this works (or not) on your mouse, please report model, firmware and what happened in the project's issue tracker.";

type SharedMouse = Arc<Mutex<Option<Mouse<Box<dyn Transport>>>>>;

const USAGE: &str = "\
tailglow-gui: desktop app for Keychron mice

Usage: tailglow-gui [OPTIONS]

Options:
      --theme <MODE>      system, light or dark for this run (the picker's
                          choice is saved in gui.toml; this flag is not)
      --screenshot <DIR>  headless walk-through: save one PNG per page, exit
  -h, --help              print this help and exit
  -V, --version           print the version and exit

Environment:
  TAILGLOW_FAKE=session2|1|none|noperm|<pcap>  use the fake mouse
  TAILGLOW_STATE=FILE        mouse state file (shared with tailglow watch)
  TAILGLOW_GUI_CONFIG=FILE   GUI settings (default $XDG_CONFIG_HOME/tailglow/gui.toml)
  TAILGLOW_SCREENSHOT_DIR    same as --screenshot
";

/// Command-line options for a window run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Options {
    screenshot: Option<PathBuf>,
    theme: Option<ThemeMode>,
}

#[derive(Debug, PartialEq, Eq)]
enum Cli {
    Run(Options),
    Help,
    Version,
    Error(String),
}

/// Parses the arguments (without the program name). Help and version win
/// over everything else, so `--help` never opens a window.
fn parse_args(args: impl IntoIterator<Item = String>) -> Cli {
    let args: Vec<String> = args.into_iter().collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        return Cli::Help;
    }
    if args.iter().any(|a| a == "-V" || a == "--version") {
        return Cli::Version;
    }
    let mut opts = Options::default();
    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f.to_string(), Some(v.to_string())),
            _ => (arg.clone(), None),
        };
        match flag.as_str() {
            "--screenshot" | "--theme" => {
                let Some(value) = inline.or_else(|| it.next()) else {
                    return Cli::Error(format!("{flag} needs a value"));
                };
                if flag == "--screenshot" {
                    opts.screenshot = Some(PathBuf::from(value));
                } else {
                    match ThemeMode::parse(&value) {
                        Some(m) => opts.theme = Some(m),
                        None => {
                            return Cli::Error(format!(
                                "--theme: expected system, light or dark, got {value:?}"
                            ))
                        }
                    }
                }
            }
            _ => return Cli::Error(format!("unexpected argument {arg:?}")),
        }
    }
    Cli::Run(opts)
}

/// Application id: Wayland app_id / X11 WM_CLASS, desktop file and icon name.
const APP_ID: &str = "io.github.cyberjody.Tailglow";

fn main() -> iced::Result {
    let opts = match parse_args(std::env::args().skip(1)) {
        Cli::Run(o) => o,
        Cli::Help => {
            print!("{USAGE}");
            return Ok(());
        }
        Cli::Version => {
            println!("tailglow-gui {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Cli::Error(e) => {
            eprintln!("tailglow-gui: {e}\nTry 'tailglow-gui --help'.");
            std::process::exit(2);
        }
    };
    iced::application(move || App::boot(opts.clone()), App::update, App::view)
        .title("Tailglow")
        .subscription(App::subscription)
        .theme(App::theme)
        .window(iced::window::Settings {
            size: iced::Size::new(1040.0, 760.0),
            min_size: Some(iced::Size::new(900.0, 640.0)),
            icon: window_icon(),
            #[cfg(target_os = "linux")]
            platform_specific: iced::window::settings::PlatformSpecific {
                // Matches io.github.cyberjody.Tailglow.desktop, so docks show the app icon.
                application_id: APP_ID.into(),
                ..Default::default()
            },
            ..Default::default()
        })
        .run()
}

/// How a lighting control was changed (see the write policy above).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditKind {
    /// Slider movement: wait for the release.
    Drag,
    /// Typing in the hex field: wait for Enter or a pause.
    Typing,
    /// A click on a tile or swatch: send now.
    Click,
}

/// Decides when the pending lighting edit goes to the mouse. Pure (times
/// are passed in) so the policy is unit-tested.
#[derive(Debug, Default, Clone)]
struct WriteScheduler {
    dirty: bool,
    due: Option<Instant>,
    last_burst: Option<Instant>,
}

impl WriteScheduler {
    fn edit(&mut self, kind: EditKind, now: Instant) {
        self.dirty = true;
        self.due = Some(match kind {
            EditKind::Drag => now + SLIDER_IDLE,
            EditKind::Typing => now + TYPING_IDLE,
            EditKind::Click => now,
        });
    }

    /// Slider released / Enter pressed: send as soon as the gap allows.
    fn commit(&mut self, now: Instant) {
        if self.dirty {
            self.due = Some(now);
        }
    }

    fn ready(&self, now: Instant) -> bool {
        self.dirty
            && self.due.is_some_and(|d| now >= d)
            && self.last_burst.is_none_or(|t| now >= t + MIN_BURST_GAP)
    }

    fn sent(&mut self, now: Instant) {
        self.dirty = false;
        self.due = None;
        self.last_burst = Some(now);
    }

    fn clear(&mut self) {
        self.dirty = false;
        self.due = None;
    }
}

/// A status message and the page it belongs to (shown only there).
#[derive(Debug, Clone)]
struct Status {
    text: String,
    error: bool,
    page: Page,
}

/// A failed write, with whether the mouse went away.
#[derive(Debug, Clone)]
struct OpError {
    message: String,
    disconnected: bool,
}

impl From<tailglow_core::Error> for OpError {
    fn from(e: tailglow_core::Error) -> Self {
        let disconnected = matches!(&e, tailglow_core::Error::Transport(t) if t.is_disconnected());
        OpError {
            message: e.to_string(),
            disconnected,
        }
    }
}

impl From<&str> for OpError {
    fn from(s: &str) -> Self {
        OpError {
            message: s.into(),
            disconnected: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    Lighting,
    Dpi,
    Buttons,
    Settings,
    About,
}

impl Page {
    const ALL: [Page; 5] = [
        Page::Lighting,
        Page::Dpi,
        Page::Buttons,
        Page::Settings,
        Page::About,
    ];

    fn label(self) -> &'static str {
        match self {
            Page::Lighting => "Lighting",
            Page::Dpi => "DPI",
            Page::Buttons => "Buttons",
            Page::Settings => "Settings",
            Page::About => "About / diagnostics",
        }
    }

    fn shot_name(self) -> &'static str {
        match self {
            Page::Lighting => "00-lighting",
            Page::Dpi => "01-dpi",
            Page::Buttons => "02-buttons",
            Page::Settings => "03-settings",
            Page::About => "04-about",
        }
    }
}

#[derive(Debug, Clone)]
enum Connection {
    Connecting,
    Connected(Box<Snapshot>, String),
    Failed { message: String, permission: bool },
}

/// One write to the mouse. Each is validated and sent by `tailglow-core`, then
/// the whole state is re-read so the pages show what the mouse holds.
#[derive(Debug, Clone)]
enum Op {
    Lighting(Lighting),
    Dpi(Dpi),
    Rate(ReportRate),
    Sensor(SensorSettings),
    Debounce(Debounce),
    Button(Key, ButtonAction),
    Reset(ResetKind),
    Import(Settings),
}

impl Op {
    fn describe(&self) -> String {
        match self {
            Op::Lighting(l) => format!("Applied {}", describe(l)),
            Op::Dpi(d) => format!(
                "DPI stages {} (active {})",
                d.stages
                    .iter()
                    .map(|s| s.to_string())
                    .collect::<Vec<_>>()
                    .join(" / "),
                d.active + 1
            ),
            Op::Rate(r) => format!("Report rate {r}"),
            Op::Sensor(_) => "Sensor settings written".into(),
            Op::Debounce(d) => format!("Debounce {} ms", d.ms()),
            Op::Button(k, a) => format!("{} = {}", k.label(), a.label()),
            Op::Reset(k) => format!("Reset {} done", k.name()),
            Op::Import(_) => "Settings file applied; the mouse matches it".into(),
        }
    }
}

#[derive(Debug, Clone)]
enum Message {
    Connected(Result<(Box<Snapshot>, String), String>),
    Reconnect,
    GoTo(Page),
    // lighting
    PickEffect(Effect),
    SetRed(u8),
    SetGreen(u8),
    SetBlue(u8),
    Preset(Rgb),
    HexChanged(String),
    HexSubmit,
    SetBrightnessPercent(u8),
    SetSpeedPercent(u8),
    Flush,
    Tick(Instant),
    // generic writes
    Do(Op),
    Done(Page, Result<(Op, Box<Snapshot>, OpReport), OpError>),
    // dpi page (edited locally, written on Apply like the vendor app's Confirm)
    DpiStage(usize, u16),
    DpiStageText(usize, String),
    DpiActive(usize),
    DpiApply,
    // buttons page
    ButtonPick(Key, ActionChoice),
    ButtonCustomChanged(String),
    ButtonCustomApply(Key),
    // settings page
    AllowUntested(bool),
    ConfirmReset(Option<ResetKind>),
    OpenFile,
    SaveFile,
    FilePicked(Option<PathBuf>, bool),
    FileDone(Result<String, String>),
    ThemeMode(ThemeChoice),
    SystemScheme(SystemScheme),
    // bluetooth (status only)
    Bluetooth(BtState),
    ConfirmForget(bool),
    ForgetStale,
    ForgetDone(Result<String, String>),
    // screenshots
    ShotStep,
    Shot(iced::window::Screenshot),
}

/// What one op did: reports sent, and read-back mismatches (imports).
#[derive(Debug, Clone, Default)]
struct OpReport {
    reports: usize,
    mismatches: Vec<String>,
    macros_skipped: usize,
}

/// A pick-list entry on the Buttons page.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ActionChoice {
    label: String,
    action: Option<ButtonAction>, // None = "Custom…"
}

impl std::fmt::Display for ActionChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.label)
    }
}

fn action_choices() -> Vec<ActionChoice> {
    let mut v = vec![ActionChoice {
        label: "Default (factory)".into(),
        action: Some(ButtonAction::Default),
    }];
    let push = |v: &mut Vec<ActionChoice>, a: ButtonAction| {
        v.push(ActionChoice {
            label: a.label(),
            action: Some(a),
        })
    };
    for m in MouseFunction::ALL {
        push(&mut v, ButtonAction::Mouse(m));
    }
    for c in ConsumerFunction::ALL {
        push(&mut v, ButtonAction::Consumer(c));
    }
    for d in DpiFunction::ALL {
        push(&mut v, ButtonAction::Dpi(d));
    }
    for l in LightingFunction::ALL {
        push(&mut v, ButtonAction::Lighting(l));
    }
    for s in SystemShortcut::ALL {
        push(&mut v, ButtonAction::System(s));
    }
    push(&mut v, ButtonAction::Disabled);
    v.push(ActionChoice {
        label: "Custom… (key:ctrl+shift+k, autofire:left-click,37ms,5)".into(),
        action: None,
    });
    v
}

struct App {
    page: Page,
    connection: Connection,
    mouse: SharedMouse,
    allow_untested: bool,
    // lighting
    lighting: Lighting,
    hex: String,
    hex_error: Option<String>,
    applied: Option<Lighting>,
    writes: WriteScheduler,
    in_flight: bool,
    // dpi editor
    dpi_edit: Dpi,
    dpi_text: [String; 5],
    // buttons
    choices: Vec<ActionChoice>,
    custom_key: Option<Key>,
    custom_text: String,
    // settings
    confirm_reset: Option<ResetKind>,
    file_path: Option<PathBuf>,
    // diagnostics
    status: Option<Status>,
    writes_sent: usize,
    last_write_at: Option<Instant>,
    started: Instant,
    now: Instant,
    theme_mode: ThemeMode,
    system_scheme: SystemScheme,
    config_path: PathBuf,
    state_path: PathBuf,
    screenshot: Option<ScreenshotRun>,
    /// Bluetooth side from BlueZ (status only); `None` until the first read.
    bt: Option<BtState>,
    /// The "Remove old pairings" confirm step is showing.
    confirm_forget: bool,
}

struct ScreenshotRun {
    dir: PathBuf,
    step: usize,
}

impl App {
    fn boot(opts: Options) -> (App, Task<Message>) {
        let screenshot_dir = opts
            .screenshot
            .or_else(|| std::env::var_os("TAILGLOW_SCREENSHOT_DIR").map(PathBuf::from));
        let config_path = theme::config_path(config_dir());
        let theme_mode = opts
            .theme
            .unwrap_or_else(|| GuiConfig::load(&config_path).theme);
        // Ask the desktop once before the first frame (≤ 300 ms), so a dark
        // desktop does not see a light flash; the subscription follows it.
        let system_scheme = theme::read_system_scheme();
        let now = Instant::now();
        let app = App {
            page: Page::Lighting,
            connection: Connection::Connecting,
            mouse: Arc::new(Mutex::new(None)),
            allow_untested: false,
            lighting: Lighting::default(),
            hex: Lighting::default().color.to_hex(),
            hex_error: None,
            applied: None,
            writes: WriteScheduler::default(),
            in_flight: false,
            dpi_edit: Dpi::FACTORY,
            dpi_text: Dpi::FACTORY.stages.map(|s| s.to_string()),
            choices: action_choices(),
            custom_key: None,
            custom_text: String::new(),
            confirm_reset: None,
            file_path: None,
            status: None,
            writes_sent: 0,
            last_write_at: None,
            started: now,
            now,
            theme_mode,
            system_scheme,
            config_path,
            state_path: state_path(),
            screenshot: screenshot_dir.map(|dir| ScreenshotRun { dir, step: 0 }),
            bt: None,
            confirm_forget: false,
        };
        app.log_theme("start");
        let mut tasks = vec![app.connect()];
        if app.screenshot.is_some() {
            tasks.push(after(Duration::from_millis(1200), Message::ShotStep));
        }
        (app, Task::batch(tasks))
    }

    fn connect(&self) -> Task<Message> {
        let shared = self.mouse.clone();
        let allow = self.allow_untested;
        Task::perform(
            blocking(move || {
                let transport = tailglow_hid::open(false, None).map_err(|e| e.to_string())?;
                let mut mouse = Mouse::new(transport);
                mouse.allow_untested(allow);
                let snap = mouse.snapshot().map_err(|e| e.to_string())?;
                let path = mouse.describe();
                *shared.lock().unwrap() = Some(mouse);
                Ok((Box::new(snap), path))
            }),
            Message::Connected,
        )
    }

    fn snapshot(&self) -> Option<&Snapshot> {
        match &self.connection {
            Connection::Connected(s, _) => Some(s),
            _ => None,
        }
    }

    fn connected(&self) -> bool {
        self.snapshot().is_some()
    }

    /// The Keychron mouse connected over Bluetooth, if any.
    fn bt_connected(&self) -> Option<&BtMouse> {
        self.bt.as_ref().and_then(|b| b.connected())
    }

    /// On Bluetooth only: status, no settings (no USB connection).
    fn bt_only(&self) -> bool {
        !self.connected() && self.bt_connected().is_some()
    }

    fn untested(&self) -> bool {
        self.snapshot()
            .and_then(|s| s.info.status())
            .map(|s| s == SupportStatus::Likely)
            .unwrap_or(false)
    }

    /// Always an explicit theme: "System" is resolved here from the portal
    /// (see theme.rs for why iced's own system theme is not used).
    fn theme(&self) -> Option<Theme> {
        Some(theme::resolve(self.theme_mode, self.system_scheme))
    }

    fn is_dark(&self) -> bool {
        theme::resolve(self.theme_mode, self.system_scheme)
            .extended_palette()
            .is_dark
    }

    /// `TAILGLOW_THEME_LOG=1`: one stderr line per theme change (the live
    /// theme test reads these).
    fn log_theme(&self, why: &str) {
        if std::env::var_os("TAILGLOW_THEME_LOG").is_some() {
            eprintln!(
                "theme: {why}: mode={:?} system={:?} -> {}",
                self.theme_mode,
                self.system_scheme,
                if self.is_dark() { "dark" } else { "light" }
            );
        }
    }

    fn subscription(&self) -> Subscription<Message> {
        // Always listen, so the picker can say what "System" resolves to
        // even while a manual theme is chosen.
        let mut subs = vec![
            theme::system_scheme_changes().map(Message::SystemScheme),
            Subscription::run(bluetooth_stream),
        ];
        let animated = self.page == Page::Lighting
            && !matches!(self.lighting.effect, Effect::Off | Effect::Static);
        if animated || self.screenshot.is_some() {
            subs.push(iced::time::every(ANIMATION_FRAME).map(Message::Tick));
        } else if self.writes.dirty && !self.in_flight {
            subs.push(iced::time::every(SCHEDULER_TICK).map(Message::Tick));
        }
        Subscription::batch(subs)
    }

    /// Shows a status line on the current page.
    fn set_status(&mut self, text: impl Into<String>, error: bool) {
        self.status = Some(Status {
            text: text.into(),
            error,
            page: self.page,
        });
    }

    /// Switches page; status lines belong to the page that produced them.
    fn go_to(&mut self, page: Page) {
        self.page = page;
        self.confirm_reset = None;
        self.status = None;
    }

    /// Takes a fresh snapshot into the page state.
    fn adopt(&mut self, snap: Box<Snapshot>, path: String) {
        let cur = snap.lighting.current();
        self.lighting = cur;
        self.applied = Some(cur);
        self.writes.clear();
        self.sync_hex();
        self.dpi_edit = snap.info.pointer.dpi;
        self.dpi_text = self.dpi_edit.stages.map(|s| s.to_string());
        self.connection = Connection::Connected(snap, path);
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Connected(Ok((snap, path))) => {
                self.adopt(snap, path);
                self.status = None;
                Task::none()
            }
            Message::Connected(Err(message)) => {
                let permission = message.contains("permission") || message.contains("udev");
                self.connection = Connection::Failed {
                    message,
                    permission,
                };
                Task::none()
            }
            Message::Reconnect => {
                self.connection = Connection::Connecting;
                *self.mouse.lock().unwrap() = None;
                self.connect()
            }
            Message::GoTo(page) => {
                self.go_to(page);
                Task::none()
            }
            // ----- bluetooth (status only; the only call that changes
            // anything is RemoveDevice, after the confirm step) ---------
            Message::Bluetooth(state) => {
                if std::env::var_os("TAILGLOW_BT_LOG").is_some() {
                    eprintln!("bluetooth: {state:?}");
                }
                if state.stale().is_empty() {
                    self.confirm_forget = false;
                }
                self.bt = Some(state);
                Task::none()
            }
            Message::ConfirmForget(show) => {
                self.confirm_forget = show;
                Task::none()
            }
            Message::ForgetStale => {
                self.confirm_forget = false;
                let stale: Vec<BtMouse> = self
                    .bt
                    .as_ref()
                    .map(|b| b.stale().into_iter().cloned().collect())
                    .unwrap_or_default();
                Task::perform(
                    blocking(move || {
                        let bz = Bluez::from_env()
                            .map_err(|e| e.to_string())?
                            .ok_or("Bluetooth status is off")?;
                        let mut removed = 0;
                        for m in &stale {
                            bz.remove_stale(m, UserConfirmed::after_explicit_confirmation())
                                .map_err(|e| e.to_string())?;
                            removed += 1;
                        }
                        Ok(format!(
                            "Removed {removed} old Bluetooth pairing(s) from this computer."
                        ))
                    }),
                    Message::ForgetDone,
                )
            }
            Message::ForgetDone(result) => {
                match result {
                    Ok(msg) => self.set_status(msg, false),
                    Err(e) => self.set_status(format!("Bluetooth: {e}"), true),
                }
                Task::none()
            }
            // ----- lighting ---------------------------------------------
            Message::PickEffect(effect) => {
                // Start from what the mouse stores for that effect (per-effect
                // colour/brightness/speed live in the mouse), like the vendor app.
                if let Some(snap) = self.snapshot() {
                    let stored = snap.lighting.for_effect(effect);
                    self.lighting = Lighting {
                        effect,
                        ..if effect == Effect::Off {
                            self.lighting
                        } else {
                            stored
                        }
                    };
                } else {
                    self.lighting.effect = effect;
                }
                self.sync_hex();
                self.changed(EditKind::Click)
            }
            Message::SetRed(v) => {
                self.lighting.color.r = v;
                self.sync_hex();
                self.changed(EditKind::Drag)
            }
            Message::SetGreen(v) => {
                self.lighting.color.g = v;
                self.sync_hex();
                self.changed(EditKind::Drag)
            }
            Message::SetBlue(v) => {
                self.lighting.color.b = v;
                self.sync_hex();
                self.changed(EditKind::Drag)
            }
            Message::Preset(color) => {
                self.lighting.color = color;
                if !self.lighting.effect.has_color() {
                    self.lighting.effect = Effect::Static;
                }
                self.sync_hex();
                self.changed(EditKind::Click)
            }
            Message::HexChanged(s) => {
                self.hex = s;
                match Rgb::from_hex(&self.hex) {
                    Ok(c) => {
                        self.hex_error = None;
                        if c != self.lighting.color {
                            self.lighting.color = c;
                            return self.changed(EditKind::Typing);
                        }
                        Task::none()
                    }
                    Err(e) => {
                        self.hex_error = Some(e);
                        Task::none()
                    }
                }
            }
            Message::HexSubmit => {
                if self.hex_error.is_none() {
                    self.sync_hex();
                }
                self.writes.commit(Instant::now());
                self.poll()
            }
            Message::SetBrightnessPercent(p) => {
                self.lighting.brightness = Brightness::from_percent(p);
                self.changed(EditKind::Drag)
            }
            Message::SetSpeedPercent(p) => {
                self.lighting.speed = Speed::from_percent(p);
                self.changed(EditKind::Drag)
            }
            Message::Flush => {
                self.writes.commit(Instant::now());
                self.poll()
            }
            Message::Tick(now) => {
                self.now = now;
                self.poll()
            }
            // ----- generic writes ---------------------------------------
            Message::Do(op) => self.perform(op),
            Message::Done(origin, result) => {
                self.in_flight = false;
                match result {
                    Ok((op, snap, report)) => {
                        self.writes_sent += report.reports;
                        self.last_write_at = Some(Instant::now());
                        let (text, error) = if !report.mismatches.is_empty() {
                            (
                                format!(
                                    "Applied, but the mouse reads back differently: {}",
                                    report.mismatches.join("; ")
                                ),
                                true,
                            )
                        } else if report.macros_skipped > 0 {
                            (
                                format!(
                                    "{} ({} macro(s) already on the mouse, not re-sent)",
                                    op.describe(),
                                    report.macros_skipped
                                ),
                                false,
                            )
                        } else {
                            (op.describe(), false)
                        };
                        self.status = Some(Status {
                            text,
                            error,
                            page: origin,
                        });
                        let path = match &self.connection {
                            Connection::Connected(_, p) => p.clone(),
                            _ => String::new(),
                        };
                        let keep_lighting = matches!(op, Op::Lighting(_));
                        let pending = self.lighting;
                        let pending_writes = self.writes.clone();
                        self.adopt(snap, path);
                        self.remember(&op);
                        if keep_lighting && pending_writes.dirty {
                            // Something changed while the write was in flight:
                            // keep the user's latest value; the scheduler
                            // sends it when its time comes.
                            self.lighting = pending;
                            self.sync_hex();
                            self.writes = pending_writes;
                            return self.poll();
                        }
                    }
                    Err(e) => {
                        self.writes.clear();
                        if e.disconnected {
                            *self.mouse.lock().unwrap() = None;
                            self.connection = Connection::Failed {
                                message: e.message,
                                permission: false,
                            };
                            return Task::none();
                        }
                        self.status = Some(Status {
                            text: format!("Write failed: {}", e.message),
                            error: true,
                            page: origin,
                        });
                        // Snap the lighting controls back to what the mouse has.
                        if let Some(l) = self.applied {
                            self.lighting = l;
                            self.sync_hex();
                        }
                    }
                }
                Task::none()
            }
            // ----- DPI page ---------------------------------------------
            Message::DpiStage(i, v) => {
                self.dpi_edit.stages[i] = Dpi::normalize_value(v);
                self.dpi_text[i] = self.dpi_edit.stages[i].to_string();
                Task::none()
            }
            Message::DpiStageText(i, s) => {
                if let Ok(v) = s.trim().parse::<u16>() {
                    self.dpi_edit.stages[i] = Dpi::normalize_value(v);
                }
                self.dpi_text[i] = s;
                Task::none()
            }
            Message::DpiActive(i) => {
                self.dpi_edit.active = i as u8;
                Task::none()
            }
            Message::DpiApply => {
                self.dpi_text = self.dpi_edit.stages.map(|s| s.to_string());
                self.perform(Op::Dpi(self.dpi_edit))
            }
            // ----- buttons page -----------------------------------------
            Message::ButtonPick(key, choice) => match choice.action {
                Some(a) => {
                    self.custom_key = None;
                    self.perform(Op::Button(key, a))
                }
                None => {
                    self.custom_key = Some(key);
                    self.custom_text = self
                        .snapshot()
                        .map(|s| s.buttons.action(key).to_string())
                        .unwrap_or_default();
                    Task::none()
                }
            },
            Message::ButtonCustomChanged(s) => {
                self.custom_text = s;
                Task::none()
            }
            Message::ButtonCustomApply(key) => match self.custom_text.parse::<ButtonAction>() {
                Ok(a) => {
                    self.custom_key = None;
                    self.perform(Op::Button(key, a))
                }
                Err(e) => {
                    self.set_status(e, true);
                    Task::none()
                }
            },
            // ----- settings page ----------------------------------------
            Message::AllowUntested(v) => {
                self.allow_untested = v;
                if let Some(m) = self.mouse.lock().unwrap().as_mut() {
                    m.allow_untested(v);
                }
                if v {
                    self.set_status(
                        format!("Untested writes are now allowed for this session. {REPORT_HINT}"),
                        true,
                    );
                }
                Task::none()
            }
            Message::ConfirmReset(kind) => {
                self.confirm_reset = kind;
                Task::none()
            }
            Message::OpenFile => Task::perform(pick_file(false), |p| Message::FilePicked(p, false)),
            Message::SaveFile => Task::perform(pick_file(true), |p| Message::FilePicked(p, true)),
            Message::FilePicked(None, _) => Task::none(),
            Message::FilePicked(Some(path), save) => {
                self.file_path = Some(path.clone());
                if save {
                    let Some(snap) = self.snapshot() else {
                        self.set_status("Not connected: nothing to save", true);
                        return Task::none();
                    };
                    // Keep macros tailglow assigned (the mouse cannot report them).
                    let remembered = load_state(&self.state_path).unwrap_or_default();
                    let mut settings = Settings::from_snapshot_with_macros(snap, &remembered);
                    settings.sent_macros.clear();
                    Task::perform(
                        blocking(move || {
                            let text = settings.to_toml().map_err(|e| e.to_string())?;
                            std::fs::write(&path, text).map_err(|e| e.to_string())?;
                            Ok(format!("Saved {}", path.display()))
                        }),
                        Message::FileDone,
                    )
                } else {
                    match std::fs::read_to_string(&path)
                        .map_err(|e| e.to_string())
                        .and_then(|t| Settings::from_toml(&t).map_err(|e| e.to_string()))
                    {
                        Ok(s) if s.is_empty() => {
                            self.set_status("File has no settings sections", true);
                            Task::none()
                        }
                        Ok(s) => self.perform(Op::Import(s)),
                        Err(e) => {
                            self.set_status(format!("Open failed: {e}"), true);
                            Task::none()
                        }
                    }
                }
            }
            Message::FileDone(Ok(msg)) => {
                self.set_status(msg, false);
                Task::none()
            }
            Message::FileDone(Err(e)) => {
                self.set_status(format!("Save failed: {e}"), true);
                Task::none()
            }
            Message::ThemeMode(choice) => {
                self.theme_mode = choice.mode;
                self.log_theme("picked");
                let config = GuiConfig { theme: choice.mode };
                if let Err(e) = config.save(&self.config_path) {
                    self.set_status(
                        format!(
                            "Theme applied, but could not be saved to {}: {e}",
                            self.config_path.display()
                        ),
                        true,
                    );
                }
                Task::none()
            }
            Message::SystemScheme(scheme) => {
                if scheme != self.system_scheme {
                    self.system_scheme = scheme;
                    self.log_theme("desktop changed");
                }
                Task::none()
            }
            Message::ShotStep => self.screenshot_step(),
            Message::Shot(shot) => {
                let connected = self.connected();
                let bt_only = self.bt_only();
                let pages = self.shot_pages();
                let Some(run) = &mut self.screenshot else {
                    return Task::none();
                };
                let page = pages.get(run.step).copied().unwrap_or(Page::Lighting);
                let name = if connected {
                    format!("{}.png", page.shot_name())
                } else if bt_only {
                    format!("bluetooth-{}.png", page.shot_name())
                } else {
                    "05-no-mouse.png".to_string()
                };
                let path = run.dir.join(&name);
                if let Err(e) = save_png(&path, &shot) {
                    eprintln!("screenshot {}: {e}", path.display());
                } else {
                    eprintln!("saved {}", path.display());
                }
                run.step += 1;
                after(Duration::from_millis(400), Message::ShotStep)
            }
        }
    }

    /// Called after any lighting edit: the preview updates at once; the
    /// scheduler decides when the mouse gets it.
    fn changed(&mut self, kind: EditKind) -> Task<Message> {
        self.writes.edit(kind, Instant::now());
        self.poll()
    }

    /// Sends the pending lighting if the scheduler says it is time and it
    /// differs from what the mouse shows.
    fn poll(&mut self) -> Task<Message> {
        if self.in_flight || !self.writes.dirty {
            return Task::none();
        }
        if !self.connected() {
            self.writes.clear();
            self.set_status("Not connected: changes are previewed only", true);
            return Task::none();
        }
        let now = Instant::now();
        if !self.writes.ready(now) {
            return Task::none();
        }
        let lighting = self.lighting;
        if self.applied.is_some_and(|a| a.same_visible(&lighting)) {
            self.writes.clear();
            return Task::none();
        }
        self.writes.sent(now);
        self.perform(Op::Lighting(lighting))
    }

    /// Runs one write on the worker and re-reads the mouse.
    fn perform(&mut self, op: Op) -> Task<Message> {
        if !self.connected() {
            self.set_status("Not connected", true);
            return Task::none();
        }
        if matches!(op, Op::Lighting(_)) {
            self.in_flight = true;
        }
        self.confirm_reset = None;
        let shared = self.mouse.clone();
        let page = self.page;
        let state_path = self.state_path.clone();
        Task::perform(
            blocking(move || {
                let mut guard = shared.lock().unwrap();
                let mouse = guard.as_mut().ok_or(OpError::from("not connected"))?;
                let sent_macros = load_state(&state_path).unwrap_or_default().sent_macros;
                let report = apply_op(mouse, &op, sent_macros)?;
                let snap = mouse.snapshot()?;
                Ok((op, Box::new(snap), report))
            }),
            move |r| Message::Done(page, r),
        )
    }

    /// Writes the state file (`tailglow watch` re-applies it on plug): the
    /// mouse's settings plus the macros tailglow assigned, updated for `op`.
    fn remember(&self, op: &Op) {
        let Some(snap) = self.snapshot() else {
            return;
        };
        let mut remembered = load_state(&self.state_path).unwrap_or_default();
        match op {
            Op::Button(k, _) => remembered.forget_macro(*k),
            Op::Reset(ResetKind::Buttons | ResetKind::Factory) => remembered.forget_macros(),
            Op::Import(s) => remembered.record_applied(s),
            _ => {}
        }
        let s = Settings::from_snapshot_with_macros(snap, &remembered);
        if let (Some(dir), Ok(text)) = (self.state_path.parent(), s.to_toml()) {
            let _ = std::fs::create_dir_all(dir);
            let _ = std::fs::write(&self.state_path, text);
        }
    }

    fn sync_hex(&mut self) {
        self.hex = self.lighting.color.to_hex();
        self.hex_error = None;
    }

    /// Pages a `--screenshot` run walks: all of them over USB, the read-only
    /// ones over Bluetooth, none (one "no mouse" shot) otherwise.
    fn shot_pages(&self) -> Vec<Page> {
        if self.connected() {
            Page::ALL.to_vec()
        } else if self.bt_only() {
            vec![Page::Lighting, Page::Settings, Page::About]
        } else {
            Vec::new()
        }
    }

    fn screenshot_step(&mut self) -> Task<Message> {
        let Some(run) = &self.screenshot else {
            return Task::none();
        };
        let step = run.step;
        let dir = run.dir.clone();
        let pages = self.shot_pages();
        let last = pages.len().max(1);
        if step >= last {
            return iced::exit();
        }
        if self.bt_only() {
            self.go_to(pages[step]);
        }
        if self.connected() {
            self.go_to(Page::ALL[step]);
            if step == 0 {
                // Exercise the write path and show an animated effect, using
                // the values the mouse already stores for it (so a screenshot
                // run on a real mouse only switches the effect).
                let stored = self
                    .snapshot()
                    .map(|s| s.lighting.for_effect(Effect::RainbowSwirl))
                    .unwrap_or_default();
                self.lighting = Lighting {
                    effect: Effect::RainbowSwirl,
                    ..stored
                };
                self.writes.edit(EditKind::Click, Instant::now());
            }
            if step == 3 {
                self.confirm_reset = Some(ResetKind::Lighting);
            }
        }
        let _ = std::fs::create_dir_all(&dir);
        after(Duration::from_millis(500), Message::Tick(Instant::now())).chain(
            iced::window::latest().then(|id| match id {
                Some(id) => iced::window::screenshot(id).map(Message::Shot),
                None => Task::none(),
            }),
        )
    }

    // ----- view -------------------------------------------------------

    fn view(&self) -> Element<'_, Message> {
        let body: Element<'_, Message> = match self.page {
            Page::Lighting => self.lighting_page(),
            Page::Dpi => self.dpi_page(),
            Page::Buttons => self.buttons_page(),
            Page::Settings => self.settings_page(),
            Page::About => self.about_page(),
        };
        let content = column![self.header(), rule::horizontal(1), body, self.status_line()]
            .spacing(12)
            .padding(16)
            .width(Fill)
            .height(Fill);
        row![self.sidebar(), rule::vertical(1), content]
            .width(Fill)
            .height(Fill)
            .into()
    }

    fn sidebar(&self) -> Element<'_, Message> {
        let mut nav = Column::new().spacing(6).width(190);
        nav = nav
            .push(text("Tailglow").size(26))
            .push(text("Keychron mouse control").size(12).style(muted));
        nav = nav.push(space().height(12));
        for page in Page::ALL {
            let selected = page == self.page;
            let b = button(text(page.label()).size(14))
                .width(Fill)
                .padding([8, 12])
                .on_press(Message::GoTo(page))
                .style(move |theme, status| {
                    if selected {
                        button::primary(theme, status)
                    } else {
                        button::text(theme, status)
                    }
                });
            nav = nav.push(b);
        }
        nav = nav.push(space().height(Fill));
        let choice = |mode| ThemeChoice {
            mode,
            system: self.system_scheme,
        };
        nav = nav.push(text("Theme").size(12).style(muted));
        nav = nav.push(
            pick_list(
                ThemeMode::ALL.map(choice).to_vec(),
                Some(choice(self.theme_mode)),
                Message::ThemeMode,
            )
            .width(Fill)
            .text_size(13),
        );
        if let Some(note) =
            theme::system_note(self.system_scheme).filter(|_| self.theme_mode == ThemeMode::System)
        {
            nav = nav.push(text(note).size(11).style(muted));
        }
        container(nav)
            .padding(16)
            .height(Fill)
            .style(container::rounded_box)
            .into()
    }

    fn header(&self) -> Element<'_, Message> {
        if let (false, Some(m)) = (self.connected(), self.bt_connected()) {
            return self.bt_header(m);
        }
        match &self.connection {
            Connection::Connecting => row![
                text("Looking for a Keychron mouse…").size(18),
                space().width(Fill),
            ]
            .align_y(iced::Center)
            .into(),
            Connection::Connected(snap, _) => {
                let info = &snap.info;
                let battery = info.device.battery_percent;
                let signals = theme::signals(&theme::resolve(self.theme_mode, self.system_scheme));
                let battery_color = if battery <= 15 {
                    signals.error
                } else {
                    signals.ok
                };
                let p = &info.pointer;
                let status = info.status().map(|s| s.label()).unwrap_or("unknown model");
                row![
                    column![
                        text(info.model_name()).size(22),
                        text(format!(
                            "firmware {}  ·  {status}  ·  USB cable",
                            info.firmware.version
                        ))
                        .size(12)
                        .style(muted),
                    ]
                    .spacing(2),
                    space().width(Fill),
                    column![
                        row![
                            text("Battery").size(12).style(muted),
                            text(format!("{battery}%")).size(16).color(battery_color)
                        ]
                        .spacing(8)
                        .align_y(iced::Center),
                        text(format!(
                            "{} DPI  ·  {}  ·  profile {}",
                            p.dpi.active_dpi(),
                            p.report_rate()
                                .map(|r| r.to_string())
                                .unwrap_or_else(|| "?".into()),
                            p.profile + 1
                        ))
                        .size(12)
                        .style(muted),
                    ]
                    .spacing(2)
                    .align_x(iced::alignment::Horizontal::Right),
                ]
                .align_y(iced::Center)
                .into()
            }
            Connection::Failed {
                message,
                permission,
            } => {
                let hint = if *permission {
                    "Linux: copy packaging/70-keychron-m1.rules to /etc/udev/rules.d/, run \
                     `sudo udevadm control --reload && sudo udevadm trigger`, then replug the mouse."
                } else {
                    "Connect the mouse with its USB cable and click Retry. (Over Bluetooth, Tailglow shows the connection and battery once BlueZ reports the mouse connected.)"
                };
                container(
                    row![
                        column![
                            text(if *permission {
                                "No permission to open the mouse"
                            } else {
                                "No Keychron mouse found"
                            })
                            .size(18)
                            .style(banner_title),
                            text(message.clone()).size(12),
                            text(hint).size(12).style(banner_hint),
                        ]
                        .spacing(4)
                        .width(Fill),
                        button("Retry")
                            .on_press(Message::Reconnect)
                            .padding([8, 16]),
                    ]
                    .spacing(16)
                    .align_y(iced::Center),
                )
                .padding(12)
                .width(Fill)
                .style(banner)
                .into()
            }
        }
    }

    /// Header when the mouse is connected over Bluetooth only.
    fn bt_header(&self, m: &BtMouse) -> Element<'_, Message> {
        let signals = theme::signals(&theme::resolve(self.theme_mode, self.system_scheme));
        let (battery, color) = match m.battery {
            Some(b) => (
                format!("{b}%"),
                if b <= 15 { signals.error } else { signals.ok },
            ),
            None => ("unknown".to_string(), signals.warn),
        };
        row![
            column![
                text(m.model).size(22),
                text("Connected over Bluetooth  ·  status only")
                    .size(12)
                    .style(muted),
            ]
            .spacing(2),
            space().width(Fill),
            column![
                row![
                    text("Battery").size(12).style(muted),
                    text(battery).size(16).color(color)
                ]
                .spacing(8)
                .align_y(iced::Center),
                text("Plug in the USB cable to change settings")
                    .size(12)
                    .style(muted),
            ]
            .spacing(2)
            .align_x(iced::alignment::Horizontal::Right),
            button(text("Retry USB").size(13))
                .padding([6, 12])
                .style(secondary_button)
                .on_press(Message::Reconnect),
        ]
        .spacing(16)
        .align_y(iced::Center)
        .into()
    }

    fn status_line(&self) -> Element<'_, Message> {
        let signals = theme::signals(&theme::resolve(self.theme_mode, self.system_scheme));
        let warn = signals.warn;
        match &self.status {
            Some(s) if s.page == self.page => text(s.text.clone())
                .size(12)
                .color(if s.error { warn } else { signals.ok })
                .into(),
            _ if self.untested() && !self.allow_untested => text(
                "This model is likely compatible but untested: writes are off until you allow them on the Settings page.",
            )
            .size(12)
            .color(warn)
            .into(),
            _ if self.bt_only() => text(
                "Bluetooth: connection and battery only. Settings need the USB cable.",
            )
            .size(12)
            .style(muted)
            .into(),
            _ if !self.connected() => text(
                "Not connected: the controls only change the preview until a mouse is found.",
            )
            .size(12)
            .style(muted)
            .into(),
            _ => text(match self.page {
                Page::Lighting => "The preview follows you live; the mouse is written when you let go of a slider, press Enter, or click, and keeps the setting.",
                Page::Dpi => "Stages are written together when you click Apply, like the vendor app's Confirm.",
                Page::Buttons | Page::Settings => {
                    "Changes are written to the mouse immediately and stay in the mouse."
                }
                Page::About => "Read-only diagnostics; nothing here writes to the mouse.",
            })
            .size(12)
            .style(muted)
            .into(),
        }
    }

    // ----- lighting page --------------------------------------------------

    fn lighting_page(&self) -> Element<'_, Message> {
        if self.bt_only() {
            return self.offline("Lighting");
        }
        let t = (self.now - self.started).as_secs_f32();
        let l = self.lighting;

        // Effect tiles: one row, each tile fills an equal share of the width.
        let mut cards = Row::new().spacing(8).width(Fill);
        for effect in Effect::ALL {
            let selected = effect == l.effect;
            let prev = Preview {
                lighting: Lighting { effect, ..l },
                t,
                silhouette: false,
            };
            let card = column![
                canvas(prev).width(Fill).height(40),
                text(effect.label()).size(12).center().width(Fill),
            ]
            .spacing(4)
            .width(Fill)
            .align_x(iced::Center);
            cards = cards.push(
                button(card)
                    .padding(6)
                    .width(Fill)
                    .on_press(Message::PickEffect(effect))
                    .style(move |theme, status| tile_style(theme, status, selected)),
            );
        }

        let big = Preview {
            lighting: l,
            t,
            silhouette: true,
        };
        // The preview takes whatever height the controls leave, so the page
        // has no dead band at the bottom.
        let preview_panel = container(canvas(big).width(Fill).height(Fill))
            .width(Fill)
            .height(Fill)
            .style(panel);

        // Color controls.
        let color_enabled = l.effect.has_color();
        let presets = [
            Rgb::RED,
            Rgb::new(0x00, 0xff, 0x06),
            Rgb::new(0x00, 0x0c, 0xff),
            Rgb::WHITE,
            Rgb::new(0xff, 0x48, 0x00),
            Rgb::new(0x00, 0xff, 0xfc),
            Rgb::new(0xf0, 0x00, 0xff),
            Rgb::new(0xff, 0xde, 0x00),
        ];
        let mut swatches = Row::new().spacing(6);
        for c in presets {
            let fill = Color::from_rgb8(c.r, c.g, c.b);
            swatches = swatches.push(
                button(space().width(22).height(22))
                    .padding(2)
                    .on_press(Message::Preset(c))
                    .style(move |theme: &Theme, status| {
                        let mut s = button::secondary(theme, status);
                        s.background = Some(fill.into());
                        // A hairline keeps white / yellow / cyan visible on
                        // the white light-theme panel.
                        s.border = s.border.rounded(6).width(1).color(Color {
                            a: 0.25,
                            ..theme.palette().text
                        });
                        if c == l.color {
                            s.border = s.border.width(2).color(theme.palette().text);
                        }
                        s
                    }),
            );
        }
        let color_section: Option<Element<'_, Message>> = color_enabled.then(|| {
            column![
                row![
                    text("Color").size(15),
                    space().width(Fill),
                    text_input("#rrggbb", &self.hex)
                        .style(input_style)
                        .on_input(Message::HexChanged)
                        .on_submit(Message::HexSubmit)
                        .font(Font::MONOSPACE)
                        .size(13)
                        .width(110)
                        .padding(6),
                    swatch(l.color),
                ]
                .spacing(8)
                .align_y(iced::Center),
                self.hex_error
                    .as_ref()
                    .map(|e| text(e.clone()).size(12).style(error_text))
                    .unwrap_or_else(|| text("").size(12)),
                swatches.wrap(),
                channel("R", l.color.r, Message::SetRed, 0),
                channel("G", l.color.g, Message::SetGreen, 1),
                channel("B", l.color.b, Message::SetBlue, 2),
            ]
            .spacing(8)
            .into()
        });

        let brightness = labeled_slider(
            "Brightness",
            format!("{}%", l.brightness.percent()),
            slider(
                0..=100u8,
                l.brightness.percent(),
                Message::SetBrightnessPercent,
            )
            .on_release(Message::Flush),
        );
        let period = l
            .speed
            .period_seconds(l.effect)
            .map(|p| format!("{p:.1} s per cycle"))
            .unwrap_or_else(|| "n/a for this effect".into());
        // Effects without a speed (Static) keep the slider (the value is
        // still stored) but draw it inactive, so it does not look live.
        let speed_live = l.speed.period_seconds(l.effect).is_some();
        let speed = labeled_slider(
            "Speed",
            format!("{}%  ·  {period}", l.speed.percent()),
            slider(0..=100u8, l.speed.percent(), Message::SetSpeedPercent)
                .on_release(Message::Flush)
                .style(move |t, s| {
                    if speed_live {
                        slider::default(t, s)
                    } else {
                        inactive_slider(t, s)
                    }
                }),
        );
        let per_effect_note =
            text("Each effect keeps its own color, brightness and speed in the mouse.")
                .size(12)
                .style(muted);

        let controls: Element<'_, Message> = match color_section {
            // Color effects: color card on the left, tuning stacked on the right.
            Some(color) => row![
                container(color)
                    .padding(14)
                    .width(Length::FillPortion(3))
                    .height(Fill)
                    .style(panel),
                container(column![brightness, speed, per_effect_note].spacing(10))
                    .padding(14)
                    .width(Length::FillPortion(2))
                    .height(Fill)
                    .style(panel),
            ]
            .spacing(12)
            .height(230)
            .into(),
            // No color for this effect: one full-width card, sliders side by
            // side, and a single line instead of an empty color card.
            None if l.effect.has_brightness_and_speed() => container(
                column![
                    row![brightness, speed].spacing(32),
                    text(format!(
                        "Each effect keeps its own brightness and speed in the mouse. {} uses its own colors; Static, Breathing and Snake take one.",
                        l.effect.label()
                    ))
                    .size(12)
                    .style(muted),
                ]
                .spacing(10),
            )
            .padding(14)
            .width(Fill)
            .style(panel)
            .into(),
            None => container(
                text("Lights are off. Pick an effect above to turn them on.")
                    .size(13)
                    .style(muted),
            )
            .padding(14)
            .width(Fill)
            .style(panel)
            .into(),
        };

        column![text("Effect").size(15), cards, preview_panel, controls]
            .spacing(12)
            .width(Fill)
            .height(Fill)
            .into()
    }

    // ----- DPI page --------------------------------------------------------

    fn dpi_page(&self) -> Element<'_, Message> {
        let Some(snap) = self.snapshot() else {
            return self.offline("DPI stages");
        };
        let current = snap.info.pointer.dpi;
        let d = self.dpi_edit;
        let mut rows = Column::new().spacing(10);
        for i in 0..Dpi::STAGE_COUNT {
            let active = d.active as usize == i;
            let indicator = indicator_color(i);
            rows = rows.push(
                row![
                    button(text(if active { "●" } else { "○" }).size(16).style(
                        move |t: &Theme| text::Style {
                            // The active stage in the accent color, like
                            // the other selections.
                            color: active.then(|| t.extended_palette().primary.base.color),
                        }
                    ))
                    .on_press(Message::DpiActive(i))
                    .style(button::text)
                    .padding(4),
                    container(space().width(14).height(14)).style(move |theme: &Theme| {
                        container::Style {
                            background: Some(indicator.into()),
                            // The outline keeps the white stage visible on
                            // the light theme's white panel.
                            border: iced::border::rounded(7).width(1).color(Color {
                                a: 0.35,
                                ..theme.palette().text
                            }),
                            ..container::Style::default()
                        }
                    }),
                    text(format!("Stage {}", i + 1)).size(14).width(70),
                    slider(
                        Dpi::MIN..=Dpi::MAX,
                        d.stages[i],
                        move |v| Message::DpiStage(i, v)
                    )
                    .step(Dpi::STEP),
                    text_input("", &self.dpi_text[i])
                        .style(input_style)
                        .on_input(move |s| Message::DpiStageText(i, s))
                        .on_submit(Message::DpiApply)
                        .font(Font::MONOSPACE)
                        .size(13)
                        .width(80)
                        .padding(6),
                    text("DPI").size(12).style(muted),
                ]
                .spacing(10)
                .align_y(iced::Center),
            );
        }
        let changed = d != current;
        let apply = button(text("Apply to mouse").size(14))
            .padding([8, 16])
            .style(primary_button)
            .on_press_maybe(changed.then_some(Message::DpiApply));
        let reset = button(text("Reset DPI (400 / 800 / 1600 / 3200 / 5000)").size(13))
            .padding([8, 12])
            .style(secondary_button)
            .on_press(Message::Do(Op::Dpi(Dpi::FACTORY)));
        let note = text(format!(
            "Mouse now: {} · active stage {} ({} DPI). Values are 100-26000 in steps of 50; like the vendor app, stages are written together when you click Apply. The indicator colors are the ones the vendor app shows (fixed).",
            current
                .stages
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
                .join(" / "),
            current.active + 1,
            current.active_dpi()
        ))
        .size(12)
        .style(muted);
        container(
            column![
                text("DPI stages").size(15),
                rows,
                row![apply, reset].spacing(10),
                note
            ]
            .spacing(14),
        )
        .padding(16)
        .width(Fill)
        .style(panel)
        .into()
    }

    // ----- buttons page ----------------------------------------------------

    fn buttons_page(&self) -> Element<'_, Message> {
        let Some(snap) = self.snapshot() else {
            return self.offline("Button mapping");
        };
        let map = &snap.buttons;
        let left_keys = map.left_click_keys();
        // 7 + 12 spacing: the page fits 1040×760 without a scroll bar.
        let mut rows = Column::new().spacing(7);
        for key in Key::ALL {
            let action = map.action(key);
            let selected = self
                .choices
                .iter()
                .find(|c| c.action.as_ref() == Some(&action))
                .cloned()
                .unwrap_or_else(|| ActionChoice {
                    label: action.label(),
                    action: Some(action.clone()),
                });
            let flagged = map.flags.is_remapped(key);
            // The only Left-Click cannot be remapped (the core refuses it):
            // show it locked instead of offering a list that would fail.
            let locked = left_keys.len() == 1 && left_keys[0] == key;
            let picker: Element<'_, Message> = if locked {
                tooltip(
                    container(
                        row![
                            text(action.label()).size(13).style(muted),
                            space().width(Fill),
                            text("locked").size(11),
                        ]
                        .align_y(iced::Center),
                    )
                    .padding([6, 10])
                    .width(PICKER_WIDTH)
                    .style(locked_field),
                    container(
                        text("This is the only Left-Click. Give another key Left-Click to unlock it.")
                            .size(12),
                    )
                    .padding(8)
                    .style(container::rounded_box),
                    tooltip::Position::Bottom,
                )
                .into()
            } else {
                pick_list(self.choices.clone(), Some(selected), move |c| {
                    Message::ButtonPick(key, c)
                })
                .text_size(13)
                .width(PICKER_WIDTH)
                .into()
            };
            let mut r = row![
                text(key.label()).size(14).width(200),
                picker,
                tag(if flagged { "remapped" } else { "factory" }, flagged),
            ]
            .spacing(10)
            .align_y(iced::Center);
            if locked {
                r = r.push(text("the only Left-Click").size(11).style(muted));
            }
            if self.custom_key == Some(key) {
                r = r.push(
                    text_input("key:ctrl+shift+k", &self.custom_text)
                        .style(input_style)
                        .on_input(Message::ButtonCustomChanged)
                        .on_submit(Message::ButtonCustomApply(key))
                        .font(Font::MONOSPACE)
                        .size(13)
                        .width(220)
                        .padding(6),
                );
                r = r.push(
                    button(text("Set").size(13))
                        .on_press(Message::ButtonCustomApply(key))
                        .padding([6, 12]),
                );
            }
            rows = rows.push(r);
        }
        let syntax = [
            ("key:ctrl+shift+k", "keyboard shortcut (also key:f7)"),
            ("consumer:volume-up", "media key"),
            ("dpi:loop", "DPI cycle (dpi:up, dpi:down)"),
            ("light:brightness-up", "lighting control"),
            (
                "autofire:left-click,37ms,5",
                "auto-fire: button, interval, count",
            ),
            ("system:copy", "system shortcut"),
            ("disabled", "no action"),
        ];
        let mut help = Column::new().spacing(4).push(
            text("Custom actions: pick \"Custom…\" in a list, then type one of")
                .size(12)
                .style(muted),
        );
        for (code_text, what) in syntax {
            help = help.push(
                row![
                    container(code(code_text)).width(230),
                    text(what).size(12).style(muted)
                ]
                .spacing(8)
                .align_y(iced::Center),
            );
        }
        help = help.push(
            row![
                text("Macros: use the CLI,").size(12).style(muted),
                code("tailglow macro key6 a+ 47ms a-"),
                text("(experimental)").size(12).style(muted),
            ]
            .spacing(6)
            .align_y(iced::Center),
        );
        let reset_row: Element<'_, Message> = if self.confirm_reset == Some(ResetKind::Buttons) {
            row![
                button(text("Confirm: reset all buttons to factory").size(13))
                    .style(button::danger)
                    .padding([8, 14])
                    .on_press(Message::Do(Op::Reset(ResetKind::Buttons))),
                button(text("Cancel").size(13))
                    .style(button::text)
                    .padding([8, 12])
                    .on_press(Message::ConfirmReset(None)),
            ]
            .spacing(10)
            .into()
        } else {
            row![
                // First step outlined; the filled red is for Confirm.
                button(text("Reset all buttons…").size(13))
                    .style(danger_outline)
                    .padding([8, 14])
                    .on_press(Message::ConfirmReset(Some(ResetKind::Buttons))),
                text("sends the vendor app's \"Reset all button functions\"")
                    .size(12)
                    .style(muted),
            ]
            .spacing(10)
            .align_y(iced::Center)
            .into()
        };
        styled_scroll(
            container(
                column![
                    text("Button mapping").size(15),
                    rows,
                    reset_row,
                    rule::horizontal(1),
                    help,
                ]
                .spacing(12),
            )
            .padding(16)
            .width(Fill)
            .style(panel),
        )
        .height(Fill)
        .into()
    }

    // ----- settings page ---------------------------------------------------

    fn settings_page(&self) -> Element<'_, Message> {
        let Some(snap) = self.snapshot() else {
            let mut col = column![self.offline("Settings")].spacing(12).width(Fill);
            if let Some(bt) = self.bluetooth_panel() {
                col = col.push(bt);
            }
            return styled_scroll(col).height(Fill).into();
        };
        let p = &snap.info.pointer;
        let s = p.sensor;
        let rate = p.report_rate();

        let mut rate_row = Row::new().spacing(8);
        for r in ReportRate::ALL {
            let sel = rate == Some(r);
            rate_row = rate_row.push(
                button(text(r.to_string()).size(13))
                    .padding([6, 14])
                    .on_press(Message::Do(Op::Rate(r)))
                    .style(move |t, st| tile_style(t, st, sel)),
            );
        }
        let mut lift_row = Row::new().spacing(8);
        for lo in [LiftOff::Mm1, LiftOff::Mm2] {
            let sel = s.lift_off == lo;
            lift_row = lift_row.push(
                button(text(format!("{} mm", lo.mm())).size(13))
                    .padding([6, 14])
                    .on_press(Message::Do(Op::Sensor(SensorSettings {
                        lift_off: lo,
                        ..s
                    })))
                    .style(move |t, st| tile_style(t, st, sel)),
            );
        }
        let toggle =
            |label: &'static str, on: bool, make: fn(SensorSettings, bool) -> SensorSettings| {
                checkbox(on)
                    .label(label)
                    .text_size(14)
                    .on_toggle(move |v| Message::Do(Op::Sensor(make(s, v))))
                    .style(checkbox_style)
            };
        let deb = p.debounce.ms();
        // 8-11 ms were captured; 1-30 only with the untested opt-in.
        let allowed = |ms: u8| {
            Debounce::new(ms)
                .ok()
                .filter(|d| d.is_observed() || self.allow_untested)
        };
        let debounce_note: Element<'_, Message> = if !p.debounce.is_observed() {
            text(format!(
                "outside the captured 8-11 ms: {}",
                p.debounce.untested_warning()
            ))
            .size(12)
            .style(warn_text)
            .into()
        } else if self.allow_untested {
            text("1-30 ms allowed (untested outside 8-11)")
                .size(12)
                .style(muted)
                .into()
        } else {
            text("vendor default 8 ms · only 8-11 ms were captured; others need \"Allow untested values\" below")
                .size(12)
                .style(muted)
                .into()
        };
        let debounce_row = row![
            text("Debounce").size(14).width(120),
            button(text("−").size(14))
                .padding([4, 12])
                .style(secondary_button)
                .on_press_maybe(
                    allowed(deb.saturating_sub(1)).map(|d| Message::Do(Op::Debounce(d)))
                ),
            text(format!("{deb} ms")).size(14).width(60).center(),
            button(text("+").size(14))
                .padding([4, 12])
                .style(secondary_button)
                .on_press_maybe(
                    allowed(deb.saturating_add(1)).map(|d| Message::Do(Op::Debounce(d)))
                ),
            debounce_note,
        ]
        .spacing(10)
        .align_y(iced::Center);

        let pointer_panel = container(
            column![
                text("Pointer").size(15),
                row![text("Report rate").size(14).width(120), rate_row]
                    .spacing(10)
                    .align_y(iced::Center),
                row![text("Lift-off").size(14).width(120), lift_row]
                    .spacing(10)
                    .align_y(iced::Center),
                row![
                    space().width(130),
                    column![
                        toggle("Ripple control", s.ripple, |s, v| SensorSettings {
                            ripple: v,
                            ..s
                        }),
                        toggle("Angle snapping", s.angle_snap, |s, v| SensorSettings {
                            angle_snap: v,
                            ..s
                        }),
                        toggle("Motion sync", s.motion_sync, |s, v| SensorSettings {
                            motion_sync: v,
                            ..s
                        }),
                        toggle("Reverse scroll direction", s.scroll_reverse, |s, v| {
                            SensorSettings {
                                scroll_reverse: v,
                                ..s
                            }
                        }),
                    ]
                    .spacing(6)
                ],
                debounce_row,
            ]
            .spacing(12),
        )
        .padding(16)
        .width(Fill)
        .style(panel);

        // Files + safety.
        let file_text = self
            .file_path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "No file chosen yet (TOML, same format as tailglow export)".into());
        let mut reset_row = Row::new().spacing(8);
        for kind in [ResetKind::Lighting, ResetKind::Buttons, ResetKind::Factory] {
            let label = match kind {
                ResetKind::Lighting => "Reset lighting",
                ResetKind::Buttons => "Reset buttons",
                ResetKind::Factory => "Factory reset",
            };
            if self.confirm_reset == Some(kind) {
                reset_row = reset_row.push(
                    button(text(format!("Confirm {}", label.to_lowercase())).size(13))
                        .padding([6, 12])
                        .style(button::danger)
                        .on_press(Message::Do(Op::Reset(kind))),
                );
                reset_row = reset_row.push(
                    button(text("Cancel").size(13))
                        .padding([6, 12])
                        .style(button::text)
                        .on_press(Message::ConfirmReset(None)),
                );
            } else {
                let factory = kind == ResetKind::Factory;
                reset_row = reset_row.push(
                    button(text(label).size(13))
                        .padding([6, 12])
                        .style(move |t, s| {
                            // The one that clears everything gets the
                            // danger outline before its Confirm step.
                            if factory {
                                danger_outline(t, s)
                            } else {
                                secondary_button(t, s)
                            }
                        })
                        .on_press(Message::ConfirmReset(Some(kind))),
                );
            }
        }
        let untested_note: Element<'_, Message> = if self.untested() {
            column![
                checkbox(self.allow_untested)
                    .label("Allow untested values, and writes to this untested model (debounce anywhere in 1-30 ms)")
                    .text_size(13)
                    .on_toggle(Message::AllowUntested)
                    .style(checkbox_style),
                text(REPORT_HINT).size(12).style(muted),
            ]
            .spacing(6)
            .into()
        } else {
            column![
                text("Verified model: every command here was captured from the vendor app driving this mouse.")
                    .size(12)
                    .style(muted),
                checkbox(self.allow_untested)
                    .label("Allow untested values (debounce anywhere in 1-30 ms instead of the captured 8-11 ms)")
                    .text_size(13)
                    .on_toggle(Message::AllowUntested)
                    .style(checkbox_style),
                text("The mouse's real debounce limits are unknown; values the vendor app never sent could make clicks unreliable.")
                    .size(12)
                    .style(muted),
            ]
            .spacing(6)
            .into()
        };
        let files_panel = container(
            column![
                text("Settings files").size(15),
                row![
                    button(text("Open…").size(13))
                        .padding([6, 14])
                        .on_press(Message::OpenFile),
                    button(text("Save as…").size(13))
                        .padding([6, 14])
                        .style(secondary_button)
                        .on_press(Message::SaveFile),
                    text(file_text).size(12).style(muted),
                ]
                .spacing(10)
                .align_y(iced::Center),
                text("Open applies only what differs from the mouse, then reads it back and reports any mismatch. Save writes everything the mouse reports, plus the macros Tailglow assigned.")
                    .size(12)
                    .style(muted),
                rule::horizontal(1),
                text("Resets").size(15),
                reset_row,
                text("These send the vendor app's own reset commands; a factory reset also clears DPI, buttons and lighting.")
                    .size(12)
                    .style(muted),
                rule::horizontal(1),
                untested_note,
            ]
            .spacing(12),
        )
        .padding(16)
        .width(Fill)
        .style(panel);

        let mut col = column![pointer_panel, files_panel].spacing(12).width(Fill);
        if let Some(bt) = self.bluetooth_panel() {
            col = col.push(bt);
        }
        styled_scroll(col).height(Fill).into()
    }

    /// Keychron mice paired over Bluetooth, with "Remove old pairings…"
    /// (two steps). `None` when BlueZ knows no Keychron mouse.
    fn bluetooth_panel(&self) -> Option<Element<'_, Message>> {
        let bt = self.bt.as_ref()?;
        if bt.mice().is_empty() {
            return None;
        }
        let mut col = column![text("Bluetooth").size(15)].spacing(10);
        for m in bt.mice() {
            let state = if m.connected {
                format!(
                    "connected  ·  battery {}",
                    m.battery
                        .map(|b| format!("{b}%"))
                        .unwrap_or_else(|| "unknown".into())
                )
            } else if m.paired {
                "paired, not connected".to_string()
            } else {
                "seen, not paired".to_string()
            };
            col = col.push(
                row![
                    text(m.name.clone()).size(14).width(160),
                    text(m.address.clone()).size(13).style(muted).width(170),
                    text(state).size(13),
                ]
                .spacing(10)
                .align_y(iced::Center),
            );
        }
        let stale = bt.stale();
        if !stale.is_empty() {
            let warn_asleep = bt.connected().is_none();
            if self.confirm_forget {
                col = col.push(
                    text(format!(
                        "Remove {} pairing(s) that are not connected from this computer? Nothing is sent to the mouse. To use a removed pairing again, pair the mouse again.",
                        stale.len()
                    ))
                    .size(13),
                );
                if warn_asleep {
                    col = col.push(
                        text("No Keychron mouse is connected right now, so one of these may be your current pairing with the mouse asleep.")
                            .size(12)
                            .style(warn_text),
                    );
                }
                col = col.push(
                    row![
                        button(text("Confirm remove").size(13))
                            .padding([6, 12])
                            .style(button::danger)
                            .on_press(Message::ForgetStale),
                        button(text("Cancel").size(13))
                            .padding([6, 12])
                            .style(button::text)
                            .on_press(Message::ConfirmForget(false)),
                    ]
                    .spacing(8),
                );
            } else {
                col = col.push(
                    row![
                        button(text("Remove old pairings…").size(13))
                            .padding([6, 12])
                            .style(danger_outline)
                            .on_press(Message::ConfirmForget(true)),
                        text("The mouse makes a new pairing each time it is re-paired; old ones pile up here.")
                            .size(12)
                            .style(muted),
                    ]
                    .spacing(10)
                    .align_y(iced::Center),
                );
            }
        }
        col = col.push(
            text("Over Bluetooth Tailglow only reads the connection and battery level; it never writes to the mouse there.")
                .size(12)
                .style(muted),
        );
        Some(container(col).padding(16).width(Fill).style(panel).into())
    }

    // ----- about / diagnostics -------------------------------------------------

    fn about_page(&self) -> Element<'_, Message> {
        let mut lines: Vec<(String, String)> = vec![
            (
                "Application".into(),
                format!(
                    "Tailglow {}: open-source settings for Keychron mice",
                    env!("CARGO_PKG_VERSION")
                ),
            ),
            ("Version".into(), env!("CARGO_PKG_VERSION").into()),
            ("License".into(), "GPL-3.0-or-later".into()),
            (
                "Website".into(),
                "https://github.com/CyberJody/tailglow".into(),
            ),
        ];
        match &self.connection {
            Connection::Connected(snap, path) => {
                let info = &snap.info;
                lines.push(("Model".into(), info.model_name().into()));
                lines.push(("USB ID".into(), info.id().to_string()));
                lines.push((
                    "Support status".into(),
                    info.status().map(|s| s.label()).unwrap_or("unknown").into(),
                ));
                if let Some(m) = info.model {
                    lines.push(("Evidence".into(), m.evidence.into()));
                    lines.push(("Vendor interface".into(), m.vendor_interface.to_string()));
                }
                lines.push(("Firmware".into(), info.firmware.version.clone()));
                lines.push((
                    "Battery".into(),
                    format!("{}%", info.device.battery_percent),
                ));
                lines.push(("Transport".into(), path.clone()));
                lines.push((
                    "On-board profile".into(),
                    format!(
                        "index {} (switching not supported: the vendor app's profile write carries an unreadable key-pairing byte)",
                        info.pointer.profile
                    ),
                ));
                lines.push((
                    "Sensor flags byte".into(),
                    format!("0x{:02x}", info.pointer.sensor_flags),
                ));
                lines.push((
                    "Slot bytes".into(),
                    format!(
                        "{:02x} {:02x} {:02x}",
                        info.pointer.slots[0], info.pointer.slots[1], info.pointer.slots[2]
                    ),
                ));
                lines.push((
                    "Remapped keys".into(),
                    if snap.buttons.flags.remapped_keys().is_empty() {
                        "none".into()
                    } else {
                        snap.buttons
                            .flags
                            .remapped_keys()
                            .iter()
                            .map(|k| k.name())
                            .collect::<Vec<_>>()
                            .join(", ")
                    },
                ));
            }
            Connection::Connecting => lines.push(("Device".into(), "connecting…".into())),
            Connection::Failed { .. } if self.bt_only() => lines.push((
                "Device".into(),
                "connected over Bluetooth only (no USB cable)".into(),
            )),
            Connection::Failed { message, .. } => lines.push(("Device".into(), message.clone())),
        }
        lines.push((
            "Connection".into(),
            match (self.connected(), self.bt_connected().is_some()) {
                (true, true) => "USB cable (also connected over Bluetooth)",
                (true, false) => "USB cable",
                (false, true) => "Bluetooth (status only; settings need the USB cable)",
                (false, false) => "none",
            }
            .into(),
        ));
        lines.push((
            "Bluetooth".into(),
            match &self.bt {
                None => "checking…".into(),
                Some(BtState::Disabled) => {
                    "not checked (fake mouse or TAILGLOW_BLUEZ_BUS=off)".into()
                }
                Some(BtState::Unavailable(why)) => format!("unavailable: {why}"),
                Some(st @ BtState::Mice(_)) => match st.connected() {
                    Some(m) => format!(
                        "{} connected, battery {}{}",
                        m.name,
                        m.battery
                            .map(|b| format!("{b}%"))
                            .unwrap_or_else(|| "unknown".into()),
                        match st.stale().len() {
                            0 => String::new(),
                            n => format!("; {n} other pairing(s) not connected"),
                        }
                    ),
                    None => format!("no Keychron mouse connected; {} paired", st.mice().len()),
                },
            },
        ));
        lines.push((
            "Reports written this session".into(),
            self.writes_sent.to_string(),
        ));
        lines.push((
            "Last write".into(),
            self.last_write_at
                .map(|t| format!("{:.1} s ago", t.elapsed().as_secs_f32()))
                .unwrap_or_else(|| "none".into()),
        ));
        lines.push((
            "Write policy".into(),
            "lighting on slider release, Enter, or 400 ms after typing; at most 2 bursts/s; no streaming"
                .into(),
        ));
        lines.push(("State file".into(), self.state_path.display().to_string()));
        lines.push((
            "Fake device".into(),
            std::env::var("TAILGLOW_FAKE").unwrap_or_else(|_| "no".into()),
        ));
        let mut col = Column::new().spacing(6);
        for (k, v) in lines {
            col = col.push(
                row![
                    text(k).size(13).style(muted).width(220),
                    text(v).size(13).width(Fill)
                ]
                .spacing(10),
            );
        }
        styled_scroll(
            container(
                column![
                    text("About / diagnostics").size(15),
                    col,
                    rule::horizontal(1),
                    text(REPORT_HINT).size(12).style(muted),
                ]
                .spacing(12),
            )
            .padding(16)
            .width(Fill)
            .style(panel),
        )
        .height(Fill)
        .into()
    }

    fn offline(&self, what: &'static str) -> Element<'_, Message> {
        let body: Element<'_, Message> = if self.bt_only() {
            column![
                text(BT_NOTE).size(14),
                text("The mouse is connected over Bluetooth, which carries only its connection and battery level. Plug in the USB cable and click Retry USB to read and change these settings.")
                    .size(12)
                    .style(muted),
            ]
            .spacing(8)
            .into()
        } else {
            text("Connect a mouse to read and change these settings.")
                .size(12)
                .style(muted)
                .into()
        };
        container(column![text(what).size(15), body].spacing(8))
            .padding(16)
            .width(Fill)
            .style(panel)
            .into()
    }
}

/// Performs one op on the worker thread. Imports go through the same
/// diff + write + read-back compare as `tailglow import`; `sent_macros` is
/// the state file's macro fingerprints.
fn apply_op(
    mouse: &mut Mouse<Box<dyn Transport>>,
    op: &Op,
    sent_macros: std::collections::BTreeMap<Key, String>,
) -> Result<OpReport, tailglow_core::Error> {
    let one = |r: Result<(), tailglow_core::Error>| {
        r.map(|_| OpReport {
            reports: 1,
            ..OpReport::default()
        })
    };
    match op {
        Op::Lighting(l) => {
            mouse.apply_lighting(l)?;
            Ok(OpReport {
                reports: l.commands().len(),
                ..OpReport::default()
            })
        }
        Op::Dpi(d) => one(mouse.apply_dpi(d)),
        Op::Rate(r) => one(mouse.apply_report_rate(*r)),
        Op::Sensor(s) => one(mouse.apply_sensor(s)),
        Op::Debounce(d) => one(mouse.apply_debounce(*d)),
        Op::Button(k, a) => {
            let map = mouse.buttons()?;
            one(mouse.apply_button(*k, a, Some(&map)))
        }
        Op::Reset(k) => one(mouse.reset(*k)),
        Op::Import(settings) => {
            let out = mouse.apply_settings(
                settings,
                &ApplyOptions {
                    force: false,
                    sent_macros,
                },
            )?;
            Ok(OpReport {
                reports: out.reports,
                mismatches: out.mismatches,
                macros_skipped: out.macros_skipped.len(),
            })
        }
    }
}

/// The window icon: the same design as packaging/icons/io.github.cyberjody.Tailglow.svg (a
/// mouse silhouette inside an RGB ring), drawn at 64×64 so the GUI needs no
/// SVG renderer.
fn window_icon() -> Option<iced::window::Icon> {
    const N: u32 = 64;
    let mut rgba = Vec::with_capacity((N * N * 4) as usize);
    for py in 0..N {
        for px in 0..N {
            // Work in the SVG's 128-unit space.
            let x = (px as f32 + 0.5) * 2.0;
            let y = (py as f32 + 0.5) * 2.0;
            let (dx, dy) = (x - 64.0, y - 64.0);
            let r = (dx * dx + dy * dy).sqrt();
            let body = ((x - 64.0) / 28.0).powi(4) + ((y - 65.0) / 43.0).powi(4);
            let px_rgba: [u8; 4] = if body <= 1.0 {
                let line = (body > 0.82)
                    || ((x - 64.0).abs() < 1.6 && y < 56.0)
                    || ((y - 56.0).abs() < 1.6);
                let wheel = (60.0..=68.0).contains(&x) && (32.0..=46.0).contains(&y);
                if wheel {
                    [0x4f, 0xc3, 0xf7, 255]
                } else if line {
                    [0xe8, 0xea, 0xed, 255]
                } else {
                    [0x23, 0x27, 0x2e, 255]
                }
            } else if (r - 54.0).abs() <= 5.0 {
                // Hue around the ring, red at the top, clockwise.
                let hue = (dx.atan2(-dy).to_degrees() + 360.0) % 360.0;
                let [r8, g8, b8] = hue_rgb(hue);
                [r8, g8, b8, 255]
            } else {
                [0, 0, 0, 0]
            };
            rgba.extend_from_slice(&px_rgba);
        }
    }
    iced::window::icon::from_rgba(rgba, N, N).ok()
}

fn hue_rgb(h: f32) -> [u8; 3] {
    let x = 1.0 - ((h / 60.0) % 2.0 - 1.0).abs();
    let (r, g, b) = match (h / 60.0) as u32 {
        0 => (1.0, x, 0.0),
        1 => (x, 1.0, 0.0),
        2 => (0.0, 1.0, x),
        3 => (0.0, x, 1.0),
        4 => (x, 0.0, 1.0),
        _ => (1.0, 0.0, x),
    };
    [(r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8]
}

/// Native file dialog (xdg-desktop-portal through `rfd`); `None` when cancelled
/// or when no portal is available.
async fn pick_file(save: bool) -> Option<PathBuf> {
    let dialog = rfd::AsyncFileDialog::new()
        .add_filter("Tailglow settings (TOML)", &["toml"])
        .set_title(if save {
            "Save mouse settings"
        } else {
            "Open mouse settings"
        });
    let handle = if save {
        dialog
            .set_file_name("tailglow-settings.toml")
            .save_file()
            .await
    } else {
        dialog.pick_file().await
    };
    handle.map(|h| h.path().to_path_buf())
}

/// Unselected tiles: a slightly raised surface with a subtle border (no
/// washed-out gray on the dark theme; white cards on the light gray page);
/// selected: the primary colour.
fn tile_style(theme: &Theme, status: button::Status, selected: bool) -> button::Style {
    if selected {
        let mut s = button::primary(theme, status);
        // Same geometry as the unselected tiles (no 1 px jump).
        s.border = iced::border::rounded(8)
            .width(1)
            .color(theme.extended_palette().primary.strong.color);
        return s;
    }
    let p = theme.extended_palette();
    let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
    let (bg, border) = if p.is_dark {
        let bg = if hovered {
            p.background.strong.color
        } else {
            p.background.weak.color
        };
        (bg, p.background.strong.color)
    } else if hovered {
        // A hint of the primary colour instead of a gray step.
        (
            iced::theme::palette::mix(Color::WHITE, p.primary.base.color, 0.08),
            p.primary.weak.color,
        )
    } else {
        (p.background.weak.color, p.background.strong.color)
    };
    button::Style {
        background: Some(bg.into()),
        text_color: p.background.base.text,
        border: iced::border::rounded(8).width(1).color(border),
        ..button::Style::default()
    }
}

/// A vertical scroll area with [`scroll_style`].
fn styled_scroll<'a>(
    content: impl Into<Element<'a, Message>>,
) -> iced::widget::Scrollable<'a, Message> {
    scrollable(content).style(scroll_style)
}

/// Secondary buttons: on light a hairline border (a light-gray fill alone
/// vanishes on the white panel); unchanged on dark.
fn secondary_button(theme: &Theme, status: button::Status) -> button::Style {
    let mut s = button::secondary(theme, status);
    let p = theme.extended_palette();
    if !p.is_dark {
        let c = if status == button::Status::Disabled {
            p.background.strong.color
        } else {
            p.background.stronger.color
        };
        s.border = s.border.width(1).color(c);
        if status == button::Status::Disabled {
            s.background = Some(p.background.weaker.color.into());
        }
    }
    s
}

/// Primary buttons; disabled on light is a neutral gray with dim text
/// (iced's half-transparent blue left white-on-pale-blue text).
fn primary_button(theme: &Theme, status: button::Status) -> button::Style {
    let mut s = button::primary(theme, status);
    let p = theme.extended_palette();
    if !p.is_dark && status == button::Status::Disabled {
        s.background = Some(p.background.neutral.color.into());
        s.text_color = Color {
            a: 0.45,
            ..p.background.base.text
        };
    }
    s
}

/// The first step of a destructive action: red text and outline, no fill.
fn danger_outline(theme: &Theme, status: button::Status) -> button::Style {
    let p = theme.extended_palette();
    let red = if p.is_dark {
        iced::theme::palette::lighten(p.danger.base.color, 0.28)
    } else {
        p.danger.base.color
    };
    let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
    button::Style {
        background: hovered.then(|| Color { a: 0.1, ..red }.into()),
        text_color: red,
        border: iced::border::rounded(4).width(1).color(red),
        ..button::Style::default()
    }
}

/// Checkboxes: on light the unchecked box gets a darker outline (iced's
/// gray-on-white box is under 2:1 and reads as empty space).
fn checkbox_style(theme: &Theme, status: checkbox::Status) -> checkbox::Style {
    let mut s = checkbox::primary(theme, status);
    let p = theme.extended_palette();
    let checked = match status {
        checkbox::Status::Active { is_checked }
        | checkbox::Status::Hovered { is_checked }
        | checkbox::Status::Disabled { is_checked } => is_checked,
    };
    if !p.is_dark && !checked {
        s.background = Color::WHITE.into();
        s.border = s.border.color(p.background.strongest.color);
    }
    s
}

/// Scroll bars: a lighter thumb on light (the default near-black thumb
/// was the heaviest thing on the page).
fn scroll_style(theme: &Theme, status: scrollable::Status) -> scrollable::Style {
    let mut s = scrollable::default(theme, status);
    let p = theme.extended_palette();
    if !p.is_dark {
        for rail in [&mut s.vertical_rail, &mut s.horizontal_rail] {
            rail.background = None;
            if rail.scroller.background == p.background.strongest.color.into() {
                rail.scroller.background = p.background.stronger.color.into();
            }
        }
    }
    s
}

/// The not-connected banner: iced's solid red on dark; on light a pale red
/// panel with a red border and dark text (solid red with dark gray hint
/// text was unreadable there).
fn banner(theme: &Theme) -> container::Style {
    let p = theme.extended_palette();
    if p.is_dark {
        return container::danger(theme);
    }
    container::Style {
        background: Some(iced::theme::palette::mix(Color::WHITE, p.danger.base.color, 0.1).into()),
        text_color: Some(p.background.base.text),
        border: iced::border::rounded(8).width(1).color(p.danger.base.color),
        ..container::Style::default()
    }
}

fn banner_title(theme: &Theme) -> text::Style {
    let p = theme.extended_palette();
    text::Style {
        color: (!p.is_dark).then(|| iced::theme::palette::darken(p.danger.base.color, 0.08)),
    }
}

/// Hint line in the banner: the banner's own text color, slightly dimmed.
fn banner_hint(theme: &Theme) -> text::Style {
    let p = theme.extended_palette();
    let base = if p.is_dark {
        p.danger.base.text
    } else {
        p.background.base.text
    };
    text::Style {
        color: Some(Color { a: 0.85, ..base }),
    }
}

/// Text fields: white on the light theme's white panels (iced fills them
/// with the page gray and draws the placeholder nearly invisible there);
/// unchanged on dark.
fn input_style(theme: &Theme, status: text_input::Status) -> text_input::Style {
    let mut s = text_input::default(theme, status);
    let p = theme.extended_palette();
    if !p.is_dark {
        s.background = Color::WHITE.into();
        s.placeholder = Color {
            a: 0.5,
            ..p.background.base.text
        };
    }
    s
}

/// A slider whose value does not apply to the current effect: gray rail and
/// handle, still draggable.
fn inactive_slider(theme: &Theme, status: slider::Status) -> slider::Style {
    let mut s = slider::default(theme, status);
    let p = theme.extended_palette();
    let c = p.background.stronger.color;
    s.rail.backgrounds = (c.into(), p.background.strong.color.into());
    s.handle.background = c.into();
    s
}

/// Width of the Buttons page pick-lists.
const PICKER_WIDTH: f32 = 260.0;

/// A small code-styled snippet (monospace on a subtle background).
fn code(s: &'static str) -> Element<'static, Message> {
    container(text(s).size(12).font(Font::MONOSPACE))
        .padding([1, 6])
        .style(|theme: &Theme| {
            let p = theme.extended_palette();
            // Dark: a raised chip; light: a gray chip on the white panel.
            let bg = if p.is_dark {
                p.background.strong
            } else {
                p.background.neutral
            };
            container::Style {
                background: Some(bg.color.into()),
                text_color: Some(bg.text),
                border: iced::border::rounded(4),
                ..container::Style::default()
            }
        })
        .into()
}

/// "factory" / "remapped" pill with enough contrast on both themes.
fn tag(label: &'static str, highlighted: bool) -> Element<'static, Message> {
    container(text(label).size(11))
        .padding([2, 8])
        .width(Length::Shrink)
        .style(move |theme: &Theme| {
            let p = theme.extended_palette();
            // Remapped: filled with the primary colour. Factory: outlined in
            // the full text colour (readable on both themes).
            let text_c = theme.palette().text;
            let (bg, fg, border) = if highlighted {
                (
                    Some(p.primary.base.color),
                    p.primary.base.text,
                    p.primary.strong.color,
                )
            } else {
                (None, text_c, Color { a: 0.55, ..text_c })
            };
            container::Style {
                background: bg.map(Into::into),
                text_color: Some(fg),
                border: iced::border::rounded(9).width(1).color(border),
                ..container::Style::default()
            }
        })
        .into()
}

/// A field that looks like a disabled pick-list (gray on the light theme,
/// where enabled lists are white).
fn locked_field(theme: &Theme) -> container::Style {
    let p = theme.extended_palette();
    let bg = if p.is_dark {
        p.background.weak.color
    } else {
        p.background.base.color
    };
    container::Style {
        background: Some(bg.into()),
        border: iced::border::rounded(4)
            .width(1)
            .color(p.background.strong.color),
        ..container::Style::default()
    }
}

/// Panels: the weak background with a subtle border, same on both themes.
fn panel(theme: &Theme) -> container::Style {
    let p = theme.extended_palette();
    container::Style {
        background: Some(p.background.weak.color.into()),
        border: iced::border::rounded(10)
            .width(1)
            .color(p.background.strong.color),
        ..container::Style::default()
    }
}

fn indicator_color(stage: usize) -> Color {
    match Dpi::INDICATOR_COLORS[stage.min(4)] {
        "white" => Color::from_rgb(0.95, 0.95, 0.95),
        "green" => Color::from_rgb(0.2, 0.8, 0.3),
        "blue" => Color::from_rgb(0.25, 0.45, 0.95),
        "yellow" => Color::from_rgb(0.95, 0.85, 0.2),
        _ => Color::from_rgb(0.9, 0.25, 0.2),
    }
}

fn swatch(c: Rgb) -> Element<'static, Message> {
    let fill = Color::from_rgb8(c.r, c.g, c.b);
    container(space().width(28).height(28))
        .style(move |theme: &Theme| container::Style {
            background: Some(fill.into()),
            border: iced::border::rounded(6).width(1).color(Color {
                a: 0.3,
                ..theme.palette().text
            }),
            ..container::Style::default()
        })
        .into()
}

/// R / G / B label colors: bright on dark, deeper on light (≥ 4.5:1 on
/// white).
fn channel_tint(theme: &Theme, channel: usize) -> Color {
    let dark = theme::is_dark(theme);
    match (channel, dark) {
        (0, true) => Color::from_rgb(0.9, 0.25, 0.25),
        (1, true) => Color::from_rgb(0.2, 0.75, 0.3),
        (_, true) => Color::from_rgb(0.25, 0.45, 0.95),
        (0, false) => Color::from_rgb8(0xc0, 0x1f, 0x1f),
        (1, false) => Color::from_rgb8(0x1a, 0x7a, 0x34),
        (_, false) => Color::from_rgb8(0x1f, 0x4f, 0xc8),
    }
}

fn channel(
    name: &'static str,
    value: u8,
    on_change: fn(u8) -> Message,
    index: usize,
) -> Element<'static, Message> {
    row![
        text(name)
            .size(13)
            .style(move |t: &Theme| text::Style {
                color: Some(channel_tint(t, index))
            })
            .width(16),
        slider(0..=255u8, value, on_change).on_release(Message::Flush),
        text(format!("{value:>3}"))
            .size(12)
            .font(Font::MONOSPACE)
            .width(30),
    ]
    .spacing(10)
    .align_y(iced::Center)
    .into()
}

fn labeled_slider<'a>(
    name: &'static str,
    value: String,
    s: slider::Slider<'a, u8, Message>,
) -> Element<'a, Message> {
    column![
        row![
            text(name).size(15),
            space().width(Fill),
            text(value).size(12).style(muted)
        ]
        .align_y(iced::Center),
        s,
    ]
    .spacing(4)
    .into()
}

fn error_text(theme: &Theme) -> text::Style {
    text::Style {
        color: Some(theme::signals(theme).error),
    }
}

fn warn_text(theme: &Theme) -> text::Style {
    text::Style {
        color: Some(theme::signals(theme).warn),
    }
}

/// Secondary text. Light needs more opacity than dark for the same
/// readability (0.75 of near-black on white ≈ 6:1, ≥ 5:1 on the page gray).
fn muted(theme: &Theme) -> text::Style {
    let a = if theme::is_dark(theme) { 0.6 } else { 0.75 };
    text::Style {
        color: Some(Color {
            a,
            ..theme.palette().text
        }),
    }
}

fn describe(l: &Lighting) -> String {
    let mut s = l.effect.label().to_string();
    if l.effect.has_color() {
        s.push_str(&format!(" {}", l.color.to_hex()));
    }
    if l.effect.has_brightness_and_speed() {
        s.push_str(&format!(" at {}%", l.brightness.percent()));
    }
    s
}

/// Runs a blocking closure on a plain thread and resolves when it finishes.
fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> T + Send + 'static,
) -> impl std::future::Future<Output = T> {
    let (tx, rx) = iced::futures::channel::oneshot::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    async move { rx.await.expect("worker thread panicked") }
}

/// BlueZ state changes as messages. The watcher blocks on D-Bus signals, so
/// each wait runs on a worker thread; it only reads (see tailglow-hid's
/// bluez module).
fn bluetooth_stream() -> impl iced::futures::Stream<Item = Message> {
    use iced::futures::SinkExt;
    iced::stream::channel(
        4,
        async |mut out: iced::futures::channel::mpsc::Sender<Message>| {
            let mut watcher = Watcher::from_env();
            loop {
                let (w, state) = blocking(move || {
                    let s = watcher.wait_change();
                    (watcher, s)
                })
                .await;
                watcher = w;
                if out.send(Message::Bluetooth(state)).await.is_err() {
                    break;
                }
            }
        },
    )
}

fn after(delay: Duration, message: Message) -> Task<Message> {
    Task::perform(blocking(move || std::thread::sleep(delay)), move |_| {
        message.clone()
    })
}

fn state_path() -> PathBuf {
    if let Some(p) = std::env::var_os("TAILGLOW_STATE") {
        return PathBuf::from(p);
    }
    config_dir().join("state.toml")
}

/// `$XDG_CONFIG_HOME/tailglow` (or `~/.config/tailglow`, `%APPDATA%\tailglow`).
fn config_dir() -> PathBuf {
    tailglow_hid::paths::config_dir().unwrap_or_else(|| PathBuf::from("."))
}

fn load_state(path: &std::path::Path) -> Option<Settings> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| Settings::from_toml(&t).ok())
}

fn save_png(path: &std::path::Path, shot: &iced::window::Screenshot) -> Result<(), String> {
    let img = image::RgbaImage::from_raw(shot.size.width, shot.size.height, shot.rgba.to_vec())
        .ok_or("screenshot buffer size mismatch")?;
    img.save(path).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_path_env() {
        std::env::set_var("TAILGLOW_STATE", "/tmp/kc-state.toml");
        assert_eq!(state_path(), PathBuf::from("/tmp/kc-state.toml"));
    }

    fn args(a: &[&str]) -> Cli {
        parse_args(a.iter().map(|s| s.to_string()))
    }

    #[test]
    fn help_and_version_win_over_everything() {
        assert_eq!(args(&["--help"]), Cli::Help);
        assert_eq!(args(&["-h"]), Cli::Help);
        assert_eq!(args(&["--screenshot", "x", "--help"]), Cli::Help);
        assert_eq!(args(&["--bogus", "--help"]), Cli::Help);
        assert_eq!(args(&["--version"]), Cli::Version);
        assert_eq!(args(&["-V"]), Cli::Version);
        assert!(USAGE.contains("--theme") && USAGE.contains("--screenshot"));
    }

    #[test]
    fn run_options() {
        assert_eq!(args(&[]), Cli::Run(Options::default()));
        assert_eq!(
            args(&["--screenshot", "out", "--theme", "dark"]),
            Cli::Run(Options {
                screenshot: Some(PathBuf::from("out")),
                theme: Some(ThemeMode::Dark),
            })
        );
        assert_eq!(
            args(&["--theme=light"]),
            Cli::Run(Options {
                screenshot: None,
                theme: Some(ThemeMode::Light),
            })
        );
        assert!(matches!(args(&["--theme", "blue"]), Cli::Error(_)));
        assert!(matches!(args(&["--theme"]), Cli::Error(_)));
        assert!(matches!(args(&["--screenshot"]), Cli::Error(_)));
        assert!(matches!(args(&["file.toml"]), Cli::Error(_)));
    }

    #[test]
    fn describe_lighting() {
        let l = Lighting::static_color(Rgb::RED);
        assert_eq!(describe(&l), "Static Light #ff0000 at 100%");
        let off = Lighting {
            effect: Effect::Off,
            ..l
        };
        assert_eq!(describe(&off), "Lights Off");
    }

    #[test]
    fn action_choices_cover_every_named_action() {
        let c = action_choices();
        assert!(c.len() >= 40, "{}", c.len());
        assert_eq!(c.first().unwrap().action, Some(ButtonAction::Default));
        assert!(c.last().unwrap().action.is_none());
        for ch in &c {
            if let Some(a) = &ch.action {
                assert_eq!(a.to_string().parse::<ButtonAction>().unwrap(), *a);
            }
        }
    }

    #[test]
    fn indicator_colors_follow_the_vendor_table() {
        assert_eq!(
            Dpi::INDICATOR_COLORS,
            ["white", "green", "blue", "yellow", "red"]
        );
        let _ = (0..5).map(indicator_color).count();
    }

    #[test]
    fn slider_drags_send_once_on_release() {
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let mut w = WriteScheduler::default();
        let mut sent = Vec::new();
        // A 2 s drag: 100 changes, no write while the button is held.
        for i in 0..100u64 {
            w.edit(EditKind::Drag, ms(i * 20));
            assert!(!w.ready(ms(i * 20)), "no write mid-drag at {i}");
        }
        // Release at 2.0 s: sent right away.
        w.commit(ms(2000));
        assert!(w.ready(ms(2000)));
        w.sent(ms(2000));
        sent.push(2000);
        assert!(!w.ready(ms(2100)));
        assert_eq!(sent.len(), 1);
    }

    #[test]
    fn typing_waits_for_a_pause_and_enter_sends_now() {
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let mut w = WriteScheduler::default();
        // Six keystrokes 150 ms apart: nothing until 400 ms after the last.
        for i in 0..6u64 {
            w.edit(EditKind::Typing, ms(i * 150));
            assert!(!w.ready(ms(i * 150 + 100)));
        }
        assert!(!w.ready(ms(750 + 399)));
        assert!(w.ready(ms(750 + 400)));
        w.sent(ms(1150));
        // Enter right after another keystroke: sent as soon as the gap allows.
        w.edit(EditKind::Typing, ms(1300));
        w.commit(ms(1310));
        assert!(!w.ready(ms(1310)), "500 ms gap after the previous burst");
        assert!(w.ready(ms(1650)));
    }

    #[test]
    fn never_more_than_two_bursts_per_second() {
        let t0 = Instant::now();
        let mut w = WriteScheduler::default();
        let mut bursts = Vec::new();
        // A user hammering tiles every 50 ms for 5 s, polled every 10 ms.
        for t in (0..5000u64).step_by(10) {
            let now = t0 + Duration::from_millis(t);
            if t % 50 == 0 {
                w.edit(EditKind::Click, now);
            }
            if w.ready(now) {
                w.sent(now);
                bursts.push(t);
            }
        }
        for pair in bursts.windows(2) {
            assert!(pair[1] - pair[0] >= 500, "{pair:?}");
        }
        assert!(bursts.len() <= 10, "{}", bursts.len());
        assert!(
            bursts.len() >= 9,
            "clicks still get through: {}",
            bursts.len()
        );
    }

    #[test]
    fn window_icon_builds() {
        assert!(window_icon().is_some());
        assert_eq!(hue_rgb(0.0), [255, 0, 0]);
        assert_eq!(hue_rgb(120.0), [0, 255, 0]);
    }

    /// Every op on the fake: writes succeed and the snapshot re-read matches.
    #[test]
    fn ops_round_trip_on_the_fake() {
        let mut mouse =
            Mouse::new(Box::new(tailglow_hid::FakeMouse::session2()) as Box<dyn Transport>);
        mouse.snapshot().unwrap();
        let ops = [
            Op::Lighting(Lighting::static_color(Rgb::new(0x12, 0x34, 0x56))),
            Op::Dpi(Dpi {
                stages: [450, 800, 1600, 4900, 5000],
                active: 3,
            }),
            Op::Rate(ReportRate::Hz500),
            Op::Sensor(SensorSettings {
                scroll_reverse: true,
                ..SensorSettings::FACTORY
            }),
            Op::Debounce(Debounce(11)),
            Op::Button(
                Key::Key4,
                ButtonAction::Consumer(ConsumerFunction::VolumeUp),
            ),
        ];
        for op in &ops {
            apply_op(&mut mouse, op, Default::default()).unwrap();
        }
        let snap = mouse.snapshot().unwrap();
        assert_eq!(snap.lighting.current().color, Rgb::new(0x12, 0x34, 0x56));
        assert_eq!(snap.info.pointer.dpi.active, 3);
        assert_eq!(snap.info.pointer.report_rate(), Some(ReportRate::Hz500));
        assert!(snap.info.pointer.sensor.scroll_reverse);
        assert_eq!(snap.info.pointer.debounce.ms(), 11);
        assert_eq!(
            snap.buttons.action(Key::Key4),
            ButtonAction::Consumer(ConsumerFunction::VolumeUp)
        );
        // Import of the baseline file writes only the differences back. Keys
        // absent from the file are left alone (a file resets a key by listing
        // it as "default").
        let mut baseline = {
            let mut m =
                Mouse::new(Box::new(tailglow_hid::FakeMouse::session2()) as Box<dyn Transport>);
            Settings::from_snapshot(&m.snapshot().unwrap())
        };
        baseline.buttons.insert(Key::Key4, ButtonAction::Default);
        let report = apply_op(&mut mouse, &Op::Import(baseline), Default::default()).unwrap();
        assert_eq!(
            report.reports,
            4 + 1 + 1 + 1 + 1 + 1,
            "lighting burst + dpi + rate + sensor + debounce + key4"
        );
        assert!(report.mismatches.is_empty(), "{:?}", report.mismatches);
        let back = Settings::from_snapshot(&mouse.snapshot().unwrap());
        assert_eq!(back.dpi, Some(Dpi::FACTORY));
        assert!(back.buttons.is_empty());
        // Left-click guard surfaces as an error, and the mouse keeps Key 1.
        assert!(apply_op(
            &mut mouse,
            &Op::Button(Key::Key1, ButtonAction::Disabled),
            Default::default()
        )
        .is_err());
        // Debounce outside 8-11 needs the opt-in.
        assert!(apply_op(&mut mouse, &Op::Debounce(Debounce(12)), Default::default()).is_err());
        let back = mouse.snapshot().unwrap();
        assert!(back.buttons.action(Key::Key1).is_left_click());
        assert!(!back.buttons.flags.is_remapped(Key::Key1));
    }
}
