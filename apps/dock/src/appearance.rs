//! The theme Settings chose for the dock, the menu bar and their menus and
//! panels. New installs use Glass, which is light and does not follow Windows
//! dark mode. `Auto` follows Windows' light or dark mode; the others fix both
//! the material and whether content is drawn dark or light.
//!
//! It is process state, like the Windows theme it replaces: everything that
//! draws reads `current()` when it builds, and a change rebuilds through the
//! same keys a Windows theme change does.

use std::sync::atomic::{AtomicU8, Ordering};

use muelle_config::{Appearance, GLASS_BLUR_DEFAULT, GLASS_LEVEL_MAX, GLASS_TRANSPARENCY_DEFAULT};
use muelle_shell::system::{self, translucency_allowed, uses_light_theme};

/// What the glass is made of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Material {
    /// The dock as it always was: the light or dark macOS glass.
    Standard,
    Frosted,
    Clear,
    Aurora,
    /// Light glass. Blur and transparency come from the Glass sliders.
    Glass,
}

#[cfg(test)]
impl Material {
    pub const ALL: [Self; 5] = [
        Self::Standard,
        Self::Frosted,
        Self::Clear,
        Self::Aurora,
        Self::Glass,
    ];
}

/// What a surface is drawn for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Look {
    pub material: Material,
    /// Light surfaces with dark content; otherwise the reverse.
    pub light: bool,
    /// Host backdrop is available. See `system::transparency_enabled`.
    pub glass: bool,
    /// A tint may be see-through without blur. See `system::translucency_allowed`.
    pub translucent: bool,
    /// Glass theme only, 0..=100. 100 is the most see-through.
    pub transparency: u8,
    /// Glass theme only, 0..=100. 0 is no blur.
    pub blur: u8,
}

impl Look {
    /// A surface that is not driven by the live Glass sliders. The sliders
    /// sit at their defaults so a Glass material still has a defined tint;
    /// the other materials ignore them.
    #[must_use]
    pub const fn classic(material: Material, light: bool, glass: bool) -> Self {
        Self {
            material,
            light,
            glass,
            translucent: glass,
            transparency: GLASS_TRANSPARENCY_DEFAULT,
            blur: GLASS_BLUR_DEFAULT,
        }
    }
}

static CHOICE: AtomicU8 = AtomicU8::new(Appearance::Auto as u8);
static TRANSPARENCY: AtomicU8 = AtomicU8::new(GLASS_TRANSPARENCY_DEFAULT);
static BLUR: AtomicU8 = AtomicU8::new(GLASS_BLUR_DEFAULT);

pub fn set(choice: Appearance) {
    CHOICE.store(choice as u8, Ordering::Relaxed);
}

/// Glass theme sliders. Each value is clamped to 0..=100. Other themes ignore them.
pub fn set_glass_levels(transparency: u8, blur: u8) {
    TRANSPARENCY.store(transparency.min(GLASS_LEVEL_MAX), Ordering::Relaxed);
    BLUR.store(blur.min(GLASS_LEVEL_MAX), Ordering::Relaxed);
}

const CHOICES: [Appearance; 7] = [
    Appearance::Light,
    Appearance::Dark,
    Appearance::Frosted,
    Appearance::Clear,
    Appearance::Aurora,
    Appearance::Glass,
    Appearance::Auto,
];

fn decode(stored: u8) -> Appearance {
    CHOICES
        .into_iter()
        .find(|choice| *choice as u8 == stored)
        .unwrap_or_default()
}

#[must_use]
pub fn resolve(
    choice: Appearance,
    system_light: bool,
    glass: bool,
    translucent: bool,
    transparency: u8,
    blur: u8,
) -> Look {
    let (material, light) = match choice {
        Appearance::Auto => (Material::Standard, system_light),
        Appearance::Light => (Material::Standard, true),
        Appearance::Dark => (Material::Standard, false),
        Appearance::Frosted => (Material::Frosted, true),
        Appearance::Clear => (Material::Clear, false),
        Appearance::Aurora => (Material::Aurora, false),
        Appearance::Glass => (Material::Glass, true),
    };
    Look {
        material,
        light,
        glass,
        translucent,
        transparency: transparency.min(GLASS_LEVEL_MAX),
        blur: blur.min(GLASS_LEVEL_MAX),
    }
}

/// Read on every rebuild. Transparency, energy saver and the Glass sliders
/// are picked up here, so a Windows theme change or a power notification
/// already rebuilds the dock when they change.
#[must_use]
pub fn current() -> Look {
    let choice = decode(CHOICE.load(Ordering::Relaxed));
    // Only `Auto` needs the Windows theme.
    let system_light = choice == Appearance::Auto && uses_light_theme();
    // Same pair `transparency_enabled` reports: the backdrop exists only when
    // Windows 11 allows the same translucency.
    let translucent = translucency_allowed();
    let glass = system::is_windows_11() && translucent;
    resolve(
        choice,
        system_light,
        glass,
        translucent,
        TRANSPARENCY.load(Ordering::Relaxed),
        BLUR.load(Ordering::Relaxed),
    )
}

/// Tint alpha for Glass when the host backdrop is available.
///
/// 235 at 0 (almost solid), 50 at 65, 35 at 80, 25 at the Clear Liquid Glass
/// default of 90, and 15 at 100. The straight line that put 65 at alpha 95
/// laid a white wash over the live blur, which reads as a flat gray pill on a
/// dark wallpaper. This curve drops quickly through that range and then eases,
/// and every step still lowers the alpha.
#[must_use]
pub fn glass_tint_alpha(transparency: u8) -> u8 {
    let level = u32::from(transparency.min(GLASS_LEVEL_MAX));
    let alpha = if level <= 60 {
        235 - 3 * level
    } else {
        115 - level
    };
    u8::try_from(alpha).unwrap_or(15)
}

/// Opacity of the host-backdrop sprite. 0 is a sharp wallpaper, 1 is the
/// full system blur.
#[must_use]
pub fn glass_backdrop_opacity(blur: u8) -> f32 {
    f32::from(blur.min(GLASS_LEVEL_MAX)) / f32::from(GLASS_LEVEL_MAX)
}

/// Minimum Glass tint alpha when there is no backdrop, so icons stay readable
/// on Windows 10.
pub const GLASS_UNBLURRED_ALPHA_FLOOR: u8 = 150;

/// Tint alpha for Glass when translucency is allowed but the backdrop is not.
#[must_use]
pub fn glass_unblurred_tint_alpha(transparency: u8) -> u8 {
    glass_tint_alpha(transparency).max(GLASS_UNBLURRED_ALPHA_FLOOR)
}

/// One startup line: why the dock is glass, translucent or solid on this PC.
pub fn log_capabilities() {
    let support = system::glass_support();
    let look = current();
    muelle_shell::log::line(&format!(
        "vidrio: windows 11 {win}, efectos de transparencia {effects}, ahorro de energía {saver}, material {material:?}, claro {light}, fondo {glass}, translúcido {translucent}, transparencia {transparency}, desenfoque {blur}",
        win = yes_no(support.windows_11),
        effects = yes_no(support.transparency_effects),
        saver = yes_no(support.energy_saver),
        material = look.material,
        light = yes_no(look.light),
        glass = yes_no(look.glass),
        translucent = yes_no(look.translucent),
        transparency = look.transparency,
        blur = look.blur,
    ));
}

fn yes_no(on: bool) -> &'static str {
    if on { "sí" } else { "no" }
}

/// Whether menus and labels are drawn light, with dark content.
#[must_use]
pub fn light() -> bool {
    current().light
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(choice: Appearance, system_light: bool, glass: bool) -> Look {
        resolve(
            choice,
            system_light,
            glass,
            glass,
            GLASS_TRANSPARENCY_DEFAULT,
            GLASS_BLUR_DEFAULT,
        )
    }

    #[test]
    fn auto_follows_windows_and_the_others_do_not() {
        for system_light in [true, false] {
            assert_eq!(
                sample(Appearance::Auto, system_light, true).light,
                system_light
            );
            assert!(sample(Appearance::Light, system_light, true).light);
            assert!(sample(Appearance::Frosted, system_light, true).light);
            assert!(sample(Appearance::Glass, system_light, true).light);
            assert_eq!(
                sample(Appearance::Glass, system_light, true).material,
                Material::Glass
            );
            for dark in [Appearance::Dark, Appearance::Clear, Appearance::Aurora] {
                assert!(!sample(dark, system_light, true).light);
            }
        }
    }

    #[test]
    fn glass_levels_are_stored_and_clamped() {
        let look = resolve(Appearance::Glass, false, true, false, 250, 40);
        assert!(look.light);
        assert!(look.glass);
        assert!(!look.translucent);
        assert_eq!(look.transparency, GLASS_LEVEL_MAX);
        assert_eq!(look.blur, 40);
    }

    #[test]
    fn glass_tint_and_blur_map_the_ends_and_stay_monotonic() {
        assert_eq!(glass_tint_alpha(0), 235);
        assert_eq!(glass_tint_alpha(60), 55);
        assert_eq!(glass_tint_alpha(65), 50);
        assert_eq!(glass_tint_alpha(80), 35);
        assert_eq!(glass_tint_alpha(90), 25);
        assert_eq!(glass_tint_alpha(100), 15);
        assert_eq!(glass_backdrop_opacity(0), 0.0);
        assert_eq!(glass_backdrop_opacity(45), 0.45);
        assert_eq!(glass_backdrop_opacity(100), 1.0);
        let mut previous_alpha = glass_tint_alpha(0);
        let mut previous_blur = glass_backdrop_opacity(0);
        for level in 1..=GLASS_LEVEL_MAX {
            let alpha = glass_tint_alpha(level);
            let blur = glass_backdrop_opacity(level);
            assert!(alpha < previous_alpha);
            assert!(blur > previous_blur);
            previous_alpha = alpha;
            previous_blur = blur;
        }
    }

    #[test]
    fn the_unblurred_floor_keeps_a_clear_tint_readable() {
        assert_eq!(glass_unblurred_tint_alpha(0), glass_tint_alpha(0));
        assert!(glass_tint_alpha(65) < GLASS_UNBLURRED_ALPHA_FLOOR);
        assert_eq!(glass_unblurred_tint_alpha(65), GLASS_UNBLURRED_ALPHA_FLOOR);
        assert_eq!(glass_unblurred_tint_alpha(100), GLASS_UNBLURRED_ALPHA_FLOOR);
        assert!(glass_tint_alpha(100) < GLASS_UNBLURRED_ALPHA_FLOOR);
    }

    #[test]
    fn every_choice_decodes_as_itself() {
        for choice in CHOICES {
            assert_eq!(decode(choice as u8), choice);
        }
    }
}
