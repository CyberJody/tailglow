// SPDX-License-Identifier: GPL-3.0-or-later
//! Where Tailglow keeps its files.
//!
//! Everything lives in one config directory: `$XDG_CONFIG_HOME/tailglow`,
//! else `~/.config/tailglow`, else `%APPDATA%\tailglow`. It holds
//! `state.toml` (shared by the CLI, GUI, tray and `tailglow watch`),
//! `gui.toml` and the tray's `apps.toml`.

use std::path::PathBuf;

/// Directory name under the config base.
pub const APP_DIR: &str = "tailglow";

/// `$XDG_CONFIG_HOME` (absolute only), `~/.config` or `%APPDATA%`.
pub fn config_base() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .or_else(|| std::env::var_os("APPDATA").map(PathBuf::from))
}

/// `<base>/tailglow`. `None` when no config base can be found.
pub fn config_dir() -> Option<PathBuf> {
    Some(config_base()?.join(APP_DIR))
}
