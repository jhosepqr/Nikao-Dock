use std::{mem::size_of, sync::OnceLock};

use serde::Serialize;

use windows::{
    Win32::{
        Foundation::{ERROR_SUCCESS, HLOCAL, LPARAM, LocalFree, POINT, RECT, STATUS_SUCCESS},
        Graphics::Gdi::{
            EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITOR_DEFAULTTONULL,
            MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromPoint,
        },
        System::{
            Com::CoTaskMemFree,
            LibraryLoader::{GetProcAddress, LoadLibraryW},
            Power::{
                CallNtPowerInformation, PowerGetActiveScheme, PowerReadFriendlyName,
                SYSTEM_BATTERY_STATE, SystemBatteryState,
            },
            Registry::{
                HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ,
                RegGetValueW,
            },
        },
        UI::{
            HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI},
            Shell::{FOLDERID_Downloads, KF_FLAG_DEFAULT, SHGetKnownFolderPath},
            WindowsAndMessaging::{SPI_GETCLIENTAREAANIMATION, SystemParametersInfoW},
        },
    },
    core::{BOOL, GUID, PCWSTR, s, w},
};

/// Returns `true` if Windows uses the light theme for the taskbar and shell.
///
/// Assumes the dark theme on read failure (the Windows 11 shell default).
#[must_use]
pub fn uses_light_theme() -> bool {
    personalize(w!("SystemUsesLightTheme")).is_some_and(|value| value != 0)
}

/// Returns `true` while the host backdrop brush is available: Windows 11,
/// "Transparency effects" on, and energy saver off (it turns the effect off).
/// Otherwise the backdrop stops blurring and shows up black, so surfaces that
/// rely on it have to be drawn solid.
///
/// On Windows 10 there is no backdrop brush for desktop windows at all
/// (`DWMWA_USE_HOSTBACKDROPBRUSH` arrived with Windows 11), so this is always
/// `false` there. A tint with no blur can still be drawn when
/// [`translucency_allowed`] is true.
#[must_use]
pub fn transparency_enabled() -> bool {
    is_windows_11() && translucency_allowed()
}

/// "Transparency effects" is on and energy saver is off, on any Windows
/// version. This allows a translucent tint without blur on Windows 10, where
/// [`transparency_enabled`] is always false.
#[must_use]
pub fn translucency_allowed() -> bool {
    // Missing means never changed, and the default is on.
    !energy_saver_on() && transparency_effects_on()
}

/// What Settings shows to explain a Glass dock that cannot blur.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GlassSupport {
    pub windows_11: bool,
    pub transparency_effects: bool,
    pub energy_saver: bool,
}

/// The three switches behind [`transparency_enabled`] and [`translucency_allowed`].
#[must_use]
pub fn glass_support() -> GlassSupport {
    GlassSupport {
        windows_11: is_windows_11(),
        transparency_effects: transparency_effects_on(),
        energy_saver: energy_saver_on(),
    }
}

fn transparency_effects_on() -> bool {
    personalize(w!("EnableTransparency")) != Some(0)
}

/// `true` while energy saver (battery saver on Windows 10) is on.
#[must_use]
pub fn energy_saver_on() -> bool {
    if let Some(on) = crate::power::notified_saver_state() {
        return on;
    }
    use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
    const SAVER_ON: u8 = 1;
    let mut status = SYSTEM_POWER_STATUS::default();
    // SAFETY: `status` is a valid structure the call fills in.
    unsafe { GetSystemPowerStatus(&mut status) }.is_ok() && status.SystemStatusFlag & SAVER_ON != 0
}

/// The input language of the window in front as two capital letters
/// ("ES", "EN"), or `None` with a single language installed: the Windows
/// taskbar only shows its language indicator when there is a choice.
#[must_use]
pub fn input_language() -> Option<String> {
    use windows::Win32::{
        Globalization::LCIDToLocaleName, UI::Input::KeyboardAndMouse::GetKeyboardLayoutList,
    };
    // SAFETY: Read-only queries; `name` is a valid buffer for the locale name.
    unsafe {
        if GetKeyboardLayoutList(None) < 2 {
            return None;
        }
        // Each thread has its own layout: the one that matters is in front.
        let language = (crate::keyboard::Context::current()?.layout as usize & 0xFFFF) as u32;
        let mut name = [0u16; 85];
        let len = usize::try_from(LCIDToLocaleName(language, Some(&mut name), 0)).ok()?;
        let tag = String::from_utf16_lossy(name.get(..len.checked_sub(1)?)?);
        let code = tag.split('-').next()?.to_uppercase();
        (!code.is_empty()).then_some(code)
    }
}

/// `true` on Windows 11 (build 22000 or later), `false` on Windows 10.
/// Read once: the version does not change while the dock runs.
#[must_use]
pub fn is_windows_11() -> bool {
    static WINDOWS_11: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *WINDOWS_11.get_or_init(|| windows_build().is_none_or(|build| build >= 22000))
}

/// The Windows build number, e.g. 26100. Windows reports "10.0" for both
/// Windows 10 and 11; only the build tells them apart.
fn windows_build() -> Option<u32> {
    let mut buffer = [0u16; 16];
    let mut size = size_of_val(&buffer) as u32;
    // SAFETY: `buffer` and `size` describe a valid buffer for the string.
    unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            w!("SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion"),
            w!("CurrentBuildNumber"),
            RRF_RT_REG_SZ,
            None,
            Some(buffer.as_mut_ptr().cast()),
            Some(&mut size),
        )
    }
    .ok()
    .ok()?;
    let len = buffer
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..len]).parse().ok()
}

/// A DWORD from the user's personalization settings.
/// The accent color chosen in Settings → Personalization → Colors, as
/// red, green and blue.
#[must_use]
pub fn accent_color() -> Option<[u8; 3]> {
    let mut value = 0u32;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: `value` and `size` describe a valid 4-byte buffer.
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!(r"Software\Microsoft\Windows\DWM"),
            w!("AccentColor"),
            RRF_RT_REG_DWORD,
            None,
            Some((&raw mut value).cast()),
            Some(&mut size),
        )
    }
    .ok()
    .ok()?;
    // Stored as 0xAABBGGRR.
    let [r, g, b, _] = value.to_le_bytes();
    Some([r, g, b])
}

fn personalize(name: PCWSTR) -> Option<u32> {
    let mut value = 0u32;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: `value` and `size` describe a valid 4-byte buffer.
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            name,
            RRF_RT_REG_DWORD,
            None,
            Some((&raw mut value).cast()),
            Some(&mut size),
        )
    }
    .is_ok()
    .then_some(value)
}

/// Monitor bounds, work area, and scale in physical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Monitor {
    /// Full-screen bounds.
    pub screen: RECT,
    pub work_left: i32,
    pub work_top: i32,
    pub work_right: i32,
    pub work_bottom: i32,
    /// 1.0 = 96 DPI (100%).
    pub scale: f32,
}

/// Primary monitor. `None` only if Windows cannot describe it.
#[must_use]
pub fn primary_monitor() -> Option<Monitor> {
    // SAFETY: The origin is on the primary monitor in virtual screen coordinates.
    monitor_info(unsafe { MonitorFromPoint(POINT::default(), MONITOR_DEFAULTTOPRIMARY) })
}

/// The monitor containing a screen point, with no fallback for gaps or disconnected displays.
#[must_use]
pub fn monitor_at(point: POINT) -> Option<Monitor> {
    // SAFETY: Read-only monitor lookup.
    monitor_info(unsafe { MonitorFromPoint(point, MONITOR_DEFAULTTONULL) })
}

/// Physical bounds of the connected displays, including negative coordinates.
#[must_use]
pub fn monitor_rects() -> Vec<RECT> {
    unsafe extern "system" fn append(_: HMONITOR, _: HDC, rect: *mut RECT, data: LPARAM) -> BOOL {
        // SAFETY: EnumDisplayMonitors runs synchronously and supplies the
        // current monitor bounds. The caller owns the vector for that call.
        unsafe {
            (&mut *(data.0 as *mut Vec<RECT>)).push(*rect);
        }
        BOOL(1)
    }
    let mut screens = Vec::new();
    // SAFETY: The callback's vector lives through the synchronous enumeration.
    if unsafe {
        !EnumDisplayMonitors(
            None,
            None,
            Some(append),
            LPARAM((&raw mut screens) as isize),
        )
        .as_bool()
    } {
        screens.clear();
    }
    screens
}

fn monitor_info(handle: HMONITOR) -> Option<Monitor> {
    // SAFETY: Read-only queries using correctly sized structures.
    unsafe {
        let mut info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !GetMonitorInfoW(handle, &mut info).as_bool() {
            return None;
        }
        let (mut dpi_x, mut dpi_y) = (96u32, 96u32);
        // Fall back to 96 DPI.
        let _ = GetDpiForMonitor(handle, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
        Some(Monitor {
            screen: info.rcMonitor,
            work_left: info.rcWork.left,
            work_top: info.rcWork.top,
            work_right: info.rcWork.right,
            work_bottom: info.rcWork.bottom,
            scale: dpi_x as f32 / 96.0,
        })
    }
}

/// Path to the user's Downloads folder, which may be on another drive.
#[must_use]
pub fn downloads_folder() -> Option<String> {
    // SAFETY: The returned string is copied and freed with the allocator that created it.
    unsafe {
        let raw = SHGetKnownFolderPath(&FOLDERID_Downloads, KF_FLAG_DEFAULT, None).ok()?;
        let path = raw.to_string().ok();
        CoTaskMemFree(Some(raw.as_ptr().cast()));
        path
    }
}

/// The languages Muelle's text comes in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UiLanguage {
    Spanish,
    English,
    French,
    Chinese,
    Hindi,
    Arabic,
    Bengali,
    Portuguese,
}

/// The Windows display language, or English when Muelle has no text in it.
#[must_use]
pub fn ui_language() -> UiLanguage {
    // SAFETY: Read-only query.
    let language = unsafe { windows::Win32::Globalization::GetUserDefaultUILanguage() };
    ui_language_from_id(language)
}

fn ui_language_from_id(language: u16) -> UiLanguage {
    // PRIMARYLANGID: regional variants share the low ten bits.
    match language & 0x3FF {
        0x0A => UiLanguage::Spanish,
        0x0C => UiLanguage::French,
        0x04 => UiLanguage::Chinese,
        0x39 => UiLanguage::Hindi,
        0x01 => UiLanguage::Arabic,
        0x45 => UiLanguage::Bengali,
        0x16 => UiLanguage::Portuguese,
        _ => UiLanguage::English,
    }
}

/// `false` if the user turned off "Animation effects" in Windows settings.
#[must_use]
pub fn animations_enabled() -> bool {
    let mut enabled = BOOL::from(true);
    // SAFETY: `enabled` is a valid BOOL the call writes to.
    unsafe {
        let _ = SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            Some((&raw mut enabled).cast()),
            Default::default(),
        );
    }
    enabled.as_bool()
}

/// Enables or disables the DWM transitions (minimize, maximize, close
/// animations) of one window. It works across processes.
pub fn set_window_transitions(hwnd: isize, enabled: bool) {
    let disabled = BOOL::from(!enabled);
    // SAFETY: The value outlives the call; a window that has closed makes it fail harmlessly.
    unsafe {
        let _ = windows::Win32::Graphics::Dwm::DwmSetWindowAttribute(
            windows::Win32::Foundation::HWND(hwnd as *mut _),
            windows::Win32::Graphics::Dwm::DWMWA_TRANSITIONS_FORCEDISABLED,
            (&raw const disabled).cast(),
            size_of::<BOOL>() as u32,
        );
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Battery {
    /// 0..=100.
    pub percent: u8,
    pub charging: bool,
    pub saver: bool,
}

/// Battery state, or `None` on a computer without a battery.
#[must_use]
pub fn battery() -> Option<Battery> {
    use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
    const NO_BATTERY: u8 = 128;
    const UNKNOWN: u8 = 255;
    let mut status = SYSTEM_POWER_STATUS::default();
    // SAFETY: `status` is a valid structure the call fills in.
    unsafe { GetSystemPowerStatus(&mut status) }.ok()?;
    let saver = energy_saver_on();
    (status.BatteryFlag & NO_BATTERY == 0
        && status.BatteryFlag != UNKNOWN
        && status.BatteryLifePercent <= 100)
        .then_some(Battery {
            percent: status.BatteryLifePercent,
            charging: status.ACLineStatus == 1,
            saver,
        })
}

/// Whether the battery is gaining charge, losing it, or sitting on AC.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Charge {
    Charging,
    Discharging,
    /// Plugged in and at 100%.
    Full,
    /// Plugged in, not charging, and not full.
    Plugged,
}

/// The Windows 11 power slider. Energy saver can force efficiency even when
/// the slider still shows the last choice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerMode {
    BestEfficiency,
    Balanced,
    BestPerformance,
}

/// What the battery menu shows. Read on demand; nothing here is cached or polled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatteryMenu {
    pub percent: u8,
    pub charge: Charge,
    pub saver: bool,
    /// Seconds until empty, when Windows can estimate it.
    pub remaining: Option<u32>,
    /// Seconds until full, when the charge rate is known.
    pub until_full: Option<u32>,
    pub mode: Option<PowerMode>,
    /// Localized name of the active power plan. Empty when it cannot be read.
    pub plan: String,
}

/// Battery details for the menu, or `None` when this computer has no battery.
///
/// Callers ask when the menu opens and when Windows sends a power event.
#[must_use]
pub fn battery_menu() -> Option<BatteryMenu> {
    use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
    const NO_BATTERY: u8 = 128;
    const CHARGING: u8 = 8;
    const UNKNOWN: u8 = 255;
    let mut status = SYSTEM_POWER_STATUS::default();
    // SAFETY: `status` is a valid structure the call fills in.
    unsafe { GetSystemPowerStatus(&mut status) }.ok()?;
    if status.BatteryFlag & NO_BATTERY != 0
        || status.BatteryFlag == UNKNOWN
        || status.BatteryLifePercent > 100
    {
        return None;
    }
    let percent = status.BatteryLifePercent;
    let (charge, remaining, until_full) = if let Some(state) = battery_state() {
        let charge = classify_charge(state.AcOnLine, state.Charging, percent);
        let remaining = match charge {
            Charge::Discharging => known_seconds(state.EstimatedTime)
                .or_else(|| known_seconds(status.BatteryLifeTime))
                .or_else(|| remaining_from_rate(state.RemainingCapacity, state.Rate as i32)),
            Charge::Charging | Charge::Full | Charge::Plugged => None,
        };
        let until_full = match charge {
            Charge::Charging => until_full_seconds(
                state.MaxCapacity,
                state.RemainingCapacity,
                state.Rate as i32,
            ),
            Charge::Discharging | Charge::Full | Charge::Plugged => None,
        };
        (charge, remaining, until_full)
    } else {
        let charge = classify_charge(
            status.ACLineStatus == 1,
            status.BatteryFlag & CHARGING != 0,
            percent,
        );
        let remaining = match charge {
            Charge::Discharging => known_seconds(status.BatteryLifeTime),
            Charge::Charging | Charge::Full | Charge::Plugged => None,
        };
        (charge, remaining, None)
    };
    Some(BatteryMenu {
        percent,
        charge,
        saver: energy_saver_on(),
        remaining,
        until_full,
        mode: effective_power_mode(),
        plan: active_plan_name(),
    })
}

fn battery_state() -> Option<SYSTEM_BATTERY_STATE> {
    let mut state = SYSTEM_BATTERY_STATE::default();
    // SAFETY: `state` is a valid output buffer of the size the call expects.
    let status = unsafe {
        CallNtPowerInformation(
            SystemBatteryState,
            None,
            0,
            Some((&raw mut state).cast()),
            size_of::<SYSTEM_BATTERY_STATE>() as u32,
        )
    };
    (status == STATUS_SUCCESS && state.BatteryPresent).then_some(state)
}

fn classify_charge(ac_online: bool, charging: bool, percent: u8) -> Charge {
    if charging {
        Charge::Charging
    } else if ac_online && percent >= 100 {
        Charge::Full
    } else if ac_online {
        Charge::Plugged
    } else {
        Charge::Discharging
    }
}

/// `u32::MAX` is the documented unknown lifetime. Zero is not a useful estimate.
fn known_seconds(value: u32) -> Option<u32> {
    (value != 0 && value != u32::MAX).then_some(value)
}

/// Seconds to fill `max_mwh - remaining_mwh` at `rate_mw`.
/// A negative rate is charging, which is how `SYSTEM_BATTERY_STATE` reports it.
/// A positive rate is accepted too: some batteries only put the sign in `Charging`.
fn until_full_seconds(max_mwh: u32, remaining_mwh: u32, rate_mw: i32) -> Option<u32> {
    let gap = u64::from(max_mwh.saturating_sub(remaining_mwh));
    let rate = if rate_mw < 0 {
        u64::from(rate_mw.unsigned_abs())
    } else if rate_mw > 0 {
        u64::from(rate_mw as u32)
    } else {
        return None;
    };
    (gap != 0 && rate != 0)
        .then(|| u32::try_from(gap * 3600 / rate).ok())
        .flatten()
}

fn remaining_from_rate(capacity_mwh: u32, rate_mw: i32) -> Option<u32> {
    if rate_mw <= 0 || capacity_mwh == 0 {
        return None;
    }
    u32::try_from(u64::from(capacity_mwh) * 3600 / u64::from(rate_mw as u32)).ok()
}

/// Windows 11 power-mode GUIDs from `PowerGetUserConfiguredDCPowerMode`.
const BEST_EFFICIENCY: GUID = GUID::from_u128(0x961c_c777_2547_4f9d_8174_7d86_181b_8a7a);
const BEST_PERFORMANCE: GUID = GUID::from_u128(0xded5_74b5_45a0_4f42_8737_4634_5c09_c238);

fn power_mode(guid: GUID) -> Option<PowerMode> {
    if guid == BEST_EFFICIENCY {
        Some(PowerMode::BestEfficiency)
    } else if guid == GUID::from_u128(0) {
        Some(PowerMode::Balanced)
    } else if guid == BEST_PERFORMANCE {
        Some(PowerMode::BestPerformance)
    } else {
        None
    }
}

fn effective_power_mode() -> Option<PowerMode> {
    let query = overlay_query()?;
    let mut guid = GUID::from_u128(0);
    // SAFETY: `guid` is a live GUID. The resolved export writes one GUID and
    // returns a Win32 error code. A missing export never reaches here.
    let code = unsafe { query(&mut guid) };
    (code == 0).then(|| power_mode(guid)).flatten()
}

type OverlayQuery = unsafe extern "system" fn(*mut GUID) -> u32;

fn overlay_query() -> Option<OverlayQuery> {
    static ADDRESS: OnceLock<usize> = OnceLock::new();
    let address = *ADDRESS.get_or_init(overlay_address);
    (address != 0).then(|| unsafe { std::mem::transmute::<usize, OverlayQuery>(address) })
}

fn overlay_address() -> usize {
    let Ok(module) = (unsafe { LoadLibraryW(w!("powrprof.dll")) }) else {
        return 0;
    };
    let Some(symbol) = (unsafe { GetProcAddress(module, s!("PowerGetEffectiveOverlayScheme")) })
    else {
        return 0;
    };
    symbol as *const () as usize
}

fn active_plan_name() -> String {
    let mut raw = std::ptr::null_mut();
    // SAFETY: On success the call allocates a GUID the caller frees with LocalFree.
    if unsafe { PowerGetActiveScheme(None, &mut raw) } != ERROR_SUCCESS || raw.is_null() {
        return String::new();
    }
    let guid = unsafe { raw.read() };
    unsafe {
        let _ = LocalFree(Some(HLOCAL(raw.cast())));
    }
    let mut bytes = [0u8; 512];
    let mut size = bytes.len() as u32;
    // SAFETY: The buffer and size live for the call. Failure leaves the name blank.
    if unsafe {
        PowerReadFriendlyName(
            None,
            Some(&raw const guid),
            None,
            None,
            Some(bytes.as_mut_ptr()),
            &mut size,
        )
    } != ERROR_SUCCESS
    {
        return String::new();
    }
    let count = (size as usize / 2).min(bytes.len() / 2);
    let (pairs, _) = bytes[..count * 2].as_chunks::<2>();
    let units: Vec<u16> = pairs
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .take_while(|unit| *unit != 0)
        .collect();
    String::from_utf16_lossy(&units)
}

/// Current date and time in the regional format of the user, e.g. "vie 13 nov  19:42".
#[must_use]
pub fn clock_text() -> String {
    use windows::Win32::Globalization::{GetDateFormatEx, GetTimeFormatEx, TIME_NOSECONDS};
    let mut date = [0u16; 64];
    let mut time = [0u16; 32];
    // SAFETY: The buffers live for the calls, which return the length written
    // including the terminator (0 on failure).
    let (date_len, time_len) = unsafe {
        (
            GetDateFormatEx(
                None,
                Default::default(),
                None,
                w!("ddd d MMM"),
                Some(&mut date),
                None,
            ),
            GetTimeFormatEx(None, TIME_NOSECONDS, None, None, Some(&mut time)),
        )
    };
    let text =
        |buffer: &[u16], len: i32| String::from_utf16_lossy(&buffer[..(len - 1).max(0) as usize]);
    format!("{}  {}", text(&date, date_len), text(&time, time_len))
}

#[cfg(test)]
mod language_tests {
    use super::{UiLanguage, ui_language_from_id};

    #[test]
    fn windows_display_languages_include_regional_variants() {
        for (id, expected) in [
            (0x0409, UiLanguage::English),
            (0x080A, UiLanguage::Spanish),
            (0x0C0C, UiLanguage::French),
            (0x0804, UiLanguage::Chinese),
            (0x1004, UiLanguage::Chinese),
            (0x0439, UiLanguage::Hindi),
            (0x0401, UiLanguage::Arabic),
            (0x0C01, UiLanguage::Arabic),
            (0x0445, UiLanguage::Bengali),
            (0x0845, UiLanguage::Bengali),
            (0x0416, UiLanguage::Portuguese),
            (0x0816, UiLanguage::Portuguese),
            (0x0407, UiLanguage::English),
        ] {
            assert_eq!(ui_language_from_id(id), expected, "LANGID {id:04x}");
        }
    }
}

#[cfg(test)]
mod battery_menu_tests {
    use super::{
        BEST_EFFICIENCY, BEST_PERFORMANCE, Charge, PowerMode, classify_charge, known_seconds,
        power_mode, remaining_from_rate, until_full_seconds,
    };
    use windows::core::GUID;

    #[test]
    fn charging_at_full_stays_full_only_when_it_is_not_still_charging() {
        assert_eq!(classify_charge(true, true, 100), Charge::Charging);
        assert_eq!(classify_charge(true, false, 100), Charge::Full);
        assert_eq!(classify_charge(true, false, 80), Charge::Plugged);
        assert_eq!(classify_charge(false, false, 40), Charge::Discharging);
    }

    #[test]
    fn time_to_full_uses_the_gap_and_either_sign_of_the_rate() {
        // 20 mWh at 10 mW is two hours.
        assert_eq!(until_full_seconds(50, 30, -10), Some(7_200));
        assert_eq!(until_full_seconds(50, 30, 10), Some(7_200));
        assert_eq!(until_full_seconds(50, 50, -10), None);
        assert_eq!(until_full_seconds(50, 30, 0), None);
        assert_eq!(remaining_from_rate(30, 10), Some(10_800));
        assert_eq!(remaining_from_rate(30, -10), None);
        assert_eq!(known_seconds(90), Some(90));
        assert_eq!(known_seconds(0), None);
        assert_eq!(known_seconds(u32::MAX), None);
    }

    #[test]
    fn the_three_windows_power_modes_are_the_documented_guids() {
        assert_eq!(power_mode(BEST_EFFICIENCY), Some(PowerMode::BestEfficiency));
        assert_eq!(power_mode(GUID::from_u128(0)), Some(PowerMode::Balanced));
        assert_eq!(
            power_mode(BEST_PERFORMANCE),
            Some(PowerMode::BestPerformance)
        );
        assert_eq!(power_mode(GUID::from_u128(1)), None);
    }
}
