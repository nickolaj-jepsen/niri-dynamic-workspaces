mod helpers;
mod keyboard_view;
mod metrics;
mod mode;
mod overlay;
mod template_picker;
mod types;
mod variable_input;

pub use mode::Mode;
pub use overlay::{AppOverlay, OverlayInit};

/// Modifier mask for matching keybinds (includes Super to detect compositor keybind hold).
const RELEVANT_MODS: gdk4::ModifierType = gdk4::ModifierType::from_bits_retain(
    gdk4::ModifierType::CONTROL_MASK.bits()
        | gdk4::ModifierType::SHIFT_MASK.bits()
        | gdk4::ModifierType::ALT_MASK.bits()
        | gdk4::ModifierType::SUPER_MASK.bits(),
);

/// Modifier mask for workspace key actions (excludes Super so holding Mod doesn't block input).
const ACTION_MODS: gdk4::ModifierType = gdk4::ModifierType::from_bits_retain(
    gdk4::ModifierType::CONTROL_MASK.bits()
        | gdk4::ModifierType::SHIFT_MASK.bits()
        | gdk4::ModifierType::ALT_MASK.bits(),
);
