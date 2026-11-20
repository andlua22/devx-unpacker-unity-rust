//! Native GPCKeyCodeItem primary and secondary key controls.

use super::*;
use crate::config::PcKeyCodeEntry;

pub(super) struct NativeKeyRow {
    cfg: PcKeyCodeEntry,
    name: Entity,
    labels: [Entity; 2],
    checks: [Option<Entity>; 2],
}

pub(super) fn bind_native_keys(
    bound: &BoundUiPanel,
    doc: &UiPanelDoc,
    configs: &GameConfigs,
    bindings: &mut NativeLobbyBindings,
) -> Vec<NativeKeyRow> {
    const PREFIX: &str = "GameObject/GameObject/m_PCKeyCode/items/m_content/fl_";
    let mut rows = Vec::new();
    for binding in doc
        .script_bindings
        .iter()
        .filter(|binding| binding.field_name == "GameObjects" && binding.slot_index == Some(0))
    {
        let Some(path) = binding
            .owner_path
            .as_deref()
            .and_then(|path| path.strip_prefix(PREFIX))
        else {
            continue;
        };
        let Some((group, index)) = path.split_once('/') else {
            continue;
        };
        let (Ok(group), Ok(index)) = (group.parse::<i32>(), index.parse::<usize>()) else {
            continue;
        };
        let Some(cfg) = configs
            .pc_key_codes
            .iter()
            .filter(|cfg| cfg.key_type == group)
            .nth(index.saturating_sub(1))
        else {
            continue;
        };
        let Some(controller) = binding.owner_component_ref else {
            continue;
        };
        let targets: Option<Vec<_>> = (0..5)
            .map(|slot| bound.controller_slot(controller, "GameObjects", slot))
            .collect();
        let Some(targets) = targets else {
            continue;
        };
        for slot in 0..2 {
            bindings.bind(
                bound,
                targets[slot],
                LobbyAction::BeginNativeKeyEdit(cfg.id, slot),
            );
        }
        rows.push(NativeKeyRow {
            cfg: cfg.clone(),
            name: targets[2],
            labels: [targets[3], targets[4]],
            checks: [
                child_entity(bound, targets[0], "Background/1/Checkmark"),
                child_entity(bound, targets[1], "Background/1/Checkmark"),
            ],
        });
    }
    if let Some(controller) = controller_ref(doc, -167_170_049_505_455_824)
        && let Some(reset) = bound.controller_slot(controller, "GameObjects", 0)
    {
        bindings.bind(bound, reset, LobbyAction::ResetNativeKeys);
    }
    rows
}

fn chord_for<'a>(
    profile: &'a PlayerProfileSave,
    cfg: &'a PcKeyCodeEntry,
    slot: usize,
) -> &'a [i32] {
    profile
        .native_keybinds
        .get(&cfg.id)
        .and_then(|chords| chords.get(slot))
        .map(Vec::as_slice)
        .unwrap_or(if slot == 0 { &cfg.keycode } else { &[] })
}

fn key_label(code: i32) -> String {
    match code {
        0 => String::new(),
        8 => "Backspace".into(),
        9 => "Tab".into(),
        13 => "Enter".into(),
        27 => "Esc".into(),
        32 => "Space".into(),
        127 => "Delete".into(),
        273 => "Up".into(),
        274 => "Down".into(),
        275 => "Right".into(),
        276 => "Left".into(),
        277 => "Insert".into(),
        278 => "Home".into(),
        279 => "End".into(),
        280 => "Page Up".into(),
        281 => "Page Down".into(),
        282..=296 => format!("F{}", code - 281),
        303 | 304 => "Shift".into(),
        305 | 306 => "Ctrl".into(),
        307 | 308 => "Alt".into(),
        323 => "Mouse 1".into(),
        324 => "Mouse 2".into(),
        325 => "Mouse 3".into(),
        33..=126 => char::from_u32(code as u32)
            .unwrap()
            .to_ascii_uppercase()
            .to_string(),
        _ => code.to_string(),
    }
}

pub(super) fn sync_native_keys(
    bound: &BoundUiPanel,
    rows: &[NativeKeyRow],
    profile: &PlayerProfileSave,
    ui_state: &MainLobbyUiState,
    l10n: &LocalizationDb,
    texts: &mut Query<&mut Text>,
    visibility: &mut Query<&mut Visibility>,
    nodes: &mut Query<&mut Node>,
) {
    for row in rows {
        bound.set_entity_text(row.name, l10n.tr(&row.cfg.func_name), texts);
        for slot in 0..2 {
            let editing = ui_state.editing_native_key == Some((row.cfg.id, slot));
            let label = if editing {
                "...".into()
            } else {
                chord_for(profile, &row.cfg, slot)
                    .iter()
                    .map(|&code| key_label(code))
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<_>>()
                    .join("+")
            };
            bound.set_entity_text(row.labels[slot], &label, texts);
            if let Some(check) = row.checks[slot] {
                BoundUiPanel::set_entity_active(check, editing, visibility, nodes);
            }
        }
    }
}

fn unity_key_code(key: KeyCode) -> Option<i32> {
    let name = format!("{key:?}");
    if let Some(letter) = name.strip_prefix("Key").filter(|name| name.len() == 1) {
        return Some(i32::from(letter.as_bytes()[0].to_ascii_lowercase()));
    }
    if let Some(digit) = name.strip_prefix("Digit").filter(|name| name.len() == 1) {
        return Some(i32::from(digit.as_bytes()[0]));
    }
    if let Some(number) = name
        .strip_prefix('F')
        .and_then(|number| number.parse::<i32>().ok())
        .filter(|&number| (1..=15).contains(&number))
    {
        return Some(281 + number);
    }
    Some(match key {
        KeyCode::Backspace => 8,
        KeyCode::Tab => 9,
        KeyCode::Enter => 13,
        KeyCode::Space => 32,
        KeyCode::Quote => 39,
        KeyCode::Comma => 44,
        KeyCode::Minus => 45,
        KeyCode::Period => 46,
        KeyCode::Slash => 47,
        KeyCode::Semicolon => 59,
        KeyCode::Equal => 61,
        KeyCode::BracketLeft => 91,
        KeyCode::Backslash => 92,
        KeyCode::BracketRight => 93,
        KeyCode::Backquote => 96,
        KeyCode::Delete => 127,
        KeyCode::ArrowUp => 273,
        KeyCode::ArrowDown => 274,
        KeyCode::ArrowRight => 275,
        KeyCode::ArrowLeft => 276,
        KeyCode::Insert => 277,
        KeyCode::Home => 278,
        KeyCode::End => 279,
        KeyCode::PageUp => 280,
        KeyCode::PageDown => 281,
        KeyCode::ShiftRight => 303,
        KeyCode::ShiftLeft => 304,
        KeyCode::ControlRight => 305,
        KeyCode::ControlLeft => 306,
        KeyCode::AltRight => 307,
        KeyCode::AltLeft => 308,
        _ => return None,
    })
}

pub(in crate::ui::main_lobby) fn capture_native_keys(
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    configs: Res<GameConfigs>,
    mut profile: ResMut<PlayerProfileSave>,
    storage: Res<PlayerProfileStorage>,
    mut ui_state: ResMut<MainLobbyUiState>,
) {
    ui_state.native_key_capture_frame = false;
    let Some((id, slot)) = ui_state.editing_native_key else {
        return;
    };
    if ui_state.active_tab != LobbyTab::Settings || ui_state.settings_subtab != 7 {
        ui_state.editing_native_key = None;
        return;
    }
    if keys.just_pressed(KeyCode::Escape) {
        ui_state.editing_native_key = None;
        ui_state.native_key_capture_frame = true;
        return;
    }
    let terminal = keys
        .get_just_pressed()
        .copied()
        .filter_map(unity_key_code)
        .find(|&code| !(303..=308).contains(&code))
        .or_else(|| {
            [
                (MouseButton::Left, 323),
                (MouseButton::Right, 324),
                (MouseButton::Middle, 325),
            ]
            .into_iter()
            .find_map(|(button, code)| mouse.just_pressed(button).then_some(code))
        });
    let Some(terminal) = terminal else {
        return;
    };
    let mut chord = Vec::new();
    for (left, right, code) in [
        (KeyCode::ControlLeft, KeyCode::ControlRight, 306),
        (KeyCode::AltLeft, KeyCode::AltRight, 308),
        (KeyCode::ShiftLeft, KeyCode::ShiftRight, 304),
    ] {
        if keys.pressed(left) || keys.pressed(right) {
            chord.push(code);
        }
    }
    chord.push(terminal);
    let primary = configs
        .pc_key_codes
        .iter()
        .find(|cfg| cfg.id == id)
        .map(|cfg| cfg.keycode.clone())
        .unwrap_or_default();
    let chords = profile
        .native_keybinds
        .entry(id)
        .or_insert_with(|| vec![primary, Vec::new()]);
    chords.resize(2, Vec::new());
    chords[slot.min(1)] = chord;
    ui_state.editing_native_key = None;
    ui_state.native_key_capture_frame = true;
    if let Err(error) = profile.save_to_disk(&storage.0) {
        ui_state.status_toast = format!("Settings could not be saved: {error}");
        ui_state.dirty = true;
    }
}
