// SPDX-License-Identifier: GPL-3.0-or-later
//! `Mouse::apply_settings` on the stateful fake: diff, macro fingerprints,
//! read-back compare, and the debounce opt-in.

use tailglow_core::buttons::{LoopMode, MacroEvent, MouseFunction};
use tailglow_core::{
    ApplyOptions, ButtonAction, Debounce, Effect, Error, Key, Lighting, Macro, Mouse, Rgb, Settings,
};
use tailglow_hid::FakeMouse;

fn mouse() -> Mouse<FakeMouse> {
    let mut m = Mouse::new(FakeMouse::session2());
    m.read_info().unwrap();
    m.transport_mut().clear_writes();
    m
}

fn writes(m: &Mouse<FakeMouse>) -> usize {
    m.transport().lighting_writes().len()
}

fn macro6() -> Macro {
    Macro {
        name: "Custom 1".into(),
        loop_mode: LoopMode::Times(3),
        fixed_delay_ms: 25,
        events: vec![
            MacroEvent::KeyDown(4),
            MacroEvent::Delay(47),
            MacroEvent::KeyUp(4),
        ],
    }
}

#[test]
fn unchanged_macros_are_not_resent_and_state_tracks_them() {
    let mut m = mouse();
    let mut file = Settings::with_lighting(Lighting::static_color(Rgb::RED));
    file.macros.insert(Key::Key6, macro6());

    // First apply: lighting burst (4) + macro (2).
    let mut state = Settings::default();
    let out = m
        .apply_settings(
            &file,
            &ApplyOptions {
                sent_macros: state.sent_macros.clone(),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!((out.changes, out.reports), (2, 6));
    assert!(out.macros_skipped.is_empty());
    assert!(out.mismatches.is_empty(), "{:?}", out.mismatches);
    assert_eq!(writes(&m), 6);
    state.record_applied(&file);
    let state = Settings::from_snapshot_with_macros(&out.snapshot, &state);
    assert!(state.sent_macros.contains_key(&Key::Key6));

    // Same file again (e.g. the next plug): nothing differs, macro skipped.
    m.transport_mut().clear_writes();
    let out = m
        .apply_settings(
            &file,
            &ApplyOptions {
                sent_macros: state.sent_macros.clone(),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(out.changes, 0);
    assert_eq!(out.macros_skipped, vec![Key::Key6]);
    assert_eq!(writes(&m), 0, "{:?}", m.transport().lighting_writes());

    // A different macro on the same key is sent.
    let mut file2 = file.clone();
    file2.macros.insert(
        Key::Key6,
        Macro {
            name: "Custom 2".into(),
            ..macro6()
        },
    );
    let out = m
        .apply_settings(
            &file2,
            &ApplyOptions {
                sent_macros: state.sent_macros.clone(),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(out.changes, 1);
    assert_eq!(writes(&m), 2);

    // --force re-sends everything, macros included.
    m.transport_mut().clear_writes();
    let out = m
        .apply_settings(
            &file,
            &ApplyOptions {
                force: true,
                sent_macros: state.sent_macros.clone(),
            },
        )
        .unwrap();
    assert_eq!(out.reports, 6);
}

#[test]
fn a_macro_replaced_on_the_mouse_is_resent() {
    let mut m = mouse();
    let mut file = Settings::default();
    file.macros.insert(Key::Key6, macro6());
    let mut state = Settings::default();
    m.apply_settings(&file, &ApplyOptions::default()).unwrap();
    state.record_applied(&file);
    // Someone (the vendor app, another tailglow) mapped key6 to Middle-Click
    // meanwhile, without updating this state file.
    let map = m.buttons().unwrap();
    m.apply_button(
        Key::Key6,
        &ButtonAction::Mouse(MouseFunction::Middle),
        Some(&map),
    )
    .unwrap();
    m.transport_mut().clear_writes();
    let out = m
        .apply_settings(
            &file,
            &ApplyOptions {
                sent_macros: state.sent_macros.clone(),
                ..Default::default()
            },
        )
        .unwrap();
    assert!(out.macros_skipped.is_empty());
    assert_eq!(writes(&m), 2);
}

#[test]
fn read_back_mismatches_are_reported() {
    let mut m = mouse();
    // The fake stores every write, so the live compare comes back clean;
    // the mismatch path is checked on a doctored read-back (a mouse that
    // kept green instead of the blue we sent).
    let file = Settings::with_lighting(Lighting::static_color(Rgb::BLUE));
    let out = m.apply_settings(&file, &ApplyOptions::default()).unwrap();
    assert!(out.mismatches.is_empty());
    let mut after = Settings::from_snapshot(&out.snapshot);
    after.lighting = Some(Lighting {
        effect: Effect::Static,
        color: Rgb::GREEN,
        ..after.lighting.unwrap()
    });
    let mm = file.mismatches(&after);
    assert_eq!(mm.len(), 1);
    assert!(
        mm[0].contains("#0000ff") && mm[0].contains("#00ff00"),
        "{mm:?}"
    );
}

#[test]
fn debounce_outside_the_captured_range_needs_the_opt_in() {
    let mut m = mouse();
    for ms in 8..=11 {
        m.apply_debounce(Debounce::new(ms).unwrap()).unwrap();
    }
    let err = m.apply_debounce(Debounce::new(12).unwrap()).unwrap_err();
    assert!(matches!(err, Error::UntestedValue(_)), "{err}");
    let msg = err.to_string();
    assert!(
        msg.contains("8-11") && msg.contains("--allow-untested"),
        "{msg}"
    );
    assert!(m.apply_debounce(Debounce::new(1).unwrap()).is_err());
    let before = writes(&m);
    m.allow_untested(true);
    m.apply_debounce(Debounce::new(12).unwrap()).unwrap();
    assert_eq!(writes(&m), before + 1);
    assert_eq!(m.pointer().unwrap().debounce.ms(), 12);
}
