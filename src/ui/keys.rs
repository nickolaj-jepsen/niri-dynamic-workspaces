//! Layout-independent key resolution shared by the overlay views.

use gtk4::prelude::*;
use gtk4::EventControllerKey;

/// Modifiers that turn a workspace key into something else. Super is left
/// out so a Mod still held from the launch bind doesn't block input.
const ACTION_MODS: gdk4::ModifierType = gdk4::ModifierType::from_bits_retain(
    gdk4::ModifierType::CONTROL_MASK.bits()
        | gdk4::ModifierType::SHIFT_MASK.bits()
        | gdk4::ModifierType::ALT_MASK.bits(),
);

/// (layout group, shift level, keysym) of one keymap entry.
type KeymapEntry = (u32, u32, gdk4::Key);

/// The key event `ctrl` is emitting a signal for; `None` outside a handler.
pub(super) fn current_key_event(ctrl: &EventControllerKey) -> Option<gdk4::KeyEvent> {
    ctrl.current_event()?.downcast::<gdk4::KeyEvent>().ok()
}

/// The workspace key a press selects, with the modifiers the user added on
/// purpose (Ctrl, Alt, or a Shift the layout did not consume).
///
/// The typed character wins. A key that types none (AZERTY's unshifted
/// digits, a letter of a non-Latin group) selects the first workspace
/// character on the same physical key: active group first, lowest level first.
pub(super) fn workspace_key_press(event: &gdk4::KeyEvent) -> Option<(char, gdk4::ModifierType)> {
    let ch = workspace_char(event.keyval())
        .or_else(|| keymap_workspace_char(event.layout(), &keycode_entries(event)))?;
    Some((
        ch,
        deliberate_mods(event.modifier_state(), event.consumed_modifiers()),
    ))
}

/// Every keysym the keymap puts on the event's physical key.
fn keycode_entries(event: &gdk4::KeyEvent) -> Vec<KeymapEntry> {
    event
        .display()
        .and_then(|d| d.map_keycode(event.keycode()))
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(k, keyval)| {
            Some((
                u32::try_from(k.group()).ok()?,
                u32::try_from(k.level()).ok()?,
                keyval,
            ))
        })
        .collect()
}

fn workspace_char(keyval: gdk4::Key) -> Option<char> {
    keyval
        .to_unicode()
        .map(|c| c.to_ascii_lowercase())
        .filter(|&c| crate::config::is_workspace_char(c))
}

fn keymap_workspace_char(group: u32, entries: &[KeymapEntry]) -> Option<char> {
    entries
        .iter()
        .filter_map(|&(g, level, keyval)| Some((g, level, workspace_char(keyval)?)))
        .min_by_key(|&(g, level, _)| (g != group, g, level))
        .map(|(_, _, c)| c)
}

fn deliberate_mods(state: gdk4::ModifierType, consumed: gdk4::ModifierType) -> gdk4::ModifierType {
    state & !consumed & ACTION_MODS
}

#[cfg(test)]
mod tests {
    use super::*;
    use gdk4::{Key, ModifierType};

    #[test]
    fn workspace_char_folds_case_and_rejects_symbols() {
        assert_eq!(workspace_char(Key::A), Some('a'));
        assert_eq!(workspace_char(Key::_1), Some('1'));
        assert_eq!(workspace_char(Key::ampersand), None);
        assert_eq!(workspace_char(Key::Cyrillic_ef), None);
    }

    #[test]
    fn keymap_char_finds_azerty_digit_on_shift_level() {
        let fr = [(0, 0, Key::ampersand), (0, 1, Key::_1)];
        assert_eq!(keymap_workspace_char(0, &fr), Some('1'));
    }

    #[test]
    fn keymap_char_falls_back_to_latin_group() {
        let us_ru = [(0, 0, Key::a), (0, 1, Key::A), (1, 0, Key::Cyrillic_ef)];
        assert_eq!(keymap_workspace_char(1, &us_ru), Some('a'));
        let ru_us = [(0, 0, Key::Cyrillic_ef), (1, 0, Key::a), (1, 1, Key::A)];
        assert_eq!(keymap_workspace_char(0, &ru_us), Some('a'));
    }

    #[test]
    fn keymap_char_prefers_active_group() {
        let e = [(0, 0, Key::x), (1, 0, Key::comma), (1, 1, Key::y)];
        assert_eq!(keymap_workspace_char(1, &e), Some('y'));
        assert_eq!(keymap_workspace_char(0, &e), Some('x'));
    }

    #[test]
    fn keymap_char_none_without_workspace_char() {
        assert_eq!(keymap_workspace_char(0, &[(0, 0, Key::Escape)]), None);
        assert_eq!(keymap_workspace_char(0, &[]), None);
    }

    #[test]
    fn deliberate_mods_ignore_consumed_shift_super_and_lock() {
        let shift = ModifierType::SHIFT_MASK;
        let ctrl = ModifierType::CONTROL_MASK;
        assert!(deliberate_mods(shift, shift).is_empty());
        assert_eq!(deliberate_mods(ctrl | shift, shift), ctrl);
        assert!(deliberate_mods(
            ModifierType::SUPER_MASK | ModifierType::LOCK_MASK,
            ModifierType::empty()
        )
        .is_empty());
        assert_eq!(
            deliberate_mods(ModifierType::ALT_MASK, ModifierType::empty()),
            ModifierType::ALT_MASK
        );
    }
}
