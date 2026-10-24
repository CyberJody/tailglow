// SPDX-License-Identifier: GPL-3.0-or-later
//! `tailglow`: command-line control for Keychron mice.
//!
//! Every setting is read back from the mouse (0x07, 0x12-0x17, 0x61/0x62);
//! the only host-side state is `state.toml`, which remembers the last
//! settings tailglow applied (macros cannot be read back) and feeds
//! `tailglow watch` (apply-on-plug).

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};

use tailglow_core::buttons::{Key, LoopMode, Macro, MacroEvent};
use tailglow_core::pointer::{Debounce, Dpi, LiftOff, ReportRate, ResetKind, SensorSettings};
use tailglow_core::{
    ApplyOptions, ApplyOutcome, Brightness, ButtonAction, ButtonMap, Effect, Lighting, Mouse, Rgb,
    Settings, Snapshot, Speed, SupportStatus, Transport,
};
use tailglow_hid::bluez::{Bluez, BtState, UserConfirmed};

const REPORT_URL: &str =
    "please report what happened (model, firmware, what worked) in the project's issue tracker";

#[derive(Parser)]
#[command(
    name = "tailglow",
    version,
    about = "Tailglow: open-source settings for Keychron mice",
    after_help = "Without hardware: `tailglow --fake info` replays the recorded Keychron M1.\n\
                  Writes go to the mouse immediately and persist in it (there is no save step)."
)]
struct Cli {
    /// Use the fake mouse (recorded replies) instead of real hardware.
    /// Same as TAILGLOW_FAKE=1.
    #[arg(long, global = true, hide = true)]
    fake: bool,

    /// Open this hidraw/hidapi path instead of auto-detecting. It must be the
    /// vendor interface of a known model (checked from sysfs/hidapi before
    /// anything is sent).
    #[arg(long, global = true, value_name = "PATH")]
    device: Option<String>,

    /// Allow writes that were never tested: models listed as likely-compatible
    /// (M2, M3, M3 mini, M6, M7, 2.4 GHz receivers) and debounce values
    /// outside the captured 8-11 ms (1-30 accepted). Reads never need this.
    #[arg(long, global = true)]
    allow_untested: bool,

    /// Where the last-applied settings are remembered
    /// (default: $XDG_CONFIG_HOME/tailglow/state.toml; env TAILGLOW_STATE).
    #[arg(long, global = true, value_name = "FILE", env = "TAILGLOW_STATE")]
    state: Option<PathBuf>,

    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// How the mouse is connected (USB cable, Bluetooth) and its battery
    Status,
    /// List Keychron mice found on this computer
    List,
    /// Show everything the mouse reports (identity, lighting, DPI, buttons)
    Info,
    /// Set a solid color (or another single-color effect)
    Color {
        /// RRGGBB or #RRGGBB
        color: Rgb,
        /// static, breathing or snake
        #[arg(long, default_value = "static")]
        effect: Effect,
        #[command(flatten)]
        tuning: Tuning,
    },
    /// Select a lighting effect (keeps the mouse's stored color/brightness/speed for it)
    Effect {
        /// off, static, breathing, rainbow, rainbow-swirl, snake, christmas
        effect: Effect,
        /// Color for static/breathing/snake (RRGGBB)
        #[arg(long)]
        color: Option<Rgb>,
        #[command(flatten)]
        tuning: Tuning,
    },
    /// DPI stages: show, or set all five and the active one
    Dpi {
        #[command(subcommand)]
        action: Option<DpiCmd>,
    },
    /// USB report rate: show, or set 125, 500 or 1000 Hz
    Rate {
        /// 125, 500 or 1000
        hz: Option<ReportRate>,
    },
    /// Sensor options: show, or change any of them
    Sensor {
        /// 1 or 2 (mm)
        #[arg(long, value_name = "MM")]
        lift_off: Option<u8>,
        #[arg(long, value_name = "on|off")]
        ripple: Option<OnOff>,
        #[arg(long, value_name = "on|off")]
        angle_snap: Option<OnOff>,
        #[arg(long, value_name = "on|off")]
        motion_sync: Option<OnOff>,
        /// forward or reverse
        #[arg(long, value_name = "forward|reverse")]
        scroll: Option<ScrollDir>,
    },
    /// Button debounce: show, or set in ms (vendor default 8; 8-11 captured,
    /// 1-30 with --allow-untested)
    Debounce { ms: Option<u8> },
    /// Button mapping: show, or set / reset a key
    Buttons {
        #[command(subcommand)]
        action: Option<ButtonsCmd>,
    },
    /// Assign a macro to a key (experimental: single-report macros, max 13 steps)
    Macro {
        /// key2 … key7, scroll-up, scroll-down (key1 is left click and stays)
        key: Key,
        /// Name stored in the mouse (ASCII)
        #[arg(long, default_value = "Custom 1")]
        name: String,
        /// times:N (default times:1), until-release, until-any-click
        #[arg(long = "loop", default_value = "times:1")]
        loop_mode: LoopMode,
        /// Fixed delay in ms between steps (0 = use the delays in STEPS)
        #[arg(long, default_value_t = 0)]
        fixed_delay: u16,
        /// Steps: KEY+ (press), KEY- (release), Nms (delay); e.g. a+ 47ms a- 3000ms b+ 32ms b-
        #[arg(required = true)]
        steps: Vec<MacroEvent>,
    },
    /// Show the on-board profile index (read-only; switching is not supported)
    Profile,
    /// Reset buttons, lighting or everything (factory) — asks for --yes
    Reset {
        /// buttons, lighting or factory
        what: ResetKind,
        /// Confirm
        #[arg(long)]
        yes: bool,
    },
    /// Write the mouse's current settings to a TOML file (stdout if no FILE)
    Export { file: Option<PathBuf> },
    /// Apply settings from a TOML file (only sections present in the file),
    /// then read the mouse back and fail on any mismatch
    Import {
        file: PathBuf,
        /// Write everything in the file even when the mouse already matches
        /// (macros included)
        #[arg(long)]
        force: bool,
    },
    /// Bluetooth pairings of Keychron mice: show them, or remove old ones
    /// (status only: the mouse takes no settings over Bluetooth)
    Bluetooth {
        #[command(subcommand)]
        action: Option<BluetoothCmd>,
    },
    /// Raw read-only dump of the launch reads as JSON (used by scripts/compare_baseline.py)
    #[command(hide = true)]
    Dump { file: Option<PathBuf> },
    /// Apply-on-plug: wait for the mouse and re-apply the last saved settings
    /// (or FILE) whenever it appears; runs until stopped (systemd user unit)
    Watch {
        /// Settings file to apply (default: the state file)
        file: Option<PathBuf>,
        /// Poll interval in seconds
        #[arg(long, default_value_t = 2)]
        interval: u64,
        /// Apply once if the mouse is already present, then exit
        #[arg(long)]
        once: bool,
    },
}

#[derive(Subcommand)]
enum BluetoothCmd {
    /// Remove paired Keychron mice that are not connected (the mouse makes a
    /// new pairing each time it is re-paired). Lists them unless --yes.
    ForgetStale {
        /// Confirm the removal
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand)]
enum DpiCmd {
    /// Set all five stages (100-26000, multiples of 50) and optionally the active one
    Set {
        #[arg(num_args = 5, value_name = "DPI")]
        stages: Vec<u16>,
        /// Active stage 1-5
        #[arg(long, value_name = "N")]
        active: Option<u8>,
    },
    /// Select the active stage (1-5)
    Active { stage: u8 },
    /// Back to the vendor defaults 400/800/1600/3200/5000, stage 3
    Reset,
}

#[derive(Subcommand)]
enum ButtonsCmd {
    /// Assign an action: left-click … scroll-right, key:ctrl+shift+k,
    /// consumer:volume-up, dpi:loop|up|down, light:effect|speed|color|brightness-up|brightness-down,
    /// autofire:left-click,37ms,5, system:copy, disabled, default
    Set { key: Key, action: ButtonAction },
    /// Back to the key's factory function
    Reset {
        key: Option<Key>,
        /// Reset every key (sends the vendor app's "Reset all button functions")
        #[arg(long)]
        all: bool,
    },
    /// List every action name the mouse understands
    Actions,
}

#[derive(Clone, Copy, ValueEnum)]
enum OnOff {
    On,
    Off,
}

#[derive(Clone, Copy, ValueEnum)]
enum ScrollDir {
    Forward,
    Reverse,
}

#[derive(Args, Clone)]
struct Tuning {
    /// 0-255 raw, or a percentage like 50% (the mouse's minimum is 12)
    #[arg(long, value_parser = parse_brightness)]
    brightness: Option<Brightness>,
    /// 20-255 raw, or a percentage like 50% (vendor default 128)
    #[arg(long, value_parser = parse_speed)]
    speed: Option<Speed>,
}

fn parse_percent_or_raw(s: &str, what: &str) -> Result<Result<u8, u8>, String> {
    let s = s.trim();
    if let Some(p) = s.strip_suffix('%') {
        let p: u8 = p
            .trim()
            .parse()
            .map_err(|_| format!("'{s}' is not a percentage"))?;
        if p > 100 {
            return Err("percentage must be 0-100".into());
        }
        return Ok(Err(p));
    }
    let raw: u8 = s
        .parse()
        .map_err(|_| format!("'{s}' is not a {what} (0-255 or a percentage)"))?;
    Ok(Ok(raw))
}

fn parse_brightness(s: &str) -> Result<Brightness, String> {
    Ok(match parse_percent_or_raw(s, "brightness")? {
        Ok(raw) => Brightness::new(raw),
        Err(pct) => Brightness::from_percent(pct),
    })
}

fn parse_speed(s: &str) -> Result<Speed, String> {
    Ok(match parse_percent_or_raw(s, "speed")? {
        Ok(raw) => Speed::new(raw),
        Err(pct) => Speed::from_percent(pct),
    })
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<ExitCode> {
    let state_path = state_path(cli.state.clone())?;
    match &cli.command {
        Cmd::Status => cmd_status(&cli),
        Cmd::Bluetooth { action } => cmd_bluetooth(action.as_ref()),
        Cmd::List => cmd_list(&cli),
        Cmd::Info => {
            let mut mouse = open(&cli)?;
            let snap = mouse.snapshot()?;
            print_info(&snap, &mouse.describe());
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Color {
            color,
            effect,
            tuning,
        } => {
            if !effect.has_color() {
                bail!(
                    "effect '{}' has no color; use `tailglow effect {}`",
                    effect.name(),
                    effect.name()
                );
            }
            let mut session = Session::open(&cli, &state_path)?;
            let mut lighting = session.snap.lighting.for_effect(*effect);
            lighting.color = *color;
            apply_tuning(&mut lighting, tuning);
            session.apply_lighting(lighting)
        }
        Cmd::Effect {
            effect,
            color,
            tuning,
        } => {
            let mut session = Session::open(&cli, &state_path)?;
            let mut lighting = session.snap.lighting.for_effect(*effect);
            if let Some(c) = color {
                lighting.color = *c;
            }
            apply_tuning(&mut lighting, tuning);
            session.apply_lighting(lighting)
        }
        Cmd::Dpi { action } => {
            let mut session = Session::open(&cli, &state_path)?;
            let current = session.snap.info.pointer.dpi;
            let new = match action {
                None => {
                    print_dpi(&current);
                    return Ok(ExitCode::SUCCESS);
                }
                Some(DpiCmd::Set { stages, active }) => {
                    let mut st = [0u16; 5];
                    for (i, s) in stages.iter().enumerate() {
                        let n = Dpi::normalize_value(*s);
                        if n != *s {
                            eprintln!(
                                "note: stage {} {s} adjusted to {n} (100-26000, steps of 50, like the vendor app)",
                                i + 1
                            );
                        }
                        st[i] = n;
                    }
                    Dpi {
                        stages: st,
                        active: match active {
                            Some(a) => stage_index(*a)?,
                            None => current.active,
                        },
                    }
                }
                Some(DpiCmd::Active { stage }) => Dpi {
                    active: stage_index(*stage)?,
                    ..current
                },
                Some(DpiCmd::Reset) => Dpi::FACTORY,
            };
            session.mouse.apply_dpi(&new)?;
            let back = session.mouse.pointer()?;
            if back.dpi != new {
                bail!("read-back mismatch after DPI write: {:?}", back.dpi);
            }
            session.snap.info.pointer = back;
            session.save()?;
            print_dpi(&new);
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Rate { hz } => {
            let mut session = Session::open(&cli, &state_path)?;
            let Some(rate) = hz else {
                println!(
                    "{}",
                    session
                        .snap
                        .info
                        .pointer
                        .report_rate()
                        .map(|r| r.to_string())
                        .unwrap_or_else(|| "unknown".into())
                );
                return Ok(ExitCode::SUCCESS);
            };
            session.mouse.apply_report_rate(*rate)?;
            let back = session.mouse.pointer()?;
            if back.report_rate() != Some(*rate) {
                bail!(
                    "read-back mismatch after report-rate write: slot byte 0x{:02x}",
                    back.slots[0]
                );
            }
            session.snap.info.pointer = back;
            session.save()?;
            println!("{rate}");
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Sensor {
            lift_off,
            ripple,
            angle_snap,
            motion_sync,
            scroll,
        } => {
            let mut session = Session::open(&cli, &state_path)?;
            let mut s = session.snap.info.pointer.sensor;
            let mut changed = false;
            if let Some(mm) = lift_off {
                s.lift_off = LiftOff::from_byte(*mm)
                    .ok_or_else(|| anyhow!("lift-off must be 1 or 2 (mm)"))?;
                changed = true;
            }
            for (flag, field) in [
                (ripple, &mut s.ripple),
                (angle_snap, &mut s.angle_snap),
                (motion_sync, &mut s.motion_sync),
            ] {
                if let Some(v) = flag {
                    *field = matches!(v, OnOff::On);
                    changed = true;
                }
            }
            if let Some(d) = scroll {
                s.scroll_reverse = matches!(d, ScrollDir::Reverse);
                changed = true;
            }
            if changed {
                session.mouse.apply_sensor(&s)?;
                let back = session.mouse.pointer()?;
                if back.sensor != s {
                    bail!(
                        "read-back mismatch after sensor write: flags 0x{:02x}",
                        back.sensor_flags
                    );
                }
                session.snap.info.pointer = back;
                session.save()?;
            }
            print_sensor(&s);
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Debounce { ms } => {
            let mut session = Session::open(&cli, &state_path)?;
            if let Some(ms) = ms {
                let d = Debounce::new(*ms).map_err(|e| anyhow!(e))?;
                if !d.is_observed() && cli.allow_untested {
                    eprintln!(
                        "warning: {}; writing because --allow-untested was given. {REPORT_URL}.",
                        d.untested_warning()
                    );
                }
                session.mouse.apply_debounce(d)?;
                let back = session.mouse.pointer()?;
                if back.debounce != d {
                    bail!(
                        "read-back mismatch after debounce write: {}",
                        back.debounce.ms()
                    );
                }
                session.snap.info.pointer = back;
                session.save()?;
                println!("debounce {} ms", d.ms());
            } else {
                println!("debounce {} ms", session.snap.info.pointer.debounce.ms());
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Buttons { action } => match action {
            None => {
                let mut mouse = open(&cli)?;
                let snap = mouse.snapshot()?;
                print_buttons(&snap.buttons);
                Ok(ExitCode::SUCCESS)
            }
            Some(ButtonsCmd::Actions) => {
                print_actions();
                Ok(ExitCode::SUCCESS)
            }
            Some(ButtonsCmd::Set { key, action }) => {
                let mut session = Session::open(&cli, &state_path)?;
                let map = session.snap.buttons.clone();
                session.mouse.apply_button(*key, action, Some(&map))?;
                let back = session.mouse.buttons()?;
                let got = back.action(*key);
                let want = if *action == ButtonAction::Default {
                    key.factory_action()
                } else {
                    action.clone()
                };
                if got != want {
                    bail!("read-back mismatch after button write: {key} reads {got}");
                }
                session.snap.buttons = back;
                session.state.forget_macro(*key);
                session.save()?;
                println!("{key} = {action} ({})", action.label());
                Ok(ExitCode::SUCCESS)
            }
            Some(ButtonsCmd::Reset { key, all }) => {
                let mut session = Session::open(&cli, &state_path)?;
                if *all {
                    session.mouse.reset(ResetKind::Buttons)?;
                    session.snap.buttons = session.mouse.buttons()?;
                    session.state.forget_macros();
                    session.save()?;
                    println!("all buttons reset to factory");
                    print_buttons(&session.snap.buttons);
                    return Ok(ExitCode::SUCCESS);
                }
                let key = key.ok_or_else(|| anyhow!("give a key, or --all"))?;
                let map = session.snap.buttons.clone();
                session
                    .mouse
                    .apply_button(key, &ButtonAction::Default, Some(&map))?;
                session.snap.buttons = session.mouse.buttons()?;
                session.state.forget_macro(key);
                session.save()?;
                println!("{key} = default ({})", key.factory_action().label());
                Ok(ExitCode::SUCCESS)
            }
        },
        Cmd::Macro {
            key,
            name,
            loop_mode,
            fixed_delay,
            steps,
        } => {
            let m = Macro {
                name: name.clone(),
                loop_mode: *loop_mode,
                fixed_delay_ms: *fixed_delay,
                events: steps.clone(),
            };
            m.validate().map_err(|e| anyhow!(e))?;
            let mut session = Session::open(&cli, &state_path)?;
            let map = session.snap.buttons.clone();
            session.mouse.apply_macro(*key, &m, Some(&map))?;
            // The mouse does not report macros: on the real M1 the 0x61 flag for
            // the key stayed 0 after a macro write (hw-test 2026-10-24), so there
            // is no read-back check here. The write itself was accepted.
            let back = session.mouse.buttons()?;
            eprintln!(
                "note: macros cannot be read back; the mouse {} {key} as remapped",
                if back.flags.is_remapped(*key) {
                    "flags"
                } else {
                    "does not flag"
                }
            );
            session.snap.buttons = back;
            let mut applied = Settings::default();
            applied.macros.insert(*key, m.clone());
            session.state.record_applied(&applied);
            session.save()?;
            println!(
                "{key} = macro \"{}\" ({} steps, {}, fixed delay {} ms) [experimental]",
                m.name,
                m.events.len(),
                m.loop_mode,
                m.fixed_delay_ms
            );
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Profile => {
            let mut mouse = open(&cli)?;
            let info = mouse.read_info()?;
            println!(
                "on-board profile index: {} (Profile {})",
                info.pointer.profile,
                info.pointer.profile + 1
            );
            println!(
                "Switching profiles is not supported: the vendor app's profile write (0x57) carries the \
                 key-pairing byte, which cannot be read back, and the mouse's profile byte never changed \
                 in the captures. Use `tailglow export` / `import` files as profiles instead."
            );
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Reset { what, yes } => {
            if !yes {
                bail!(
                    "this sends the vendor app's reset ({}) to the mouse; add --yes to confirm",
                    what.name()
                );
            }
            let mut session = Session::open(&cli, &state_path)?;
            session.mouse.reset(*what)?;
            session.snap = session.mouse.snapshot()?;
            if *what != ResetKind::Lighting {
                session.state.forget_macros();
            }
            session.save()?;
            println!("reset {} done", what.name());
            print_info(&session.snap, &session.mouse.describe());
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Export { file } => {
            let mut mouse = open(&cli)?;
            let snap = mouse.snapshot()?;
            // Macros cannot be read back: carry the ones tailglow assigned.
            let remembered = load_state_or_warn(&state_path).unwrap_or_default();
            let mut settings = Settings::from_snapshot_with_macros(&snap, &remembered);
            settings.sent_macros.clear();
            let text = settings.to_toml()?;
            match file {
                Some(path) => {
                    std::fs::write(path, &text)
                        .with_context(|| format!("writing {}", path.display()))?;
                    eprintln!("wrote {}", path.display());
                }
                None => print!("{text}"),
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Import { file, force } => {
            let text = std::fs::read_to_string(file)
                .with_context(|| format!("reading {}", file.display()))?;
            let settings = Settings::from_toml(&text)?;
            if settings.is_empty() {
                bail!(
                    "{} has no settings sections; nothing to apply",
                    file.display()
                );
            }
            let mut session = Session::open(&cli, &state_path)?;
            let out = session.apply_settings(&settings, *force)?;
            session.save()?;
            println!(
                "applied {} change(s) ({} report(s)) from {}{}",
                out.changes,
                out.reports,
                file.display(),
                if out.macros_skipped.is_empty() {
                    String::new()
                } else {
                    format!(
                        "; {} macro(s) already on the mouse, not re-sent",
                        out.macros_skipped.len()
                    )
                }
            );
            if !out.mismatches.is_empty() {
                for m in &out.mismatches {
                    eprintln!("read-back mismatch: {m}");
                }
                bail!(
                    "the mouse does not match {} after the import ({} mismatch(es))",
                    file.display(),
                    out.mismatches.len()
                );
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Watch {
            file,
            interval,
            once,
        } => cmd_watch(&cli, &state_path, file.as_deref(), *interval, *once),
        Cmd::Dump { file } => {
            let mut mouse = open(&cli)?;
            let mut lines = Vec::new();
            for op in tailglow_core::protocol::opcode::STARTUP_QUERIES {
                let r = mouse.query(op)?;
                lines.push(format!(
                    "  \"{op:02x}\": \"{}\"",
                    tailglow_core::protocol::hex(&r)
                ));
            }
            let flags = mouse.query_long(tailglow_core::protocol::opcode::BUTTON_FLAGS, 0)?;
            lines.push(format!(
                "  \"52-61\": \"{}\"",
                tailglow_core::protocol::hex(&flags)
            ));
            let text = format!(
                "{{\n \"node\": \"{}\",\n \"reads\": {{\n{}\n }}\n}}\n",
                mouse.describe(),
                lines.join(",\n")
            );
            match file {
                Some(path) => std::fs::write(path, &text)
                    .with_context(|| format!("writing {}", path.display()))?,
                None => print!("{text}"),
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}

// ----- helpers -----------------------------------------------------------

fn stage_index(one_based: u8) -> Result<u8> {
    if (1..=5).contains(&one_based) {
        Ok(one_based - 1)
    } else {
        bail!("stage must be 1-5")
    }
}

fn open(cli: &Cli) -> Result<Mouse<Box<dyn Transport>>> {
    let transport = match tailglow_hid::open(cli.fake, cli.device.as_deref()) {
        Ok(t) => t,
        Err(e @ tailglow_core::TransportError::NotFound) if cli.device.is_none() => {
            // Say so when the mouse is there, but only over Bluetooth.
            if let Some(m) = bluetooth_state().connected() {
                bail!(
                    "the {} is connected over Bluetooth only, which carries no settings \
                     (Keychron's firmware doesn't support it). Connect the USB cable to read or \
                     change settings; `tailglow status` shows the battery.",
                    m.model
                );
            }
            return Err(e.into());
        }
        Err(e) => return Err(e.into()),
    };
    let mut mouse = Mouse::new(transport);
    mouse.allow_untested(cli.allow_untested);
    Ok(mouse)
}

/// A connected mouse plus its snapshot and the host-side state file.
struct Session {
    mouse: Mouse<Box<dyn Transport>>,
    snap: Snapshot,
    state: Settings,
    state_path: PathBuf,
}

impl Session {
    fn open(cli: &Cli, state_path: &Path) -> Result<Session> {
        let mut mouse = open(cli)?;
        let snap = mouse.snapshot()?;
        let Some(model) = snap.info.model else {
            bail!(
                "{} is not a known Keychron mouse; refusing to write to it",
                snap.info.id()
            );
        };
        if model.status == SupportStatus::Likely && cli.allow_untested {
            eprintln!(
                "warning: {} ({}) was never tested with Tailglow; writing because --allow-untested was given. {REPORT_URL}.",
                model.name, model.id
            );
        }
        let state = load_state_or_warn(state_path).unwrap_or_default();
        Ok(Session {
            mouse,
            snap,
            state,
            state_path: state_path.to_path_buf(),
        })
    }

    fn apply_lighting(&mut self, lighting: Lighting) -> Result<ExitCode> {
        self.mouse.apply_lighting(&lighting)?;
        let back = self.mouse.lighting()?;
        if lighting.effect == Effect::Off {
            if back.effect != Effect::Off {
                bail!(
                    "read-back mismatch: mouse still reports effect {}",
                    back.effect.name()
                );
            }
        } else if back.current() != lighting {
            bail!(
                "read-back mismatch after lighting write: mouse reports {}",
                describe_lighting(&back.current())
            );
        }
        self.snap.lighting = back;
        self.save()?;
        println!("{}", describe_lighting(&lighting));
        Ok(ExitCode::SUCCESS)
    }

    /// Applies the sections present in `settings` (only what differs unless
    /// `force`; macros the state file says were already sent are skipped),
    /// then reads the mouse back and returns the outcome with any mismatches.
    fn apply_settings(&mut self, settings: &Settings, force: bool) -> Result<ApplyOutcome> {
        let out = self.mouse.apply_settings(
            settings,
            &ApplyOptions {
                force,
                sent_macros: self.state.sent_macros.clone(),
            },
        )?;
        self.state.record_applied(settings);
        self.snap = out.snapshot.clone();
        Ok(out)
    }

    /// Writes the state file: the mouse's current settings plus the macros
    /// tailglow assigned (which the mouse cannot report) and their
    /// fingerprints.
    fn save(&mut self) -> Result<()> {
        self.state = Settings::from_snapshot_with_macros(&self.snap, &self.state);
        if let Some(dir) = self.state_path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        std::fs::write(&self.state_path, self.state.to_toml()?)
            .with_context(|| format!("writing {}", self.state_path.display()))
    }
}

fn apply_tuning(l: &mut Lighting, t: &Tuning) {
    if let Some(b) = t.brightness {
        l.brightness = b;
    }
    if let Some(s) = t.speed {
        l.speed = s;
    }
}

/// BlueZ state for the CLI (one read; `Disabled` with the fake mouse).
fn bluetooth_state() -> BtState {
    match Bluez::from_env() {
        Ok(Some(bz)) => bz.state(),
        Ok(None) => BtState::Disabled,
        Err(e) => BtState::Unavailable(e.to_string()),
    }
}

fn battery_text(b: Option<u8>) -> String {
    b.map(|b| format!("battery {b}%"))
        .unwrap_or_else(|| "battery unknown".into())
}

fn cmd_status(cli: &Cli) -> Result<ExitCode> {
    // USB cable: one cheap read (0x06 device info) for the battery.
    let usb: Option<String> = if tailglow_hid::open_from_env(cli.fake).is_some()
        || cli.device.is_some()
    {
        match tailglow_hid::open(cli.fake, cli.device.as_deref()) {
            Err(tailglow_core::TransportError::NotFound) => None,
            Err(e) => return Err(e.into()),
            Ok(t) => {
                let mut mouse = Mouse::new(t);
                let info = mouse.device_info()?;
                Some(format!(
                    "{} via {}, battery {}%",
                    tailglow_core::device::lookup(tailglow_core::DeviceId::new(info.vid, info.pid))
                        .map(|m| m.name)
                        .unwrap_or("unknown model"),
                    mouse.describe(),
                    info.battery_percent
                ))
            }
        }
    } else {
        let found = tailglow_hid::enumerate().unwrap_or_default();
        found
            .iter()
            .find(|d| d.is_vendor_interface() && d.model.readable())
            .map(|d| match tailglow_hid::open_path(&d.path) {
                Ok((t, model)) => {
                    let mut mouse = Mouse::with_model(t, model);
                    match mouse.device_info() {
                        Ok(info) => format!(
                            "{} at {}, battery {}%",
                            d.model.name, d.path, info.battery_percent
                        ),
                        Err(e) => format!("{} at {} (read failed: {e})", d.model.name, d.path),
                    }
                }
                Err(e) if e.is_permission() => format!(
                    "{} at {}, NO PERMISSION (install 70-keychron-m1.rules)",
                    d.model.name, d.path
                ),
                Err(e) => format!("{} at {} ({e})", d.model.name, d.path),
            })
    };
    println!(
        "USB cable:  {}",
        usb.clone().unwrap_or_else(|| "not connected".into())
    );

    let bt = bluetooth_state();
    match &bt {
        BtState::Disabled => {
            println!("Bluetooth:  not checked (fake mouse, or TAILGLOW_BLUEZ_BUS=off)")
        }
        BtState::Unavailable(why) => println!("Bluetooth:  unavailable ({why})"),
        BtState::Mice(_) => match bt.connected() {
            Some(m) => println!(
                "Bluetooth:  {} connected (\"{}\", {}), {}",
                m.model,
                m.name,
                m.address,
                battery_text(m.battery)
            ),
            None => println!("Bluetooth:  no Keychron mouse connected"),
        },
    }
    let stale = bt.stale();
    if !stale.is_empty() {
        println!(
            "            {} more pairing(s) not connected ({}): `tailglow bluetooth` lists them, \
             `tailglow bluetooth forget-stale --yes` removes them",
            stale.len(),
            if bt.connected().is_some() {
                "old pairings"
            } else {
                "old pairings, or the mouse is asleep"
            }
        );
    }
    let settings = match (&usb, bt.connected()) {
        (Some(_), _) => "can be changed (USB cable)",
        (None, Some(_)) => {
            "read-only: settings can only be changed over the USB cable \
             (Keychron's firmware doesn't support it over Bluetooth). Settings you save stay in the mouse."
        }
        (None, None) => "no mouse connected",
    };
    println!("Settings:   {settings}");
    Ok(if usb.is_some() || bt.connected().is_some() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

fn cmd_bluetooth(action: Option<&BluetoothCmd>) -> Result<ExitCode> {
    let bz = match Bluez::from_env() {
        Ok(Some(bz)) => bz,
        Ok(None) => bail!("Bluetooth status is off (fake mouse, or TAILGLOW_BLUEZ_BUS=off)"),
        Err(e) => bail!("cannot reach BlueZ: {e}"),
    };
    let mice = bz.mice().map_err(|e| anyhow!("reading BlueZ: {e}"))?;
    match action {
        None => {
            if mice.is_empty() {
                println!("no Keychron mouse is paired over Bluetooth");
            }
            for m in &mice {
                println!(
                    "{:<40} {:<14} {:<18} {}",
                    m.path,
                    m.name,
                    m.address,
                    if m.connected {
                        format!("connected, {}", battery_text(m.battery))
                    } else if m.paired {
                        "paired, not connected (an old pairing, or the mouse is asleep)".into()
                    } else {
                        "seen, not paired".into()
                    }
                );
            }
            Ok(ExitCode::SUCCESS)
        }
        Some(BluetoothCmd::ForgetStale { yes }) => {
            let stale: Vec<_> = mice.iter().filter(|m| m.is_stale()).collect();
            if stale.is_empty() {
                println!("no old Keychron pairings to remove");
                return Ok(ExitCode::SUCCESS);
            }
            if !*yes {
                println!("would remove {} old pairing(s):", stale.len());
                for m in &stale {
                    println!("  {}  {}  {}", m.path, m.name, m.address);
                }
                if !mice.iter().any(|m| m.connected) {
                    println!(
                        "note: no Keychron mouse is connected right now, so one of these may be \
                         your current pairing with the mouse asleep; removing it means pairing \
                         the mouse again"
                    );
                }
                println!("run again with --yes to remove them (connected mice are never removed)");
                return Ok(ExitCode::FAILURE);
            }
            let mut failed = 0;
            for m in stale {
                match bz.remove_stale(m, UserConfirmed::after_explicit_confirmation()) {
                    Ok(()) => println!("removed {}  {}  {}", m.path, m.name, m.address),
                    Err(e) => {
                        failed += 1;
                        eprintln!("not removed {}: {e}", m.path);
                    }
                }
            }
            Ok(if failed == 0 {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            })
        }
    }
}

fn cmd_list(cli: &Cli) -> Result<ExitCode> {
    if tailglow_hid::open_from_env(cli.fake).is_some() {
        println!(
            "fake mouse (TAILGLOW_FAKE)  Keychron M1  3434:d035  interface 2  verified, ready"
        );
        return Ok(ExitCode::SUCCESS);
    }
    let found = tailglow_hid::enumerate()?;
    if found.is_empty() {
        println!("no Keychron mouse found (is it connected by USB cable?)");
        return Ok(ExitCode::FAILURE);
    }
    for d in &found {
        let role = if !d.model.readable() {
            format!("unsupported: {}", d.model.evidence)
        } else if d.is_vendor_interface() {
            match tailglow_hid::open_path(&d.path) {
                Ok(_) => "vendor interface, ready".to_string(),
                Err(e) if e.is_permission() => {
                    "vendor interface, NO PERMISSION (install 70-keychron-m1.rules)".to_string()
                }
                Err(e) => format!("vendor interface, error: {e}"),
            }
        } else {
            "input interface".to_string()
        };
        println!(
            "{:<14} {:<34} {}  interface {}  {}  {}",
            d.path,
            d.model.name,
            d.id,
            d.interface
                .map(|i| i.to_string())
                .unwrap_or_else(|| "?".into()),
            d.model.status.label(),
            role
        );
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_watch(
    cli: &Cli,
    state_path: &Path,
    file: Option<&Path>,
    interval: u64,
    once: bool,
) -> Result<ExitCode> {
    let source = file.unwrap_or(state_path);
    let mut present = false;
    loop {
        let found = if cli.fake || std::env::var_os("TAILGLOW_FAKE").is_some() {
            true
        } else {
            tailglow_hid::enumerate()?
                .iter()
                .any(|d| d.is_vendor_interface() && d.model.readable())
        };
        if found && !present {
            present = true;
            match std::fs::read_to_string(source) {
                Ok(text) => match Settings::from_toml(&text) {
                    Ok(settings) if !settings.is_empty() => {
                        // The node can appear a moment before udev applies the ACL.
                        std::thread::sleep(Duration::from_millis(500));
                        match Session::open(cli, state_path) {
                            Ok(mut session) => match session.apply_settings(&settings, false) {
                                Ok(out) => {
                                    if let Err(e) = session.save() {
                                        eprintln!("tailglow watch: {e:#}");
                                    }
                                    println!(
                                        "tailglow watch: mouse connected, {} change(s) re-applied from {}{}",
                                        out.changes,
                                        source.display(),
                                        if out.macros_skipped.is_empty() {
                                            String::new()
                                        } else {
                                            format!(
                                                " ({} macro(s) already on the mouse)",
                                                out.macros_skipped.len()
                                            )
                                        }
                                    );
                                    for m in &out.mismatches {
                                        eprintln!("tailglow watch: read-back mismatch: {m}");
                                    }
                                }
                                Err(e) => eprintln!("tailglow watch: apply failed: {e:#}"),
                            },
                            Err(e) => {
                                eprintln!("tailglow watch: {e:#}");
                                present = false; // retry next poll
                            }
                        }
                    }
                    Ok(_) => println!(
                        "tailglow watch: {} has no settings; nothing to apply",
                        source.display()
                    ),
                    Err(e) => eprintln!("tailglow watch: {}: {e}", source.display()),
                },
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    println!(
                        "tailglow watch: no saved settings yet ({})",
                        source.display()
                    )
                }
                Err(e) => eprintln!("tailglow watch: {}: {e}", source.display()),
            }
            if once {
                return Ok(ExitCode::SUCCESS);
            }
        } else if !found && present {
            present = false;
            println!("tailglow watch: mouse disconnected");
        } else if once && !found {
            println!("tailglow watch: no mouse present");
            return Ok(ExitCode::FAILURE);
        }
        std::thread::sleep(Duration::from_secs(interval.max(1)));
    }
}

// ----- printing ------------------------------------------------------------

fn print_info(snap: &Snapshot, via: &str) {
    let info = &snap.info;
    let p = &info.pointer;
    println!(
        "Device:   {} ({}) via {}  [{}]",
        info.model_name(),
        info.id(),
        via,
        info.status().map(|s| s.label()).unwrap_or("unknown")
    );
    println!("Firmware: {}", info.firmware.version);
    println!("Battery:  {}%", info.device.battery_percent);
    println!("Profile:  {} (on-board index {})", p.profile + 1, p.profile);
    println!("Lighting: {}", describe_lighting(&snap.lighting.current()));
    for e in Effect::ALL.iter().skip(1) {
        let l = snap.lighting.for_effect(*e);
        println!(
            "          {:<14} brightness {:>3} speed {:>3}{}",
            e.label(),
            l.brightness.raw(),
            l.speed.raw(),
            if e.has_color() {
                format!(" color {}", l.color.to_hex())
            } else {
                String::new()
            }
        );
    }
    print_dpi(&p.dpi);
    println!(
        "Rate:     {}",
        p.report_rate()
            .map(|r| r.to_string())
            .unwrap_or_else(|| "unknown".into())
    );
    print_sensor(&p.sensor);
    println!("Debounce: {} ms", p.debounce.ms());
    print_buttons(&snap.buttons);
    if info.model.is_none() {
        eprintln!(
            "warning: {} is not in the device table; writes are refused",
            info.id()
        );
    } else if info.status() == Some(SupportStatus::Likely) {
        eprintln!(
            "note: this model is likely compatible but untested; writes need --allow-untested. {REPORT_URL}."
        );
    }
}

fn print_dpi(d: &Dpi) {
    let stages: Vec<String> = d
        .stages
        .iter()
        .enumerate()
        .map(|(i, s)| {
            if i as u8 == d.active {
                format!("[{s}]")
            } else {
                s.to_string()
            }
        })
        .collect();
    println!(
        "DPI:      {}  (active stage {} = {} DPI, indicator {})",
        stages.join(" "),
        d.active + 1,
        d.active_dpi(),
        Dpi::INDICATOR_COLORS[d.active as usize]
    );
}

fn print_sensor(s: &SensorSettings) {
    let onoff = |b: bool| if b { "on" } else { "off" };
    println!(
        "Sensor:   lift-off {} mm, ripple {}, angle snap {}, motion sync {}, scroll {}",
        s.lift_off.mm(),
        onoff(s.ripple),
        onoff(s.angle_snap),
        onoff(s.motion_sync),
        if s.scroll_reverse {
            "reverse"
        } else {
            "forward"
        }
    );
}

fn print_buttons(map: &ButtonMap) {
    println!("Buttons:");
    for key in Key::ALL {
        let a = map.action(key);
        let mark = if map.flags.is_remapped(key) { "*" } else { " " };
        println!("  {mark} {:<12} {:<28} {}", key.name(), a.label(), a);
    }
    if map.flags.remapped_keys().is_empty() {
        println!("    (all factory; * marks keys the mouse reports as remapped)");
    }
}

fn print_actions() {
    use tailglow_core::buttons::*;
    println!(
        "mouse:     {}",
        MouseFunction::ALL.map(MouseFunction::name).join(", ")
    );
    println!("keyboard:  key:ctrl+shift+k, key:f7, key:super+0x1e  (modifiers ctrl, shift, alt, super; a-z, 0-9, f1-f12, enter, esc, tab, space, arrows…)");
    println!(
        "media:     consumer:{}",
        ConsumerFunction::ALL
            .map(ConsumerFunction::name)
            .join(", consumer:")
    );
    println!("dpi:       dpi:loop, dpi:up, dpi:down");
    println!(
        "lighting:  light:{}",
        LightingFunction::ALL
            .map(LightingFunction::name)
            .join(", light:")
    );
    println!("auto-fire: autofire:BUTTON,INTERVALms,TIMES  e.g. autofire:left-click,37ms,5");
    println!(
        "system:    system:{}",
        SystemShortcut::ALL
            .map(SystemShortcut::name)
            .join(", system:")
    );
    println!("other:     disabled, default");
    println!("macros:    tailglow macro KEY --name NAME [--loop times:3|until-release|until-any-click] [--fixed-delay MS] a+ 47ms a- …");
}

fn describe_lighting(l: &Lighting) -> String {
    let mut s = l.effect.name().to_string();
    if l.effect.has_color() {
        s.push_str(&format!(" {}", l.color.to_hex()));
    }
    if l.effect.has_brightness_and_speed() {
        s.push_str(&format!(
            " brightness {}% ({}) speed {}",
            l.brightness.percent(),
            l.brightness.raw(),
            l.speed.raw()
        ));
        if let Some(p) = l.speed.period_seconds(l.effect) {
            s.push_str(&format!(" (~{p:.1} s period)"));
        }
    }
    s
}

fn state_path(explicit: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(p) = explicit {
        return Ok(p);
    }
    let dir = tailglow_hid::paths::config_dir()
        .ok_or_else(|| anyhow!("cannot find a config directory; pass --state FILE"))?;
    Ok(dir.join("state.toml"))
}

/// The state file, or `None` with a warning when it is missing or unreadable
/// (a damaged state file must not block every command).
fn load_state_or_warn(path: &Path) -> Option<Settings> {
    match load_state(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("warning: ignoring the state file: {e:#}");
            None
        }
    }
}

fn load_state(path: &Path) -> Result<Option<Settings>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(
            Settings::from_toml(&text).with_context(|| format!("reading {}", path.display()))?,
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brightness_and_speed_arg_forms() {
        assert_eq!(parse_brightness("255").unwrap().raw(), 0xff);
        assert_eq!(parse_brightness("0").unwrap().raw(), 0x0c);
        assert_eq!(parse_brightness("100%").unwrap().raw(), 0xff);
        assert_eq!(parse_brightness("0%").unwrap().raw(), 0x0c);
        assert!(parse_brightness("300").is_err());
        assert!(parse_brightness("101%").is_err());
        assert!(parse_brightness("bright").is_err());
        assert_eq!(parse_speed("0").unwrap().raw(), 0x14);
        assert_eq!(parse_speed("128").unwrap().raw(), 0x80);
        assert_eq!(parse_speed("100%").unwrap().raw(), 0xff);
    }

    #[test]
    fn cli_parses() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }

    #[test]
    fn stage_indexes() {
        assert_eq!(stage_index(1).unwrap(), 0);
        assert_eq!(stage_index(5).unwrap(), 4);
        assert!(stage_index(0).is_err());
        assert!(stage_index(6).is_err());
    }
}
