//! Shared look and focus handling for the menu-bar panels drawn with GDI
//! (search and battery).
//!
//! GDI writes no alpha, so these windows must not extend the DWM frame into
//! their client area: on a glass sheet every GDI colour would be added to the
//! backdrop and text would wash out. They are opaque cards that follow the
//! Windows app theme, with DWM rounded corners and a thin border.
//!
//! Everything here runs only while a panel opens or closes.

use std::mem::size_of;

use muelle_shell::system;
use windows::{
    Win32::{
        Foundation::{COLORREF, HWND},
        Graphics::Dwm::{
            DWMWA_BORDER_COLOR, DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE,
            DWMWCP_ROUND, DwmSetWindowAttribute,
        },
        System::Threading::{AttachThreadInput, GetCurrentThreadId},
        UI::{
            Input::KeyboardAndMouse::SetFocus,
            WindowsAndMessaging::{
                GetForegroundWindow, GetWindowThreadProcessId, SetForegroundWindow,
            },
        },
    },
    core::BOOL,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    pub light: bool,
    pub fill: COLORREF,
    pub edit: COLORREF,
    pub select: COLORREF,
    pub text: COLORREF,
    pub secondary: COLORREF,
    pub link: COLORREF,
    pub border: COLORREF,
}

impl Palette {
    /// Read when a panel opens: one registry value, no watcher.
    #[must_use]
    pub fn current() -> Self {
        Self::for_light(system::apps_use_light_theme())
    }

    #[must_use]
    pub const fn for_light(light: bool) -> Self {
        if light {
            Self {
                light,
                fill: rgb(249, 249, 251),
                edit: rgb(255, 255, 255),
                select: rgb(229, 234, 243),
                text: rgb(26, 26, 28),
                secondary: rgb(96, 96, 104),
                link: rgb(0, 95, 184),
                border: rgb(214, 214, 220),
            }
        } else {
            Self {
                light,
                fill: rgb(40, 40, 44),
                edit: rgb(28, 28, 32),
                select: rgb(62, 66, 76),
                text: rgb(245, 245, 247),
                secondary: rgb(184, 184, 192),
                link: rgb(120, 190, 255),
                border: rgb(72, 72, 80),
            }
        }
    }
}

#[must_use]
pub const fn rgb(red: u8, green: u8, blue: u8) -> COLORREF {
    COLORREF(red as u32 | ((green as u32) << 8) | ((blue as u32) << 16))
}

/// Rounded corners, title-bar theme and border colour. No system backdrop
/// and no frame extension (see the module notes).
pub fn style_window(hwnd: HWND, palette: Palette) {
    let corner = DWMWCP_ROUND.0;
    let dark = BOOL::from(!palette.light);
    let border = palette.border;
    // SAFETY: Our own window; each value outlives its call.
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            (&raw const corner).cast(),
            size_of::<i32>() as u32,
        );
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            (&raw const dark).cast(),
            size_of::<BOOL>() as u32,
        );
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_BORDER_COLOR,
            (&raw const border).cast(),
            size_of::<COLORREF>() as u32,
        );
    }
}

/// Brings `hwnd` to the front with the keyboard focus on `focus`.
///
/// Called from the menu-bar click, so this process owns the last input and
/// `SetForegroundWindow` normally succeeds on its own. Joining the input of
/// the app in front is only a fallback: it makes Windows send the panel a
/// passing `WA_INACTIVE`, which the panels re-check before closing.
pub fn take_focus(hwnd: HWND, focus: HWND, what: &str) {
    // SAFETY: Focus calls on our own windows; the input join is undone below.
    unsafe {
        if !SetForegroundWindow(hwnd).as_bool() {
            let foreground = GetForegroundWindow();
            let fore_thread = GetWindowThreadProcessId(foreground, None);
            let our_thread = GetCurrentThreadId();
            let mut taken = false;
            if fore_thread != 0 && fore_thread != our_thread {
                let _ = AttachThreadInput(fore_thread, our_thread, true);
                taken = SetForegroundWindow(hwnd).as_bool();
                let _ = AttachThreadInput(fore_thread, our_thread, false);
            }
            if !taken {
                muelle_shell::log::line(&format!("{what}: Windows no dio el foco al panel"));
            }
        }
        let _ = SetFocus(Some(focus));
    }
}

/// Whether `hwnd` still is the window in front.
#[must_use]
pub fn is_foreground(hwnd: HWND) -> bool {
    // SAFETY: Read-only query.
    unsafe { GetForegroundWindow() == hwnd }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn luminance(color: COLORREF) -> f32 {
        let channel = |shift: u32| {
            let value = ((color.0 >> shift) & 0xff) as f32 / 255.0;
            if value <= 0.039_28 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(0) + 0.7152 * channel(8) + 0.0722 * channel(16)
    }

    fn contrast(a: COLORREF, b: COLORREF) -> f32 {
        let (a, b) = (luminance(a), luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    #[test]
    fn text_is_readable_in_both_themes() {
        for light in [true, false] {
            let palette = Palette::for_light(light);
            for background in [palette.fill, palette.select, palette.edit] {
                assert!(contrast(palette.text, background) >= 7.0, "light {light}");
                assert!(contrast(palette.link, background) >= 4.5, "light {light}");
            }
            for background in [palette.fill, palette.select] {
                assert!(
                    contrast(palette.secondary, background) >= 4.5,
                    "light {light}"
                );
            }
        }
    }
}
