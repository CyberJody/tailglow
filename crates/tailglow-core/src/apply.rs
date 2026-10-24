// SPDX-License-Identifier: GPL-3.0-or-later
//! Applying a settings file to the mouse: write only what differs, skip
//! macros the mouse already holds, then read everything back and report what
//! did not land. Shared by `tailglow import`, `tailglow watch`, the GUI's
//! Open… and the tray's profile switching, so all four behave the same.

use std::collections::BTreeMap;

use crate::buttons::Key;
use crate::mouse::{Error, Mouse, Snapshot};
use crate::settings::{macro_fingerprint, macro_may_remain, Settings};
use crate::transport::Transport;

/// How to apply.
#[derive(Debug, Clone, Default)]
pub struct ApplyOptions {
    /// Write every section present, even when the mouse already matches,
    /// and re-send every macro.
    pub force: bool,
    /// Fingerprints of macros sent earlier (`state.toml` `[sent_macros]`).
    pub sent_macros: BTreeMap<Key, String>,
}

/// What an apply did.
#[derive(Debug, Clone)]
pub struct ApplyOutcome {
    /// Logical changes written (one lighting state, one DPI table, …).
    pub changes: usize,
    /// HID reports sent.
    pub reports: usize,
    /// Macro keys skipped because the same macro was sent before and the
    /// read-back does not show it was replaced.
    pub macros_skipped: Vec<Key>,
    /// Read-back comparison of every readable section in the file against
    /// the mouse after the writes; empty when everything landed.
    pub mismatches: Vec<String>,
    /// The mouse after the writes.
    pub snapshot: Snapshot,
}

impl<T: Transport> Mouse<T> {
    /// Applies `settings` (only what differs unless `opts.force`), then
    /// takes a fresh snapshot and compares it with the file.
    pub fn apply_settings(
        &mut self,
        settings: &Settings,
        opts: &ApplyOptions,
    ) -> Result<ApplyOutcome, Error> {
        let before = self.snapshot()?;
        let current = Settings::from_snapshot(&before);
        let todo = if opts.force {
            settings.clone()
        } else {
            settings.diff_against(&current)
        };
        let mut changes = 0;
        let mut reports = 0;
        if let Some(l) = todo.lighting {
            self.apply_lighting(&l)?;
            changes += 1;
            reports += l.commands().len();
        }
        if let Some(d) = todo.dpi {
            self.apply_dpi(&d)?;
            changes += 1;
            reports += 1;
        }
        if let Some(p) = todo.pointer {
            let cur = current.pointer.unwrap_or_default();
            if opts.force || cur.report_rate != p.report_rate {
                self.apply_report_rate(p.report_rate)?;
                changes += 1;
                reports += 1;
            }
            if opts.force || cur.sensor() != p.sensor() {
                self.apply_sensor(&p.sensor())?;
                changes += 1;
                reports += 1;
            }
            if opts.force || cur.debounce_ms != p.debounce_ms {
                self.apply_debounce(p.debounce_ms)?;
                changes += 1;
                reports += 1;
            }
        }
        for (key, action) in &todo.buttons {
            let map = self.buttons()?;
            self.apply_button(*key, action, Some(&map))?;
            changes += 1;
            reports += 1;
        }
        let mut macros_skipped = Vec::new();
        for (key, m) in &todo.macros {
            let unchanged = opts.sent_macros.get(key) == Some(&macro_fingerprint(*key, m));
            // A button written to this key above, or a mapping the mouse
            // reports for it, means the macro is no longer there.
            let replaced =
                todo.buttons.contains_key(key) || !macro_may_remain(&before.buttons, *key);
            if !opts.force && unchanged && !replaced {
                macros_skipped.push(*key);
                continue;
            }
            let map = self.buttons()?;
            self.apply_macro(*key, m, Some(&map))?;
            changes += 1;
            reports += 2;
        }
        let snapshot = self.snapshot()?;
        let mismatches = settings.mismatches(&Settings::from_snapshot(&snapshot));
        Ok(ApplyOutcome {
            changes,
            reports,
            macros_skipped,
            mismatches,
            snapshot,
        })
    }
}
