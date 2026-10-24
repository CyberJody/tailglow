// SPDX-License-Identifier: GPL-3.0-or-later
//! Button mapping and macros (report 0x52), from PROTOCOL.md §7.
//!
//! Every byte here was seen on the wire in `testdata/session2.pcap`. The
//! key index order (Key1 … Key7, Scroll Up, Scroll Down) is *verified* for
//! Key 4 (index 3) and Key 5 (index 4) and *likely* for the rest.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// One physical button, by the index the mouse uses (byte 2 of 0x52 reports).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Key {
    Key1 = 0,
    Key2 = 1,
    Key3 = 2,
    Key4 = 3,
    Key5 = 4,
    Key6 = 5,
    Key7 = 6,
    ScrollUp = 7,
    ScrollDown = 8,
}

impl Key {
    pub const ALL: [Key; 9] = [
        Key::Key1,
        Key::Key2,
        Key::Key3,
        Key::Key4,
        Key::Key5,
        Key::Key6,
        Key::Key7,
        Key::ScrollUp,
        Key::ScrollDown,
    ];

    pub const COUNT: usize = 9;

    pub fn index(self) -> u8 {
        self as u8
    }

    pub fn from_index(i: u8) -> Option<Key> {
        Key::ALL.get(i as usize).copied()
    }

    /// Config / CLI name: `key1` … `key7`, `scroll-up`, `scroll-down`.
    pub fn name(self) -> &'static str {
        match self {
            Key::Key1 => "key1",
            Key::Key2 => "key2",
            Key::Key3 => "key3",
            Key::Key4 => "key4",
            Key::Key5 => "key5",
            Key::Key6 => "key6",
            Key::Key7 => "key7",
            Key::ScrollUp => "scroll-up",
            Key::ScrollDown => "scroll-down",
        }
    }

    /// The vendor app's label.
    pub fn label(self) -> &'static str {
        match self {
            Key::Key1 => "Key 1 (left)",
            Key::Key2 => "Key 2 (right)",
            Key::Key3 => "Key 3 (wheel)",
            // Side-button positions from the vendor app's own button diagram
            // (its Buttons page, *likely*): Key 4/5 on the left
            // flank, Key 6/7 mirrored on the right flank, front then rear.
            Key::Key4 => "Key 4 (left side, front)",
            Key::Key5 => "Key 5 (left side, rear)",
            Key::Key6 => "Key 6 (right side, front)",
            Key::Key7 => "Key 7 (right side, rear)",
            Key::ScrollUp => "Scroll Up",
            Key::ScrollDown => "Scroll Down",
        }
    }

    /// Factory function shown by the vendor app at baseline (*verified* from
    /// the app's Buttons page; the mouse itself reports type 00).
    pub fn factory_action(self) -> ButtonAction {
        match self {
            Key::Key1 => ButtonAction::Mouse(MouseFunction::Left),
            Key::Key2 => ButtonAction::Mouse(MouseFunction::Right),
            Key::Key3 => ButtonAction::Mouse(MouseFunction::Middle),
            Key::Key4 | Key::Key6 => ButtonAction::Mouse(MouseFunction::Forward),
            Key::Key5 | Key::Key7 => ButtonAction::Mouse(MouseFunction::Backward),
            Key::ScrollUp => ButtonAction::Mouse(MouseFunction::ScrollUp),
            Key::ScrollDown => ButtonAction::Mouse(MouseFunction::ScrollDown),
        }
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl FromStr for Key {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let k = s.trim().to_ascii_lowercase().replace(['_', ' '], "-");
        let k = k.as_str();
        let alias = match k {
            "up" | "scrollup" | "scr-up" => Some(Key::ScrollUp),
            "down" | "scrolldown" | "scr-dn" | "scr-down" => Some(Key::ScrollDown),
            _ => None,
        };
        Key::ALL
            .iter()
            .copied()
            .find(|key| key.name() == k || key.name().trim_start_matches("key") == k)
            .or(alias)
            .ok_or_else(|| {
                format!(
                    "unknown key '{s}' (expected one of: {})",
                    Key::ALL.map(Key::name).join(", ")
                )
            })
    }
}

impl Serialize for Key {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.name())
    }
}

impl<'de> Deserialize<'de> for Key {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        String::deserialize(d)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

/// Type 01 payloads (*verified*: each was selected in the vendor app).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MouseFunction {
    Left,
    Right,
    Middle,
    /// HID button 4; the app calls it "Forward".
    Forward,
    /// HID button 5; the app calls it "Backward".
    Backward,
    DoubleClick,
    ScrollUp,
    ScrollDown,
    ScrollLeft,
    ScrollRight,
}

impl MouseFunction {
    pub const ALL: [MouseFunction; 10] = [
        MouseFunction::Left,
        MouseFunction::Right,
        MouseFunction::Middle,
        MouseFunction::Forward,
        MouseFunction::Backward,
        MouseFunction::DoubleClick,
        MouseFunction::ScrollUp,
        MouseFunction::ScrollDown,
        MouseFunction::ScrollLeft,
        MouseFunction::ScrollRight,
    ];

    /// `[buttons, wheel, hwheel]`.
    pub fn payload(self) -> [u8; 3] {
        match self {
            MouseFunction::Left => [0x01, 0, 0],
            MouseFunction::Right => [0x02, 0, 0],
            MouseFunction::Middle => [0x04, 0, 0],
            MouseFunction::Forward => [0x08, 0, 0],
            MouseFunction::Backward => [0x10, 0, 0],
            MouseFunction::DoubleClick => [0x80, 0, 0],
            MouseFunction::ScrollUp => [0, 0x02, 0],
            MouseFunction::ScrollDown => [0, 0xfe, 0],
            MouseFunction::ScrollLeft => [0, 0, 0xfe],
            MouseFunction::ScrollRight => [0, 0, 0x02],
        }
    }

    pub fn from_payload(p: &[u8]) -> Option<MouseFunction> {
        let mut want = [0u8; 3];
        for (i, b) in p.iter().take(3).enumerate() {
            want[i] = *b;
        }
        if p.iter().skip(3).any(|&b| b != 0) {
            return None;
        }
        MouseFunction::ALL
            .iter()
            .copied()
            .find(|f| f.payload() == want)
    }

    pub fn name(self) -> &'static str {
        match self {
            MouseFunction::Left => "left-click",
            MouseFunction::Right => "right-click",
            MouseFunction::Middle => "middle-click",
            MouseFunction::Forward => "forward",
            MouseFunction::Backward => "backward",
            MouseFunction::DoubleClick => "double-click",
            MouseFunction::ScrollUp => "scroll-up",
            MouseFunction::ScrollDown => "scroll-down",
            MouseFunction::ScrollLeft => "scroll-left",
            MouseFunction::ScrollRight => "scroll-right",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            MouseFunction::Left => "Left-Click",
            MouseFunction::Right => "Right-Click",
            MouseFunction::Middle => "Middle-Click",
            MouseFunction::Forward => "Forward",
            MouseFunction::Backward => "Backward",
            MouseFunction::DoubleClick => "Double-Click",
            MouseFunction::ScrollUp => "Scroll Up",
            MouseFunction::ScrollDown => "Scroll Down",
            MouseFunction::ScrollLeft => "Scroll Left",
            MouseFunction::ScrollRight => "Scroll Right",
        }
    }
}

/// Type 03 payloads (consumer-control usages, *verified*).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConsumerFunction {
    VolumeUp,
    VolumeDown,
    Mute,
    Player,
    PlayPause,
    Previous,
    Next,
    Stop,
}

impl ConsumerFunction {
    pub const ALL: [ConsumerFunction; 8] = [
        ConsumerFunction::VolumeUp,
        ConsumerFunction::VolumeDown,
        ConsumerFunction::Mute,
        ConsumerFunction::Player,
        ConsumerFunction::PlayPause,
        ConsumerFunction::Previous,
        ConsumerFunction::Next,
        ConsumerFunction::Stop,
    ];

    pub fn usage(self) -> u16 {
        match self {
            ConsumerFunction::VolumeUp => 0xe9,
            ConsumerFunction::VolumeDown => 0xea,
            ConsumerFunction::Mute => 0xe2,
            ConsumerFunction::Player => 0x0183,
            ConsumerFunction::PlayPause => 0xcd,
            ConsumerFunction::Previous => 0xb6,
            ConsumerFunction::Next => 0xb5,
            ConsumerFunction::Stop => 0xb7,
        }
    }

    pub fn from_usage(u: u16) -> Option<ConsumerFunction> {
        ConsumerFunction::ALL
            .iter()
            .copied()
            .find(|f| f.usage() == u)
    }

    pub fn name(self) -> &'static str {
        match self {
            ConsumerFunction::VolumeUp => "volume-up",
            ConsumerFunction::VolumeDown => "volume-down",
            ConsumerFunction::Mute => "mute",
            ConsumerFunction::Player => "player",
            ConsumerFunction::PlayPause => "play-pause",
            ConsumerFunction::Previous => "previous",
            ConsumerFunction::Next => "next",
            ConsumerFunction::Stop => "stop",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ConsumerFunction::VolumeUp => "Volume +",
            ConsumerFunction::VolumeDown => "Volume −",
            ConsumerFunction::Mute => "Mute",
            ConsumerFunction::Player => "Media Player",
            ConsumerFunction::PlayPause => "Play / Pause",
            ConsumerFunction::Previous => "Previous Track",
            ConsumerFunction::Next => "Next Track",
            ConsumerFunction::Stop => "Stop",
        }
    }
}

/// Type 05 payloads (*verified*). The M1 has no hold-for-low-DPI ("sniper")
/// action; these three are all the app offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DpiFunction {
    Loop = 1,
    Up = 2,
    Down = 3,
}

impl DpiFunction {
    pub const ALL: [DpiFunction; 3] = [DpiFunction::Loop, DpiFunction::Up, DpiFunction::Down];

    pub fn from_byte(b: u8) -> Option<DpiFunction> {
        DpiFunction::ALL.iter().copied().find(|f| *f as u8 == b)
    }

    pub fn name(self) -> &'static str {
        match self {
            DpiFunction::Loop => "loop",
            DpiFunction::Up => "up",
            DpiFunction::Down => "down",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            DpiFunction::Loop => "DPI Loop",
            DpiFunction::Up => "DPI +",
            DpiFunction::Down => "DPI −",
        }
    }
}

/// Type 06 payloads (*verified*).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LightingFunction {
    Effect = 1,
    Speed = 2,
    Color = 3,
    BrightnessUp = 4,
    BrightnessDown = 5,
}

impl LightingFunction {
    pub const ALL: [LightingFunction; 5] = [
        LightingFunction::Effect,
        LightingFunction::Speed,
        LightingFunction::Color,
        LightingFunction::BrightnessUp,
        LightingFunction::BrightnessDown,
    ];

    pub fn from_byte(b: u8) -> Option<LightingFunction> {
        LightingFunction::ALL
            .iter()
            .copied()
            .find(|f| *f as u8 == b)
    }

    pub fn name(self) -> &'static str {
        match self {
            LightingFunction::Effect => "effect",
            LightingFunction::Speed => "speed",
            LightingFunction::Color => "color",
            LightingFunction::BrightnessUp => "brightness-up",
            LightingFunction::BrightnessDown => "brightness-down",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            LightingFunction::Effect => "Next Effect",
            LightingFunction::Speed => "Next Speed",
            LightingFunction::Color => "Next Color",
            LightingFunction::BrightnessUp => "Brightness +",
            LightingFunction::BrightnessDown => "Brightness −",
        }
    }
}

/// Type 08 payloads, exactly as captured (*verified* bytes). The trailing
/// `00 00 01` of Switch App is *unknown*, so the list is closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SystemShortcut {
    BrightnessUp,
    BrightnessDown,
    Calculator,
    MyComputer,
    Website,
    Mail,
    Refresh,
    Copy,
    Cut,
    Paste,
    SwitchApp,
}

impl SystemShortcut {
    pub const ALL: [SystemShortcut; 11] = [
        SystemShortcut::BrightnessUp,
        SystemShortcut::BrightnessDown,
        SystemShortcut::Calculator,
        SystemShortcut::MyComputer,
        SystemShortcut::Website,
        SystemShortcut::Mail,
        SystemShortcut::Refresh,
        SystemShortcut::Copy,
        SystemShortcut::Cut,
        SystemShortcut::Paste,
        SystemShortcut::SwitchApp,
    ];

    pub fn payload(self) -> &'static [u8] {
        match self {
            SystemShortcut::BrightnessUp => &[0x0c, 0x6f, 0x00],
            SystemShortcut::BrightnessDown => &[0x0c, 0x70, 0x00],
            SystemShortcut::Calculator => &[0x0c, 0x92, 0x01],
            SystemShortcut::MyComputer => &[0x0c, 0x94, 0x01],
            SystemShortcut::Website => &[0x0c, 0x23, 0x02],
            SystemShortcut::Mail => &[0x0c, 0x8a, 0x01],
            SystemShortcut::Refresh => &[0x07, 0x00, 0x3e],
            SystemShortcut::Copy => &[0x07, 0x01, 0x06],
            SystemShortcut::Cut => &[0x07, 0x01, 0x1b],
            SystemShortcut::Paste => &[0x07, 0x01, 0x19],
            SystemShortcut::SwitchApp => &[0x07, 0x04, 0x2b, 0x00, 0x00, 0x01],
        }
    }

    /// Matches ignoring trailing zero padding on either side.
    pub fn from_payload(p: &[u8]) -> Option<SystemShortcut> {
        SystemShortcut::ALL.iter().copied().find(|s| {
            let want = s.payload();
            let n = want.len().max(p.len());
            (0..n).all(|i| want.get(i).copied().unwrap_or(0) == p.get(i).copied().unwrap_or(0))
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            SystemShortcut::BrightnessUp => "screen-brightness-up",
            SystemShortcut::BrightnessDown => "screen-brightness-down",
            SystemShortcut::Calculator => "calculator",
            SystemShortcut::MyComputer => "my-computer",
            SystemShortcut::Website => "website",
            SystemShortcut::Mail => "mail",
            SystemShortcut::Refresh => "refresh",
            SystemShortcut::Copy => "copy",
            SystemShortcut::Cut => "cut",
            SystemShortcut::Paste => "paste",
            SystemShortcut::SwitchApp => "switch-app",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            SystemShortcut::BrightnessUp => "Screen Brightness +",
            SystemShortcut::BrightnessDown => "Screen Brightness −",
            SystemShortcut::Calculator => "Calculator",
            SystemShortcut::MyComputer => "My Computer",
            SystemShortcut::Website => "Website",
            SystemShortcut::Mail => "Mail",
            SystemShortcut::Refresh => "Refresh",
            SystemShortcut::Copy => "Copy",
            SystemShortcut::Cut => "Cut",
            SystemShortcut::Paste => "Paste",
            SystemShortcut::SwitchApp => "Switch App",
        }
    }
}

/// HID keyboard modifier bits (left-hand set; the app sent `03` for
/// Ctrl+Shift, *verified*).
pub mod modifier {
    pub const CTRL: u8 = 0x01;
    pub const SHIFT: u8 = 0x02;
    pub const ALT: u8 = 0x04;
    pub const SUPER: u8 = 0x08;
}

/// A keyboard shortcut: modifier bits + one HID usage (keyboard page).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyCombo {
    pub modifiers: u8,
    pub usage: u8,
}

/// HID keyboard-page usage names used by the CLI and settings files.
const KEY_NAMES: &[(&str, u8)] = &[
    ("a", 0x04),
    ("b", 0x05),
    ("c", 0x06),
    ("d", 0x07),
    ("e", 0x08),
    ("f", 0x09),
    ("g", 0x0a),
    ("h", 0x0b),
    ("i", 0x0c),
    ("j", 0x0d),
    ("k", 0x0e),
    ("l", 0x0f),
    ("m", 0x10),
    ("n", 0x11),
    ("o", 0x12),
    ("p", 0x13),
    ("q", 0x14),
    ("r", 0x15),
    ("s", 0x16),
    ("t", 0x17),
    ("u", 0x18),
    ("v", 0x19),
    ("w", 0x1a),
    ("x", 0x1b),
    ("y", 0x1c),
    ("z", 0x1d),
    ("1", 0x1e),
    ("2", 0x1f),
    ("3", 0x20),
    ("4", 0x21),
    ("5", 0x22),
    ("6", 0x23),
    ("7", 0x24),
    ("8", 0x25),
    ("9", 0x26),
    ("0", 0x27),
    ("enter", 0x28),
    ("esc", 0x29),
    ("backspace", 0x2a),
    ("tab", 0x2b),
    ("space", 0x2c),
    ("minus", 0x2d),
    ("equal", 0x2e),
    ("leftbrace", 0x2f),
    ("rightbrace", 0x30),
    ("backslash", 0x31),
    ("semicolon", 0x33),
    ("apostrophe", 0x34),
    ("grave", 0x35),
    ("comma", 0x36),
    ("dot", 0x37),
    ("slash", 0x38),
    ("capslock", 0x39),
    ("f1", 0x3a),
    ("f2", 0x3b),
    ("f3", 0x3c),
    ("f4", 0x3d),
    ("f5", 0x3e),
    ("f6", 0x3f),
    ("f7", 0x40),
    ("f8", 0x41),
    ("f9", 0x42),
    ("f10", 0x43),
    ("f11", 0x44),
    ("f12", 0x45),
    ("printscreen", 0x46),
    ("scrolllock", 0x47),
    ("pause", 0x48),
    ("insert", 0x49),
    ("home", 0x4a),
    ("pageup", 0x4b),
    ("delete", 0x4c),
    ("end", 0x4d),
    ("pagedown", 0x4e),
    ("right", 0x4f),
    ("left", 0x50),
    ("down", 0x51),
    ("up", 0x52),
    ("numlock", 0x53),
];

/// HID usage for a key name (`a`, `f7`, `enter`, or `0x1e`).
pub fn key_usage(name: &str) -> Option<u8> {
    let n = name.trim().to_ascii_lowercase();
    if let Some(hex) = n.strip_prefix("0x") {
        return u8::from_str_radix(hex, 16).ok();
    }
    KEY_NAMES.iter().find(|(k, _)| *k == n).map(|(_, u)| *u)
}

/// Name for a HID usage, or `0xNN`.
pub fn key_name(usage: u8) -> String {
    KEY_NAMES
        .iter()
        .find(|(_, u)| *u == usage)
        .map(|(k, _)| k.to_string())
        .unwrap_or_else(|| format!("0x{usage:02x}"))
}

impl KeyCombo {
    /// Parses `ctrl+shift+k`, `f7`, `super+0x1e`.
    pub fn parse(s: &str) -> Result<KeyCombo, String> {
        let mut modifiers = 0u8;
        let mut usage = None;
        for part in s.split('+').map(str::trim).filter(|p| !p.is_empty()) {
            match part.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => modifiers |= modifier::CTRL,
                "shift" => modifiers |= modifier::SHIFT,
                "alt" => modifiers |= modifier::ALT,
                "super" | "win" | "meta" | "gui" | "cmd" => modifiers |= modifier::SUPER,
                key => {
                    if usage.is_some() {
                        return Err(format!("'{s}': only one non-modifier key is allowed"));
                    }
                    usage = Some(key_usage(key).ok_or_else(|| {
                        format!("'{key}' is not a known key name (try a-z, 0-9, f1-f12, enter, esc, tab, space, or 0xNN)")
                    })?);
                }
            }
        }
        Ok(KeyCombo {
            modifiers,
            usage: usage.ok_or_else(|| format!("'{s}' has no key"))?,
        })
    }
}

impl fmt::Display for KeyCombo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut parts = Vec::new();
        if self.modifiers & modifier::CTRL != 0 {
            parts.push("ctrl".to_string());
        }
        if self.modifiers & modifier::SHIFT != 0 {
            parts.push("shift".to_string());
        }
        if self.modifiers & modifier::ALT != 0 {
            parts.push("alt".to_string());
        }
        if self.modifiers & modifier::SUPER != 0 {
            parts.push("super".to_string());
        }
        parts.push(key_name(self.usage));
        f.write_str(&parts.join("+"))
    }
}

/// Type 07 "Game Enhancement": repeat a click at a fixed rate (*verified* for
/// the mouse source; the keyboard source was not captured and is refused).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AutoFire {
    pub button: MouseFunction,
    pub interval_ms: u16,
    pub times: u8,
}

/// What a button does. `type_byte()` + `payload()` form bytes 4.. of the
/// 0x52 write; [`ButtonAction::parse_payload`] is the inverse for 0x62 replies.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ButtonAction {
    /// Type 00: the key's factory function.
    Default,
    Mouse(MouseFunction),
    Keyboard(KeyCombo),
    Consumer(ConsumerFunction),
    Dpi(DpiFunction),
    Lighting(LightingFunction),
    AutoFire(AutoFire),
    System(SystemShortcut),
    Disabled,
    /// Anything the mouse reported that we don't decode (kept verbatim).
    Raw {
        type_byte: u8,
        payload: Vec<u8>,
    },
}

impl ButtonAction {
    pub fn type_byte(&self) -> u8 {
        match self {
            ButtonAction::Default => 0x00,
            ButtonAction::Mouse(_) => 0x01,
            ButtonAction::Keyboard(_) => 0x02,
            ButtonAction::Consumer(_) => 0x03,
            ButtonAction::Dpi(_) => 0x05,
            ButtonAction::Lighting(_) => 0x06,
            ButtonAction::AutoFire(_) => 0x07,
            ButtonAction::System(_) => 0x08,
            ButtonAction::Disabled => 0x09,
            ButtonAction::Raw { type_byte, .. } => *type_byte,
        }
    }

    /// Bytes after the type byte, without padding.
    pub fn payload(&self) -> Vec<u8> {
        match self {
            ButtonAction::Default | ButtonAction::Disabled => vec![],
            ButtonAction::Mouse(m) => m.payload().to_vec(),
            ButtonAction::Keyboard(k) => vec![k.modifiers, k.usage],
            ButtonAction::Consumer(c) => c.usage().to_le_bytes().to_vec(),
            ButtonAction::Dpi(d) => vec![*d as u8],
            ButtonAction::Lighting(l) => vec![*l as u8],
            ButtonAction::AutoFire(a) => {
                let [btn, ..] = a.button.payload();
                let [lo, hi] = a.interval_ms.to_le_bytes();
                vec![0x01, btn, lo, hi, a.times]
            }
            ButtonAction::System(s) => s.payload().to_vec(),
            ButtonAction::Raw { payload, .. } => payload.clone(),
        }
    }

    /// Decodes a type byte + payload (from a 0x62 reply or a 0x52 write).
    pub fn parse_payload(type_byte: u8, payload: &[u8]) -> ButtonAction {
        let trimmed_len = payload
            .iter()
            .rposition(|&b| b != 0)
            .map(|p| p + 1)
            .unwrap_or(0);
        let p = &payload[..trimmed_len];
        let raw = || ButtonAction::Raw {
            type_byte,
            payload: p.to_vec(),
        };
        match type_byte {
            0x00 if p.is_empty() => ButtonAction::Default,
            0x01 => MouseFunction::from_payload(p)
                .map(ButtonAction::Mouse)
                .unwrap_or_else(raw),
            0x02 if p.len() == 2 || (p.len() == 1) => ButtonAction::Keyboard(KeyCombo {
                modifiers: p[0],
                usage: p.get(1).copied().unwrap_or(0),
            }),
            0x03 if !p.is_empty() && p.len() <= 2 => {
                let u = u16::from_le_bytes([p[0], p.get(1).copied().unwrap_or(0)]);
                ConsumerFunction::from_usage(u)
                    .map(ButtonAction::Consumer)
                    .unwrap_or_else(raw)
            }
            0x05 if p.len() == 1 => DpiFunction::from_byte(p[0])
                .map(ButtonAction::Dpi)
                .unwrap_or_else(raw),
            0x06 if p.len() == 1 => LightingFunction::from_byte(p[0])
                .map(ButtonAction::Lighting)
                .unwrap_or_else(raw),
            0x07 if p.len() == 5 && p[0] == 0x01 => MouseFunction::from_payload(&[p[1]])
                .map(|button| {
                    ButtonAction::AutoFire(AutoFire {
                        button,
                        interval_ms: u16::from_le_bytes([p[2], p[3]]),
                        times: p[4],
                    })
                })
                .unwrap_or_else(raw),
            0x08 => SystemShortcut::from_payload(p)
                .map(ButtonAction::System)
                .unwrap_or_else(raw),
            0x09 if p.is_empty() => ButtonAction::Disabled,
            _ => raw(),
        }
    }

    pub fn is_left_click(&self) -> bool {
        matches!(self, ButtonAction::Mouse(MouseFunction::Left))
    }

    /// Human-readable label (vendor app wording where it exists).
    pub fn label(&self) -> String {
        match self {
            ButtonAction::Default => "Default".into(),
            ButtonAction::Mouse(m) => m.label().into(),
            ButtonAction::Keyboard(k) => format!("Key {k}"),
            ButtonAction::Consumer(c) => c.label().into(),
            ButtonAction::Dpi(d) => d.label().into(),
            ButtonAction::Lighting(l) => l.label().into(),
            ButtonAction::AutoFire(a) => format!(
                "Auto-fire {} every {} ms ×{}",
                a.button.label(),
                a.interval_ms,
                a.times
            ),
            ButtonAction::System(s) => s.label().into(),
            ButtonAction::Disabled => "Disabled".into(),
            ButtonAction::Raw { type_byte, payload } => format!(
                "Unknown (type 0x{type_byte:02x} {})",
                crate::protocol::hex(payload)
            ),
        }
    }
}

/// Text form used by the CLI and settings files:
/// `default`, `left-click` … `scroll-right`, `key:ctrl+shift+k`,
/// `consumer:volume-up`, `dpi:loop`, `light:brightness-up`,
/// `autofire:left-click,37ms,5`, `system:copy`, `disabled`, `raw:07:01 01 25 00 05`.
impl fmt::Display for ButtonAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ButtonAction::Default => f.write_str("default"),
            ButtonAction::Mouse(m) => f.write_str(m.name()),
            ButtonAction::Keyboard(k) => write!(f, "key:{k}"),
            ButtonAction::Consumer(c) => write!(f, "consumer:{}", c.name()),
            ButtonAction::Dpi(d) => write!(f, "dpi:{}", d.name()),
            ButtonAction::Lighting(l) => write!(f, "light:{}", l.name()),
            ButtonAction::AutoFire(a) => write!(
                f,
                "autofire:{},{}ms,{}",
                a.button.name(),
                a.interval_ms,
                a.times
            ),
            ButtonAction::System(s) => write!(f, "system:{}", s.name()),
            ButtonAction::Disabled => f.write_str("disabled"),
            ButtonAction::Raw { type_byte, payload } => {
                write!(f, "raw:{type_byte:02x}:{}", crate::protocol::hex(payload))
            }
        }
    }
}

impl FromStr for ButtonAction {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        let lower = s.to_ascii_lowercase();
        if let Some(m) = MouseFunction::ALL.iter().find(|m| m.name() == lower) {
            return Ok(ButtonAction::Mouse(*m));
        }
        match lower.as_str() {
            "default" | "reset" => return Ok(ButtonAction::Default),
            "disabled" | "disable" | "none" => return Ok(ButtonAction::Disabled),
            "left" => return Ok(ButtonAction::Mouse(MouseFunction::Left)),
            "right" => return Ok(ButtonAction::Mouse(MouseFunction::Right)),
            "middle" => return Ok(ButtonAction::Mouse(MouseFunction::Middle)),
            _ => {}
        }
        let (kind, rest) = s
            .split_once(':')
            .ok_or_else(|| format!("unknown button action '{s}'"))?;
        let rest = rest.trim();
        let lrest = rest.to_ascii_lowercase();
        match kind.trim().to_ascii_lowercase().as_str() {
            "key" | "keyboard" | "kbd" => Ok(ButtonAction::Keyboard(KeyCombo::parse(rest)?)),
            "consumer" | "media" => ConsumerFunction::ALL
                .iter()
                .find(|c| c.name() == lrest)
                .map(|c| ButtonAction::Consumer(*c))
                .ok_or_else(|| {
                    format!(
                        "unknown media function '{rest}' (expected one of: {})",
                        ConsumerFunction::ALL.map(ConsumerFunction::name).join(", ")
                    )
                }),
            "dpi" => DpiFunction::ALL
                .iter()
                .find(|d| d.name() == lrest || (lrest == "+" && **d == DpiFunction::Up) || (lrest == "-" && **d == DpiFunction::Down))
                .map(|d| ButtonAction::Dpi(*d))
                .ok_or_else(|| format!("unknown DPI function '{rest}' (expected loop, up, down)")),
            "light" | "lighting" | "rgb" => LightingFunction::ALL
                .iter()
                .find(|l| l.name() == lrest)
                .map(|l| ButtonAction::Lighting(*l))
                .ok_or_else(|| {
                    format!(
                        "unknown lighting function '{rest}' (expected one of: {})",
                        LightingFunction::ALL.map(LightingFunction::name).join(", ")
                    )
                }),
            "autofire" | "auto-fire" | "game" => {
                let parts: Vec<&str> = rest.split(',').map(str::trim).collect();
                if parts.len() != 3 {
                    return Err(format!(
                        "'{rest}': expected BUTTON,INTERVALms,TIMES (e.g. left-click,37ms,5)"
                    ));
                }
                let button = MouseFunction::ALL
                    .iter()
                    .copied()
                    .find(|m| m.name() == parts[0].to_ascii_lowercase() || m.name().trim_end_matches("-click") == parts[0].to_ascii_lowercase())
                    .filter(|m| matches!(m, MouseFunction::Left | MouseFunction::Right | MouseFunction::Middle | MouseFunction::Forward | MouseFunction::Backward))
                    .ok_or_else(|| format!("'{}' is not a clickable button", parts[0]))?;
                let interval_ms: u16 = parts[1]
                    .trim_end_matches("ms")
                    .trim()
                    .parse()
                    .map_err(|_| format!("'{}' is not an interval in ms", parts[1]))?;
                let times: u8 = parts[2]
                    .trim_start_matches(['x', '×'])
                    .parse()
                    .map_err(|_| format!("'{}' is not a repeat count", parts[2]))?;
                if interval_ms == 0 || times == 0 {
                    return Err("interval and times must be at least 1".into());
                }
                Ok(ButtonAction::AutoFire(AutoFire {
                    button,
                    interval_ms,
                    times,
                }))
            }
            "system" | "sys" => SystemShortcut::ALL
                .iter()
                .find(|c| c.name() == lrest)
                .map(|c| ButtonAction::System(*c))
                .ok_or_else(|| {
                    format!(
                        "unknown system shortcut '{rest}' (expected one of: {})",
                        SystemShortcut::ALL.map(SystemShortcut::name).join(", ")
                    )
                }),
            "raw" => {
                let (t, p) = rest
                    .split_once(':')
                    .ok_or_else(|| "raw:TT:HEX BYTES".to_string())?;
                let type_byte = u8::from_str_radix(t.trim(), 16).map_err(|e| e.to_string())?;
                let payload = crate::protocol::from_hex(p)?;
                Ok(ButtonAction::Raw { type_byte, payload })
            }
            other => Err(format!(
                "unknown button action kind '{other}' (expected a mouse function, key:, consumer:, dpi:, light:, autofire:, system:, disabled or default)"
            )),
        }
    }
}

impl Serialize for ButtonAction {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for ButtonAction {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        String::deserialize(d)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

// ----- macros -----------------------------------------------------------

/// How a macro repeats (*verified* bytes LC/LM, see PROTOCOL.md §7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LoopMode {
    /// Play `n` times (`LC = n, LM = 00`).
    Times(u8),
    /// Repeat while the button is held (`LC = 01, LM = 20`).
    UntilRelease,
    /// Repeat until any button is clicked (`LC = 01, LM = 40`).
    UntilAnyClick,
}

impl LoopMode {
    fn bytes(self) -> [u8; 2] {
        match self {
            LoopMode::Times(n) => [n, 0x00],
            LoopMode::UntilRelease => [0x01, 0x20],
            LoopMode::UntilAnyClick => [0x01, 0x40],
        }
    }

    fn from_bytes(lc: u8, lm: u8) -> Option<LoopMode> {
        match lm {
            0x00 => Some(LoopMode::Times(lc)),
            0x20 => Some(LoopMode::UntilRelease),
            0x40 => Some(LoopMode::UntilAnyClick),
            _ => None,
        }
    }
}

impl fmt::Display for LoopMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoopMode::Times(n) => write!(f, "times:{n}"),
            LoopMode::UntilRelease => f.write_str("until-release"),
            LoopMode::UntilAnyClick => f.write_str("until-any-click"),
        }
    }
}

impl FromStr for LoopMode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let l = s.trim().to_ascii_lowercase();
        match l.as_str() {
            "until-release" | "until-stop-pressing" | "hold" => Ok(LoopMode::UntilRelease),
            "until-any-click" | "toggle" => Ok(LoopMode::UntilAnyClick),
            _ => {
                let n = l
                    .strip_prefix("times:")
                    .unwrap_or(&l)
                    .parse::<u8>()
                    .map_err(|_| {
                        format!("'{s}' is not times:N, until-release or until-any-click")
                    })?;
                if n == 0 {
                    return Err("loop count must be at least 1".into());
                }
                Ok(LoopMode::Times(n))
            }
        }
    }
}

impl Serialize for LoopMode {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for LoopMode {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        String::deserialize(d)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

/// One macro step (4 bytes on the wire, *verified*).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MacroEvent {
    /// `81 <usage> 00 00`
    KeyDown(u8),
    /// `01 <usage> 00 00`
    KeyUp(u8),
    /// `0f <ms LE24>`
    Delay(u32),
}

impl MacroEvent {
    pub const MAX_DELAY_MS: u32 = 0x00ff_ffff;

    pub fn bytes(self) -> [u8; 4] {
        match self {
            MacroEvent::KeyDown(u) => [0x81, u, 0, 0],
            MacroEvent::KeyUp(u) => [0x01, u, 0, 0],
            MacroEvent::Delay(ms) => {
                let [a, b, c, _] = ms.min(Self::MAX_DELAY_MS).to_le_bytes();
                [0x0f, a, b, c]
            }
        }
    }

    pub fn from_bytes(b: [u8; 4]) -> Option<MacroEvent> {
        match b {
            [0x81, u, 0, 0] => Some(MacroEvent::KeyDown(u)),
            [0x01, u, 0, 0] => Some(MacroEvent::KeyUp(u)),
            [0x0f, a, bb, c] => Some(MacroEvent::Delay(u32::from_le_bytes([a, bb, c, 0]))),
            _ => None,
        }
    }
}

/// `a+` (down), `a-` (up), `47ms` (delay).
impl fmt::Display for MacroEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MacroEvent::KeyDown(u) => write!(f, "{}+", key_name(*u)),
            MacroEvent::KeyUp(u) => write!(f, "{}-", key_name(*u)),
            MacroEvent::Delay(ms) => write!(f, "{ms}ms"),
        }
    }
}

impl FromStr for MacroEvent {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let t = s.trim();
        if let Some(ms) = t.strip_suffix("ms") {
            let ms: u32 = ms
                .trim()
                .parse()
                .map_err(|_| format!("'{s}' is not a delay like 47ms"))?;
            if ms > MacroEvent::MAX_DELAY_MS {
                return Err(format!(
                    "delay {ms} ms is too long (max {})",
                    MacroEvent::MAX_DELAY_MS
                ));
            }
            return Ok(MacroEvent::Delay(ms));
        }
        if let Some(k) = t.strip_suffix('+') {
            return key_usage(k)
                .map(MacroEvent::KeyDown)
                .ok_or_else(|| format!("'{k}' is not a known key name"));
        }
        if let Some(k) = t.strip_suffix('-') {
            return key_usage(k)
                .map(MacroEvent::KeyUp)
                .ok_or_else(|| format!("'{k}' is not a known key name"));
        }
        Err(format!(
            "'{s}' is not a macro step (use KEY+ for press, KEY- for release, Nms for a delay)"
        ))
    }
}

impl Serialize for MacroEvent {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for MacroEvent {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        String::deserialize(d)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

/// A macro as the vendor app sends it: one 0x54 report (events) followed by
/// one 0x53 report (name). **Experimental**: only the single-chunk layout was
/// captured, so macros are capped at [`Macro::MAX_EVENTS`] steps.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Macro {
    pub name: String,
    #[serde(rename = "loop", default = "default_loop")]
    pub loop_mode: LoopMode,
    /// Fixed delay in ms inserted by the mouse between steps ("Fixed Delay"
    /// in the app); 0 = use the recorded delays / none (*likely*: the app
    /// encodes "No Delay" and "Record Delay" identically).
    #[serde(default)]
    pub fixed_delay_ms: u16,
    pub events: Vec<MacroEvent>,
}

fn default_loop() -> LoopMode {
    LoopMode::Times(1)
}

/// Bytes before the events in a 0x54 report: `52 54 KK 00 LEN 01 LC LM DL DL N N`.
pub const MACRO_HEADER_LEN: usize = 12;

impl Macro {
    /// Steps that fit one 65-byte report after the 12-byte header.
    pub const MAX_EVENTS: usize = (crate::protocol::LONG_REPORT_LEN - MACRO_HEADER_LEN) / 4;
    /// The 0x53 report holds `52 53 KK LEN` + name.
    pub const MAX_NAME_BYTES: usize = crate::protocol::LONG_REPORT_LEN - 4;

    pub fn validate(&self) -> Result<(), String> {
        if self.events.is_empty() {
            return Err("a macro needs at least one step".into());
        }
        if self.events.len() > Self::MAX_EVENTS {
            return Err(format!(
                "macro has {} steps; only {} fit in one report (longer macros were never captured, so Tailglow refuses them)",
                self.events.len(),
                Self::MAX_EVENTS
            ));
        }
        if self.name.is_empty() {
            return Err("a macro needs a name".into());
        }
        if !self.name.is_ascii() {
            return Err("macro names must be ASCII (the app sent plain bytes)".into());
        }
        if self.name.len() > Self::MAX_NAME_BYTES {
            return Err(format!(
                "macro name longer than {} bytes",
                Self::MAX_NAME_BYTES
            ));
        }
        if let LoopMode::Times(0) = self.loop_mode {
            return Err("loop count must be at least 1".into());
        }
        Ok(())
    }

    /// Bytes 4.. of the 0x54 report (`LEN` and the header + events).
    /// `LEN = 6 + 4 × events` reproduces the captured 0x22 for 7 events
    /// (*likely*: one data point).
    pub fn events_payload(&self) -> Vec<u8> {
        let n = self.events.len();
        let [lc, lm] = self.loop_mode.bytes();
        let [dlo, dhi] = self.fixed_delay_ms.to_le_bytes();
        let [nlo, nhi] = (n as u16).to_le_bytes();
        let mut out = vec![(6 + 4 * n) as u8, 0x01, lc, lm, dlo, dhi, nlo, nhi];
        for e in &self.events {
            out.extend_from_slice(&e.bytes());
        }
        out
    }

    /// Bytes 3.. of the 0x53 report (`NAMELEN` + ASCII name).
    pub fn name_payload(&self) -> Vec<u8> {
        let mut out = vec![self.name.len() as u8];
        out.extend_from_slice(self.name.as_bytes());
        out
    }

    /// Inverse of [`Macro::events_payload`] (name must be supplied).
    pub fn parse_events_payload(p: &[u8], name: String) -> Option<Macro> {
        if p.len() < 8 || p[1] != 0x01 {
            return None;
        }
        let loop_mode = LoopMode::from_bytes(p[2], p[3])?;
        let fixed_delay_ms = u16::from_le_bytes([p[4], p[5]]);
        let n = u16::from_le_bytes([p[6], p[7]]) as usize;
        let mut events = Vec::with_capacity(n);
        for i in 0..n {
            let at = 8 + 4 * i;
            let chunk = p.get(at..at + 4)?;
            events.push(MacroEvent::from_bytes([
                chunk[0], chunk[1], chunk[2], chunk[3],
            ])?);
        }
        Some(Macro {
            name,
            loop_mode,
            fixed_delay_ms,
            events,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::hex;

    #[test]
    fn mouse_function_payloads_match_capture() {
        // 52 52 03 00 01 <payload> for each selection in session 2.
        let cases = [
            (MouseFunction::Left, "01 00 00"),
            (MouseFunction::Right, "02 00 00"),
            (MouseFunction::Middle, "04 00 00"),
            (MouseFunction::Forward, "08 00 00"),
            (MouseFunction::Backward, "10 00 00"),
            (MouseFunction::DoubleClick, "80 00 00"),
            (MouseFunction::ScrollUp, "00 02 00"),
            (MouseFunction::ScrollDown, "00 fe 00"),
            (MouseFunction::ScrollLeft, "00 00 fe"),
            (MouseFunction::ScrollRight, "00 00 02"),
        ];
        for (f, want) in cases {
            assert_eq!(hex(&f.payload()), want, "{f:?}");
            assert_eq!(MouseFunction::from_payload(&f.payload()), Some(f));
            assert_eq!(
                f.name().parse::<ButtonAction>().unwrap(),
                ButtonAction::Mouse(f)
            );
        }
    }

    #[test]
    fn action_payloads_match_capture() {
        let cases: Vec<(ButtonAction, &str)> = vec![
            (ButtonAction::Consumer(ConsumerFunction::Player), "03 83 01"),
            (
                ButtonAction::Consumer(ConsumerFunction::VolumeUp),
                "03 e9 00",
            ),
            (ButtonAction::Lighting(LightingFunction::Effect), "06 01"),
            (
                ButtonAction::Lighting(LightingFunction::BrightnessDown),
                "06 05",
            ),
            (ButtonAction::Dpi(DpiFunction::Loop), "05 01"),
            (ButtonAction::Dpi(DpiFunction::Down), "05 03"),
            (
                ButtonAction::System(SystemShortcut::BrightnessUp),
                "08 0c 6f 00",
            ),
            (
                ButtonAction::System(SystemShortcut::Calculator),
                "08 0c 92 01",
            ),
            (ButtonAction::System(SystemShortcut::Website), "08 0c 23 02"),
            (ButtonAction::System(SystemShortcut::Refresh), "08 07 00 3e"),
            (
                ButtonAction::System(SystemShortcut::SwitchApp),
                "08 07 04 2b 00 00 01",
            ),
            (ButtonAction::System(SystemShortcut::Paste), "08 07 01 19"),
            (ButtonAction::Disabled, "09"),
            (ButtonAction::Default, "00"),
            (
                ButtonAction::Keyboard(KeyCombo::parse("ctrl+shift+k").unwrap()),
                "02 03 0e",
            ),
            (
                ButtonAction::Keyboard(KeyCombo::parse("f7").unwrap()),
                "02 00 40",
            ),
            (
                ButtonAction::AutoFire(AutoFire {
                    button: MouseFunction::Left,
                    interval_ms: 37,
                    times: 5,
                }),
                "07 01 01 25 00 05",
            ),
        ];
        for (a, want) in cases {
            let mut bytes = vec![a.type_byte()];
            bytes.extend(a.payload());
            assert_eq!(hex(&bytes), want, "{a}");
            // Round trip through the reply parser and the text form.
            assert_eq!(ButtonAction::parse_payload(a.type_byte(), &a.payload()), a);
            assert_eq!(a.to_string().parse::<ButtonAction>().unwrap(), a, "{a}");
        }
    }

    #[test]
    fn text_forms() {
        assert_eq!(
            "autofire:left,37ms,5".parse::<ButtonAction>().unwrap(),
            ButtonAction::AutoFire(AutoFire {
                button: MouseFunction::Left,
                interval_ms: 37,
                times: 5
            })
        );
        assert_eq!(
            "key:Ctrl+Shift+K"
                .parse::<ButtonAction>()
                .unwrap()
                .to_string(),
            "key:ctrl+shift+k"
        );
        assert_eq!(
            "dpi:+".parse::<ButtonAction>().unwrap(),
            ButtonAction::Dpi(DpiFunction::Up)
        );
        assert!("key:ctrl+shift".parse::<ButtonAction>().is_err());
        assert!("bogus".parse::<ButtonAction>().is_err());
        assert!("autofire:left,0ms,5".parse::<ButtonAction>().is_err());
        let raw = ButtonAction::parse_payload(0x0b, &[1, 2, 0, 0]);
        assert_eq!(
            raw,
            ButtonAction::Raw {
                type_byte: 0x0b,
                payload: vec![1, 2]
            }
        );
        assert_eq!(raw.to_string(), "raw:0b:01 02");
        assert_eq!(raw.to_string().parse::<ButtonAction>().unwrap(), raw);
    }

    #[test]
    fn keys() {
        assert_eq!(Key::Key4.index(), 3);
        assert_eq!(Key::ScrollDown.index(), 8);
        assert_eq!("key4".parse::<Key>().unwrap(), Key::Key4);
        assert_eq!("4".parse::<Key>().unwrap(), Key::Key4);
        assert_eq!("scroll-up".parse::<Key>().unwrap(), Key::ScrollUp);
        assert_eq!("Scr Dn".parse::<Key>().unwrap(), Key::ScrollDown);
        assert!("key9".parse::<Key>().is_err());
        assert!(Key::Key1.factory_action().is_left_click());
    }

    #[test]
    fn macro_matches_capture() {
        // 52 54 03 00 22 01 03 00 19 00 07 00 81 04 00 00 0f 2f 00 00 01 04 00 00
        // 0f b8 0b 00 81 05 00 00 0f 20 00 00 01 05 00 00   (Loop Times 3, Fixed Delay 25)
        let m = Macro {
            name: "Custom 1".into(),
            loop_mode: LoopMode::Times(3),
            fixed_delay_ms: 25,
            events: vec![
                MacroEvent::KeyDown(0x04),
                MacroEvent::Delay(47),
                MacroEvent::KeyUp(0x04),
                MacroEvent::Delay(3000),
                MacroEvent::KeyDown(0x05),
                MacroEvent::Delay(32),
                MacroEvent::KeyUp(0x05),
            ],
        };
        m.validate().unwrap();
        assert_eq!(
            hex(&m.events_payload()),
            "22 01 03 00 19 00 07 00 81 04 00 00 0f 2f 00 00 01 04 00 00 0f b8 0b 00 81 05 00 00 0f 20 00 00 01 05 00 00"
        );
        assert_eq!(hex(&m.name_payload()), "08 43 75 73 74 6f 6d 20 31");
        let until_release = Macro {
            loop_mode: LoopMode::UntilRelease,
            fixed_delay_ms: 0,
            ..m.clone()
        };
        assert_eq!(
            hex(&until_release.events_payload()[..8]),
            "22 01 01 20 00 00 07 00"
        );
        let any_click = Macro {
            loop_mode: LoopMode::UntilAnyClick,
            fixed_delay_ms: 0,
            ..m.clone()
        };
        assert_eq!(
            hex(&any_click.events_payload()[..8]),
            "22 01 01 40 00 00 07 00"
        );
        assert_eq!(
            Macro::parse_events_payload(&m.events_payload(), "Custom 1".into()).unwrap(),
            m
        );
        let steps: Vec<String> = m.events.iter().map(|e| e.to_string()).collect();
        assert_eq!(steps, ["a+", "47ms", "a-", "3000ms", "b+", "32ms", "b-"]);
        for (s, e) in steps.iter().zip(&m.events) {
            assert_eq!(s.parse::<MacroEvent>().unwrap(), *e);
        }
    }

    #[test]
    fn macro_limits() {
        assert_eq!(Macro::MAX_EVENTS, 13);
        let too_long = Macro {
            name: "x".into(),
            loop_mode: LoopMode::Times(1),
            fixed_delay_ms: 0,
            events: vec![MacroEvent::KeyDown(4); 14],
        };
        assert!(too_long.validate().unwrap_err().contains("13"));
        let ok = Macro {
            events: vec![MacroEvent::KeyDown(4); 13],
            ..too_long.clone()
        };
        ok.validate().unwrap();
        assert!(ok.events_payload().len() + 4 <= crate::protocol::LONG_REPORT_LEN);
        assert_eq!(ok.events_payload().len() + 4, 64);
        assert!(Macro {
            events: vec![],
            ..too_long.clone()
        }
        .validate()
        .is_err());
        assert!(Macro {
            name: "é".into(),
            ..ok.clone()
        }
        .validate()
        .is_err());
        assert_eq!(
            "until-release".parse::<LoopMode>().unwrap(),
            LoopMode::UntilRelease
        );
        assert_eq!("times:3".parse::<LoopMode>().unwrap(), LoopMode::Times(3));
        assert_eq!("5".parse::<LoopMode>().unwrap(), LoopMode::Times(5));
        assert!("times:0".parse::<LoopMode>().is_err());
    }
}
