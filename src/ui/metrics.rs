use std::cell::RefCell;

use gtk4::prelude::*;

use crate::config::KeyboardLayout;

thread_local! {
    static DYNAMIC_PROVIDER: RefCell<Option<gtk4::CssProvider>> = const { RefCell::new(None) };
}

#[derive(Clone, Copy)]
pub(super) struct KeyboardMetrics {
    pub(super) key_size: i32,
    pub(super) key_gap: i32,
    pub(super) layout: &'static KeyboardLayout,
}

impl KeyboardMetrics {
    /// Compute key size so the keyboard fills ~80% of the monitor width.
    /// The widest row determines the divisor (varies by layout).
    /// With gap = key/8, total width ≈ divisor * key-widths.
    pub(super) fn from_monitor_width(width: i32, layout: &'static KeyboardLayout) -> Self {
        let target = f64::from(width) * 0.80;
        let key_size = ((target / layout.widest_row_divisor) as i32).clamp(48, 200);
        let key_gap = (key_size + 7) / 8;
        Self {
            key_size,
            key_gap,
            layout,
        }
    }

    pub(super) fn row_margin(&self, row_idx: usize) -> i32 {
        (self.layout.row_offsets[row_idx] * f64::from(self.key_size + self.key_gap)) as i32
    }

    /// Generate CSS custom properties scaled to the key size.
    pub(super) fn scaled_css_variables(&self) -> String {
        let ks = self.key_size;
        format!(
            "window {{\n\
             \x20 --key-margin: {km}px;\n\
             \x20 --key-radius: {kr}px;\n\
             \x20 --key-pad-v: {kpv}px;\n\
             \x20 --key-pad-h: {kph}px;\n\
             \x20 --font-char: {fc}px;\n\
             \x20 --font-name: {fn_}px;\n\
             \x20 --font-detail: {fd}px;\n\
             \x20 --section-gap: {sg}px;\n\
             \x20 --font-tab: {ft}px;\n\
             \x20 --tab-pad-h: {tph}px;\n\
             \x20 --tab-radius: {tr}px;\n\
             \x20 --font-footer: {ff}px;\n\
             }}",
            km = ks / 8,
            kr = ks / 8,
            kpv = ks / 16,
            kph = ks / 10,
            fc = ks * 32 / 100,
            fn_ = ks * 13 / 100,
            fd = ks * 10 / 100,
            sg = ks / 5,
            ft = ks * 14 / 100,
            tph = ks / 6,
            tr = ks / 12,
            ff = ks * 15 / 100,
        )
    }
}

pub(super) fn get_monitor_width(monitor: Option<&gdk4::Monitor>) -> i32 {
    monitor
        .map(|m| m.geometry().width())
        .or_else(|| {
            gdk4::Display::default()
                .and_then(|d| d.monitors().item(0))
                .and_then(|obj| obj.downcast::<gdk4::Monitor>().ok())
                .map(|m| m.geometry().width())
        })
        .unwrap_or(1920)
}

/// Look up a GDK monitor by its connector (output) name.
pub(super) fn find_monitor_for_output(output_name: &str) -> Option<gdk4::Monitor> {
    let display = gdk4::Display::default()?;
    let monitors = display.monitors();
    for i in 0..monitors.n_items() {
        let monitor = monitors.item(i)?.downcast::<gdk4::Monitor>().ok()?;
        if monitor.connector().as_deref() == Some(output_name) {
            return Some(monitor);
        }
    }
    None
}

pub(super) fn apply_scaled_css(css: &str) {
    let Some(display) = gdk4::Display::default() else {
        return;
    };
    DYNAMIC_PROVIDER.with(|cell| {
        let mut opt = cell.borrow_mut();
        if let Some(old) = opt.take() {
            gtk4::style_context_remove_provider_for_display(&display, &old);
        }
        let provider = gtk4::CssProvider::new();
        provider.load_from_data(css);
        gtk4::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk4::STYLE_PROVIDER_PRIORITY_USER,
        );
        *opt = Some(provider);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{LAYOUT_DVORAK, LAYOUT_QWERTY};

    #[test]
    fn keyboard_metrics_qwerty_from_1920() {
        let m = KeyboardMetrics::from_monitor_width(1920, &LAYOUT_QWERTY);
        // 1920 * 0.80 / 11.6875 ≈ 131
        assert_eq!(m.key_size, 131);
        assert_eq!(m.key_gap, 17); // (131 + 7) / 8 = 17
    }

    #[test]
    fn keyboard_metrics_dvorak_from_1920() {
        let m = KeyboardMetrics::from_monitor_width(1920, &LAYOUT_DVORAK);
        // 1920 * 0.80 / 11.96875 ≈ 128
        assert_eq!(m.key_size, 128);
        assert_eq!(m.key_gap, 16); // (128 + 7) / 8 = 16
    }

    #[test]
    fn keyboard_metrics_row_margins() {
        let m = KeyboardMetrics::from_monitor_width(1920, &LAYOUT_QWERTY);
        assert_eq!(m.row_margin(0), 0);
        // Row 1: 0.5 * (131 + 17) = 74
        assert_eq!(m.row_margin(1), 74);
        // Row 2: 0.75 * 148 = 111
        assert_eq!(m.row_margin(2), 111);
        // Row 3: 1.25 * 148 = 185
        assert_eq!(m.row_margin(3), 185);
    }

    #[test]
    fn keyboard_metrics_clamps_small() {
        let m = KeyboardMetrics::from_monitor_width(400, &LAYOUT_QWERTY);
        assert_eq!(m.key_size, 48); // clamped to minimum
    }

    #[test]
    fn keyboard_metrics_clamps_large() {
        let m = KeyboardMetrics::from_monitor_width(8000, &LAYOUT_QWERTY);
        assert_eq!(m.key_size, 200); // clamped to maximum
    }
}
