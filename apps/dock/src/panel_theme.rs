//! Shared look and focus handling for the menu-bar panels (search and battery).
//!
//! Glass colours live in [`GlassTokens::LIGHT_GLASS`] and
//! [`GlassTokens::DARK_GLASS`]. That is the only place that defines them;
//! drawing code reads [`GlassTokens::current`]. The frontend pass tunes those
//! two constants.
//!
//! The panels do not extend the DWM frame into the client area. GDI would be
//! drawn with no alpha there and added onto the blur. Text is a composition
//! surface. The search field is a real EDIT control, so its fill stays opaque.
//!
//! Everything here runs only while a panel opens, closes, or is redrawn
//! because of a key, a click or a power change.

use std::mem::size_of;

use muelle_shell::system;
use windows::{
    Win32::{
        Foundation::{COLORREF, HWND},
        Graphics::Dwm::{
            DWMWA_BORDER_COLOR, DWMWA_USE_HOSTBACKDROPBRUSH, DWMWA_USE_IMMERSIVE_DARK_MODE,
            DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DwmSetWindowAttribute,
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

/// Straight sRGB. `a` is 255 for an opaque colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba {
    const fn rgb(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    const fn from_color(color: COLORREF, alpha: u8) -> Self {
        Self {
            r: (color.0 & 0xff) as u8,
            g: ((color.0 >> 8) & 0xff) as u8,
            b: ((color.0 >> 16) & 0xff) as u8,
            a: alpha,
        }
    }

    #[must_use]
    pub const fn colorref(self) -> COLORREF {
        rgb(self.r, self.g, self.b)
    }
}

/// Colours of the search and battery panels.
///
/// Change [`Self::LIGHT_GLASS`] and [`Self::DARK_GLASS`] to retune the glass.
/// Nothing else hardcodes these colours. When Windows transparency is off
/// (or this is not Windows 11), [`Self::current`] returns the opaque
/// [`Palette`] instead, with `glass` false.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GlassTokens {
    pub light: bool,
    /// Host backdrop blur is on. False draws an opaque card.
    pub glass: bool,
    pub tint: Rgba,
    pub rim: Rgba,
    pub text: Rgba,
    pub secondary: Rgba,
    pub link: Rgba,
    pub select: Rgba,
    /// Search field fill. Always opaque: the EDIT control paints over it.
    pub field: Rgba,
    pub field_border: Rgba,
}

impl GlassTokens {
    /// Light glass. Placeholder until the frontend pass replaces it.
    pub const LIGHT_GLASS: Self = Self {
        light: true,
        glass: true,
        // menu.rs `popup_tint` light glass: 246,246,248 at alpha 215.
        tint: Rgba::rgb(246, 246, 248, 215),
        rim: Rgba::rgb(255, 255, 255, 220),
        text: Rgba::rgb(26, 26, 28, 255),
        secondary: Rgba::rgb(70, 70, 78, 255),
        link: Rgba::rgb(0, 90, 170, 255),
        select: Rgba::rgb(0, 0, 0, 28),
        field: Rgba::rgb(255, 255, 255, 255),
        field_border: Rgba::rgb(255, 255, 255, 190),
    };

    /// Dark glass. Placeholder until the frontend pass replaces it.
    pub const DARK_GLASS: Self = Self {
        light: false,
        glass: true,
        // menu.rs `popup_tint` dark glass: 38,38,40 at alpha 175.
        tint: Rgba::rgb(38, 38, 40, 175),
        rim: Rgba::rgb(255, 255, 255, 96),
        text: Rgba::rgb(245, 245, 247, 255),
        secondary: Rgba::rgb(196, 196, 204, 255),
        link: Rgba::rgb(140, 200, 255, 255),
        select: Rgba::rgb(255, 255, 255, 36),
        field: Rgba::rgb(28, 28, 32, 255),
        field_border: Rgba::rgb(255, 255, 255, 56),
    };

    /// One registry read for the app theme, plus the transparency switches.
    /// Called when a panel opens, not while it is closed.
    #[must_use]
    pub fn current() -> Self {
        Self::for_theme(
            system::apps_use_light_theme(),
            system::transparency_enabled(),
        )
    }

    #[must_use]
    pub fn for_theme(light: bool, glass: bool) -> Self {
        if glass {
            if light {
                Self::LIGHT_GLASS
            } else {
                Self::DARK_GLASS
            }
        } else {
            Self::solid(Palette::for_light(light))
        }
    }

    fn solid(palette: Palette) -> Self {
        Self {
            light: palette.light,
            glass: false,
            tint: Rgba::from_color(palette.fill, 255),
            rim: Rgba::from_color(palette.border, 255),
            text: Rgba::from_color(palette.text, 255),
            secondary: Rgba::from_color(palette.secondary, 255),
            link: Rgba::from_color(palette.link, 255),
            select: Rgba::from_color(palette.select, 255),
            field: Rgba::from_color(palette.edit, 255),
            field_border: Rgba::from_color(palette.border, 255),
        }
    }
}

/// `DWMWA_COLOR_NONE`: the composition stroke is the glass rim, so the DWM
/// border is not drawn as well.
const DWM_BORDER_NONE: COLORREF = COLORREF(0xFFFF_FFFE);

/// Rounded corners and the title-bar theme. Host backdrop only while glass
/// is on. No frame extension: GDI on an extended frame is drawn with no alpha.
pub fn style_window(hwnd: HWND, tokens: GlassTokens) {
    let corner = DWMWCP_ROUND.0;
    let dark = BOOL::from(!tokens.light);
    let host = BOOL::from(tokens.glass);
    let border = if tokens.glass {
        DWM_BORDER_NONE
    } else {
        tokens.rim.colorref()
    };
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
            DWMWA_USE_HOSTBACKDROPBRUSH,
            (&raw const host).cast(),
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
                // Only on this failure: names the window that kept the focus.
                let class = muelle_shell::tracker::foreground_class();
                muelle_shell::log::line(&format!(
                    "{what}: Windows no dio el foco al panel (frente: {class})"
                ));
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

    #[test]
    fn glass_placeholders_follow_the_theme_and_keep_the_field_opaque() {
        let light = GlassTokens::for_theme(true, true);
        let dark = GlassTokens::for_theme(false, true);
        assert!(light.glass && dark.glass);
        assert_eq!(light.field.a, 255);
        assert_eq!(dark.field.a, 255);
        assert!(light.tint.a < 255 && dark.tint.a < 255);
        assert_ne!(
            (light.tint.r, light.tint.g, light.tint.b),
            (dark.tint.r, dark.tint.g, dark.tint.b)
        );
        let sum = |color: Rgba| u16::from(color.r) + u16::from(color.g) + u16::from(color.b);
        assert!(sum(light.text) < sum(light.tint));
        assert!(sum(dark.text) > sum(dark.tint));
    }

    #[test]
    fn solid_panels_use_the_opaque_palette() {
        for light in [true, false] {
            let palette = Palette::for_light(light);
            let tokens = GlassTokens::for_theme(light, false);
            assert!(!tokens.glass);
            assert_eq!(tokens.tint, Rgba::from_color(palette.fill, 255));
            assert_eq!(tokens.text, Rgba::from_color(palette.text, 255));
            assert_eq!(tokens.secondary, Rgba::from_color(palette.secondary, 255));
            assert_eq!(tokens.link, Rgba::from_color(palette.link, 255));
            assert_eq!(tokens.field, Rgba::from_color(palette.edit, 255));
            assert_eq!(tokens.field.a, 255);
        }
    }
}
