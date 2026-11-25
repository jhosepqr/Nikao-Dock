//! Battery menu. The battery icon used to send Win+A, which opens Quick
//! Settings at the bottom of the screen. It now opens this panel under the
//! icon: charge, time, energy saver and power mode.
//!
//! Nothing here runs while the panel is closed. There is no timer. Windows
//! power broadcasts are ignored until the panel is visible, and the reading
//! is taken again only then.

use std::{
    cell::{Cell, RefCell},
    time::{Duration, Instant},
};

use muelle_config::Language;
use muelle_shell::{
    launcher,
    system::{self, BatteryMenu, Charge, PowerMode},
};
use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM},
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Input::KeyboardAndMouse::{VIRTUAL_KEY, VK_ESCAPE, VK_RETURN},
            WindowsAndMessaging::{
                CS_DROPSHADOW, CreateWindowExW, DefWindowProcW, HWND_TOPMOST, IDC_ARROW,
                IsWindowVisible, LoadCursorW, PBT_APMPOWERSTATUSCHANGE, PostMessageW,
                RegisterClassW, SW_HIDE, SW_SHOWNA, SWP_NOACTIVATE, SetWindowPos, ShowWindow,
                WA_INACTIVE, WM_ACTIVATE, WM_APP, WM_DESTROY, WM_ERASEBKGND, WM_KEYDOWN,
                WM_LBUTTONUP, WM_MOUSEMOVE, WM_POWERBROADCAST, WNDCLASSW, WS_CLIPCHILDREN,
                WS_EX_NOREDIRECTIONBITMAP, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
            },
        },
    },
    core::w,
};

use crate::{
    gfx::Gfx,
    panel_glass::{self, Canvas, Line, Round},
    panel_theme::GlassTokens,
    strings,
};

/// A click that closes the panel by deactivating it is followed by the
/// battery icon's own click. Ignore a reopen that close just caused.
const REOPEN_GUARD: Duration = Duration::from_millis(250);
const SETTINGS: &str = "ms-settings:batterysaver";
/// Posted on `WA_INACTIVE`; the panel closes only if it really lost the
/// foreground once activation has settled.
const CHECK_FOREGROUND: u32 = WM_APP + 41;

thread_local! {
    static PANEL: RefCell<Option<Panel>> = const { RefCell::new(None) };
    static BUSY: Cell<bool> = const { Cell::new(false) };
    static CLOSED_AT: Cell<Option<Instant>> = const { Cell::new(None) };
}

#[derive(Clone)]
struct Lines {
    percent: Option<String>,
    rows: Vec<String>,
    link: String,
}

struct Panel {
    /// Dropped on close, before `gfx`, so the tree releases first.
    scene: Option<panel_glass::Scene>,
    gfx: Gfx,
    hwnd: HWND,
    tokens: GlassTokens,
    anchor: POINT,
    lines: Lines,
    scale: f32,
    width: i32,
    height: i32,
    pad: i32,
    percent_h: i32,
    row_h: i32,
    link_top: i32,
    link_height: i32,
    hot: bool,
}

struct Words {
    charging: &'static str,
    discharging: &'static str,
    full: &'static str,
    plugged: &'static str,
    remaining_before: &'static str,
    remaining_after: &'static str,
    until_full_before: &'static str,
    until_full_after: &'static str,
    saver_on: &'static str,
    saver_off: &'static str,
    efficiency: &'static str,
    balanced: &'static str,
    performance: &'static str,
    plan_before: &'static str,
    settings: &'static str,
    none: &'static str,
    hours: &'static str,
    minutes: &'static str,
}

/// Battery-icon click. `anchor` is the button's centre, in screen pixels, on
/// the bottom edge of the menu bar.
pub fn activate(gfx: &Gfx, anchor: POINT) {
    if visible() {
        hide(false);
        return;
    }
    let just_closed =
        CLOSED_AT.with(|closed| closed.get().is_some_and(|at| at.elapsed() < REOPEN_GUARD));
    if just_closed {
        CLOSED_AT.with(|closed| closed.set(None));
        return;
    }
    show(gfx, anchor);
}

fn visible() -> bool {
    PANEL.with(|slot| {
        slot.borrow()
            .as_ref()
            .is_some_and(|panel| unsafe { IsWindowVisible(panel.hwnd).as_bool() })
    })
}

fn show(gfx: &Gfx, anchor: POINT) {
    if PANEL.with(|slot| slot.borrow().is_none())
        && let Err(error) = create(gfx, anchor)
    {
        eprintln!("muelle: batería: {error}");
        return;
    }
    let Some(hwnd) = PANEL.with(|slot| {
        let mut guard = slot.borrow_mut();
        let panel = guard.as_mut()?;
        panel.gfx = gfx.clone();
        panel.tokens = GlassTokens::current();
        panel.anchor = anchor;
        panel.hot = false;
        panel.lines = lines(system::battery_menu().as_ref(), &words());
        place(panel);
        crate::panel_theme::style_window(panel.hwnd, panel.tokens);
        if panel.scene.is_none() {
            let attached = panel_glass::Scene::attach(&panel.gfx, panel.hwnd);
            match attached {
                Ok(scene) => panel.scene = Some(scene),
                Err(error) => {
                    muelle_shell::log::line(&format!("batería: vidrio: {error:#}"));
                    return None;
                }
            }
        }
        render(panel);
        Some(panel.hwnd)
    }) else {
        return;
    };
    // SW_SHOWNA: activation comes once, from `take_focus`, not twice.
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOWNA);
    }
    crate::panel_theme::take_focus(hwnd, hwnd, "batería");
}

fn closes_on_deactivate(hwnd: HWND) {
    if unsafe { IsWindowVisible(hwnd).as_bool() } && !crate::panel_theme::is_foreground(hwnd) {
        hide(true);
    }
}

fn hide(guard_reopen: bool) {
    if BUSY.with(Cell::get) {
        return;
    }
    BUSY.with(|busy| busy.set(true));
    let hwnd = PANEL.with(|slot| slot.borrow().as_ref().map(|panel| panel.hwnd));
    if let Some(hwnd) = hwnd {
        unsafe {
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
    }
    // The tree exists only while the panel is open.
    PANEL.with(|slot| {
        if let Some(panel) = slot.borrow_mut().as_mut() {
            panel.scene = None;
        }
    });
    if guard_reopen {
        CLOSED_AT.with(|closed| closed.set(Some(Instant::now())));
    }
    BUSY.with(|busy| busy.set(false));
}

fn refresh_if_visible() {
    PANEL.with(|slot| {
        let mut guard = slot.borrow_mut();
        let Some(panel) = guard.as_mut() else {
            return;
        };
        if unsafe { !IsWindowVisible(panel.hwnd).as_bool() } {
            return;
        }
        panel.lines = lines(system::battery_menu().as_ref(), &words());
        place(panel);
        render(panel);
    });
}

fn create(gfx: &Gfx, anchor: POINT) -> Result<(), windows::core::Error> {
    let class = w!("MuelleBattery");
    let instance = unsafe { GetModuleHandleW(None)? };
    unsafe {
        let _ = RegisterClassW(&WNDCLASSW {
            style: CS_DROPSHADOW,
            lpfnWndProc: Some(panel_proc),
            hInstance: instance.into(),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            lpszClassName: class,
            ..Default::default()
        });
    }
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            class,
            w!("Nikao Dock"),
            WS_POPUP | WS_CLIPCHILDREN,
            anchor.x,
            anchor.y,
            10,
            10,
            None,
            None,
            Some(instance.into()),
            None,
        )?
    };
    let panel = Panel {
        scene: None,
        gfx: gfx.clone(),
        hwnd,
        tokens: GlassTokens::for_theme(true, false),
        anchor,
        lines: lines(None, &words()),
        scale: 1.0,
        width: 10,
        height: 10,
        pad: 16,
        percent_h: 0,
        row_h: 24,
        link_top: 0,
        link_height: 1,
        hot: false,
    };
    PANEL.with(|slot| *slot.borrow_mut() = Some(panel));
    Ok(())
}

fn place(panel: &mut Panel) {
    let anchor = panel.anchor;
    let monitor = system::monitor_at(anchor).or_else(system::primary_monitor);
    let scale = monitor.map(|monitor| monitor.scale).unwrap_or(1.0);
    let pad = px(scale, 16.0);
    let percent_h = if panel.lines.percent.is_some() {
        px(scale, 40.0)
    } else {
        0
    };
    let row_h = px(scale, 24.0).max(1);
    let link_h = px(scale, 36.0).max(1);
    let gap = px(scale, 8.0);
    let width = px(scale, 300.0).max(220);
    let height = pad + percent_h + panel.lines.rows.len() as i32 * row_h + gap + link_h + pad;
    panel.scale = scale;
    panel.width = width;
    panel.height = height;
    panel.pad = pad;
    panel.percent_h = percent_h;
    panel.row_h = row_h;
    panel.link_height = link_h;
    panel.link_top = pad + percent_h + panel.lines.rows.len() as i32 * row_h + gap;
    let (mut left, mut top) = (anchor.x - width / 2, anchor.y + px(scale, 8.0));
    if let Some(monitor) = monitor {
        let screen = monitor.screen;
        left = left.clamp(screen.left + 8, (screen.right - width - 8).max(screen.left));
        let bottom_limit = screen.bottom - height - 8;
        if top > bottom_limit {
            top = bottom_limit.max(screen.top);
        }
    }
    unsafe {
        let _ = SetWindowPos(
            panel.hwnd,
            Some(HWND_TOPMOST),
            left,
            top,
            width,
            height,
            SWP_NOACTIVATE,
        );
    }
}

fn open_settings() {
    if launcher::open(SETTINGS).is_ok() {
        hide(false);
    }
}

fn px(scale: f32, value: f32) -> i32 {
    (value * scale).round() as i32
}

fn hi_word(lparam: LPARAM) -> i32 {
    i32::from((lparam.0 as u32 >> 16) as u16 as i16)
}

unsafe extern "system" fn panel_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_ERASEBKGND => LRESULT(1),
        WM_POWERBROADCAST => {
            if wparam.0 as u32 == PBT_APMPOWERSTATUSCHANGE {
                refresh_if_visible();
            }
            LRESULT(1)
        }
        WM_KEYDOWN => {
            let key = VIRTUAL_KEY(wparam.0 as u16);
            if key == VK_ESCAPE {
                hide(false);
            } else if key == VK_RETURN {
                open_settings();
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let y = hi_word(lparam);
            PANEL.with(|slot| {
                let mut guard = slot.borrow_mut();
                let Some(panel) = guard.as_mut() else {
                    return;
                };
                let hot = y >= panel.link_top && y < panel.link_top + panel.link_height;
                if hot == panel.hot {
                    return;
                }
                panel.hot = hot;
                render(panel);
            });
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let y = hi_word(lparam);
            let open = PANEL.with(|slot| {
                slot.borrow().as_ref().is_some_and(|panel| {
                    y >= panel.link_top && y < panel.link_top + panel.link_height
                })
            });
            if open {
                open_settings();
            }
            LRESULT(0)
        }
        WM_ACTIVATE => {
            if (wparam.0 as u32 & 0xffff) == WA_INACTIVE {
                // Activation can bounce while the panel opens; decide once
                // the message queue has settled.
                unsafe {
                    let _ = PostMessageW(Some(hwnd), CHECK_FOREGROUND, WPARAM(0), LPARAM(0));
                }
            }
            LRESULT(0)
        }
        CHECK_FOREGROUND => {
            closes_on_deactivate(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            PANEL.with(|slot| slot.borrow_mut().take());
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

fn render(panel: &mut Panel) {
    let Some(scene) = panel.scene.clone() else {
        return;
    };
    let lines = panel.lines.clone();
    let hot = panel.hot;
    let pad = panel.pad as f32;
    let percent_h = panel.percent_h as f32;
    let row_h = panel.row_h as f32;
    let link_top = panel.link_top as f32;
    let link_height = panel.link_height as f32;
    let width = panel.width as f32;
    let scale = panel.scale;
    let canvas = Canvas {
        width,
        height: panel.height as f32,
        scale,
        tokens: panel.tokens,
    };
    if let Err(error) = scene.paint(&panel.gfx, &canvas, |surface| {
        let tokens = surface.tokens();
        let mut top = pad;
        if let Some(percent) = &lines.percent {
            surface.text(
                percent,
                Line {
                    x: pad,
                    top,
                    height: percent_h,
                    max_width: width - pad * 2.0,
                },
                22.0,
                true,
                tokens.text,
            )?;
            top += percent_h;
        }
        for (index, row) in lines.rows.iter().enumerate() {
            // The charge state reads first; the details are secondary.
            let color = if index == 0 {
                tokens.text
            } else {
                tokens.secondary
            };
            surface.text(
                row,
                Line {
                    x: pad,
                    top: top + index as f32 * row_h,
                    height: row_h,
                    max_width: width - pad * 2.0,
                },
                13.0,
                false,
                color,
            )?;
        }
        if hot {
            surface.fill(
                tokens.select,
                Round {
                    x: pad / 2.0,
                    y: link_top,
                    w: width - pad,
                    h: link_height,
                    radius: 6.0 * scale,
                },
            )?;
        }
        surface.text(
            &lines.link,
            Line {
                x: pad,
                top: link_top,
                height: link_height,
                max_width: width - pad * 2.0,
            },
            13.0,
            false,
            tokens.link,
        )?;
        Ok(())
    }) {
        muelle_shell::log::line(&format!("batería: vidrio: {error:#}"));
    }
}

fn lines(menu: Option<&BatteryMenu>, words: &Words) -> Lines {
    let Some(menu) = menu else {
        return Lines {
            percent: None,
            rows: vec![words.none.to_owned()],
            link: words.settings.to_owned(),
        };
    };
    let mut rows = vec![charge_label(menu.charge, words).to_owned()];
    if let Some(text) = time_line(menu, words) {
        rows.push(text);
    }
    rows.push(
        if menu.saver {
            words.saver_on
        } else {
            words.saver_off
        }
        .to_owned(),
    );
    if let Some(text) = mode_line(menu, words) {
        rows.push(text);
    }
    Lines {
        percent: Some(format!("{}%", menu.percent)),
        rows,
        link: words.settings.to_owned(),
    }
}

fn charge_label(charge: Charge, words: &Words) -> &'static str {
    match charge {
        Charge::Charging => words.charging,
        Charge::Discharging => words.discharging,
        Charge::Full => words.full,
        Charge::Plugged => words.plugged,
    }
}

fn time_line(menu: &BatteryMenu, words: &Words) -> Option<String> {
    match menu.charge {
        Charge::Charging => menu.until_full.map(|seconds| {
            format!(
                "{}{}{}",
                words.until_full_before,
                duration(seconds, words),
                words.until_full_after
            )
        }),
        Charge::Discharging => menu.remaining.map(|seconds| {
            format!(
                "{}{}{}",
                words.remaining_before,
                duration(seconds, words),
                words.remaining_after
            )
        }),
        Charge::Full | Charge::Plugged => None,
    }
}

fn mode_line(menu: &BatteryMenu, words: &Words) -> Option<String> {
    match menu.mode {
        Some(PowerMode::BestEfficiency) => Some(words.efficiency.to_owned()),
        Some(PowerMode::Balanced) => Some(words.balanced.to_owned()),
        Some(PowerMode::BestPerformance) => Some(words.performance.to_owned()),
        None if menu.plan.is_empty() => None,
        None => Some(format!("{}{}", words.plan_before, menu.plan)),
    }
}

fn duration(seconds: u32, words: &Words) -> String {
    let mut minutes = seconds / 60;
    if minutes == 0 {
        minutes = 1;
    }
    let hours = minutes / 60;
    let minutes = minutes % 60;
    if hours == 0 {
        format!("{minutes} {}", words.minutes)
    } else if minutes == 0 {
        format!("{hours} {}", words.hours)
    } else {
        format!("{hours} {} {minutes} {}", words.hours, words.minutes)
    }
}

fn words() -> Words {
    words_for(strings::language())
}

fn words_for(language: Language) -> Words {
    match language {
        Language::Es => Words {
            charging: "Cargando",
            discharging: "En batería",
            full: "Cargada",
            plugged: "Conectada, sin cargar",
            remaining_before: "Quedan ",
            remaining_after: "",
            until_full_before: "",
            until_full_after: " para completar",
            saver_on: "Ahorro de energía activado",
            saver_off: "Ahorro de energía desactivado",
            efficiency: "Modo: Mejor eficiencia energética",
            balanced: "Modo: Equilibrado",
            performance: "Modo: Mejor rendimiento",
            plan_before: "Plan: ",
            settings: "Configuración de la batería",
            none: "Sin batería",
            hours: "h",
            minutes: "min",
        },
        Language::Fr => Words {
            charging: "En charge",
            discharging: "Sur batterie",
            full: "Chargée",
            plugged: "Branchée, sans charge",
            remaining_before: "Reste ",
            remaining_after: "",
            until_full_before: "",
            until_full_after: " pour être pleine",
            saver_on: "Économiseur d'énergie activé",
            saver_off: "Économiseur d'énergie désactivé",
            efficiency: "Mode : Meilleure efficacité",
            balanced: "Mode : Équilibré",
            performance: "Mode : Meilleures performances",
            plan_before: "Plan : ",
            settings: "Réglages de la batterie",
            none: "Pas de batterie",
            hours: "h",
            minutes: "min",
        },
        Language::Zh => Words {
            charging: "正在充电",
            discharging: "使用电池",
            full: "已充满",
            plugged: "已接通电源，未在充电",
            remaining_before: "剩余 ",
            remaining_after: "",
            until_full_before: "",
            until_full_after: "后充满",
            saver_on: "节能模式已开启",
            saver_off: "节能模式已关闭",
            efficiency: "电源模式：最佳能效",
            balanced: "电源模式：平衡",
            performance: "电源模式：最佳性能",
            plan_before: "计划：",
            settings: "电池设置",
            none: "没有电池",
            hours: "小时",
            minutes: "分钟",
        },
        Language::Hi => Words {
            charging: "चार्ज हो रही है",
            discharging: "बैटरी पर",
            full: "पूरी चार्ज",
            plugged: "प्लग इन, चार्ज नहीं हो रही",
            remaining_before: "",
            remaining_after: " शेष",
            until_full_before: "पूरी होने में ",
            until_full_after: "",
            saver_on: "ऊर्जा बचतकर्ता चालू",
            saver_off: "ऊर्जा बचतकर्ता बंद",
            efficiency: "मोड: सर्वोत्तम ऊर्जा दक्षता",
            balanced: "मोड: संतुलित",
            performance: "मोड: सर्वोत्तम प्रदर्शन",
            plan_before: "योजना: ",
            settings: "बैटरी सेटिंग्स",
            none: "कोई बैटरी नहीं",
            hours: "घंटे",
            minutes: "मिनट",
        },
        Language::Ar => Words {
            charging: "يشحن",
            discharging: "على البطارية",
            full: "مشحونة بالكامل",
            plugged: "موصول، دون شحن",
            remaining_before: "المتبقي ",
            remaining_after: "",
            until_full_before: "",
            until_full_after: " حتى الامتلاء",
            saver_on: "موفر الطاقة قيد التشغيل",
            saver_off: "موفر الطاقة متوقف",
            efficiency: "الوضع: أفضل كفاءة للطاقة",
            balanced: "الوضع: متوازن",
            performance: "الوضع: أفضل أداء",
            plan_before: "الخطة: ",
            settings: "إعدادات البطارية",
            none: "لا توجد بطارية",
            hours: "س",
            minutes: "د",
        },
        Language::Bn => Words {
            charging: "চার্জ হচ্ছে",
            discharging: "ব্যাটারিতে",
            full: "পূর্ণ চার্জ",
            plugged: "প্লাগ ইন, চার্জ হচ্ছে না",
            remaining_before: "বাকি ",
            remaining_after: "",
            until_full_before: "পূর্ণ হতে ",
            until_full_after: "",
            saver_on: "শক্তি সাশ্রয় চালু",
            saver_off: "শক্তি সাশ্রয় বন্ধ",
            efficiency: "মোড: সেরা শক্তি দক্ষতা",
            balanced: "মোড: সুষম",
            performance: "মোড: সেরা কর্মক্ষমতা",
            plan_before: "পরিকল্পনা: ",
            settings: "ব্যাটারি সেটিংস",
            none: "কোনো ব্যাটারি নেই",
            hours: "ঘণ্টা",
            minutes: "মিনিট",
        },
        Language::Pt => Words {
            charging: "A carregar",
            discharging: "Na bateria",
            full: "Totalmente carregada",
            plugged: "Ligada, sem carregar",
            remaining_before: "Restam ",
            remaining_after: "",
            until_full_before: "",
            until_full_after: " até ficar cheia",
            saver_on: "Poupança de energia ligada",
            saver_off: "Poupança de energia desligada",
            efficiency: "Modo: Melhor eficiência energética",
            balanced: "Modo: Equilibrado",
            performance: "Modo: Melhor desempenho",
            plan_before: "Plano: ",
            settings: "Definições da bateria",
            none: "Sem bateria",
            hours: "h",
            minutes: "min",
        },
        Language::En | Language::Auto => Words {
            charging: "Charging",
            discharging: "On battery",
            full: "Fully charged",
            plugged: "Plugged in, not charging",
            remaining_before: "",
            remaining_after: " remaining",
            until_full_before: "",
            until_full_after: " until full",
            saver_on: "Energy saver on",
            saver_off: "Energy saver off",
            efficiency: "Mode: Best power efficiency",
            balanced: "Mode: Balanced",
            performance: "Mode: Best performance",
            plan_before: "Plan: ",
            settings: "Battery settings",
            none: "No battery",
            hours: "h",
            minutes: "min",
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn charging() -> BatteryMenu {
        BatteryMenu {
            percent: 80,
            charge: Charge::Charging,
            saver: false,
            remaining: Some(9_999),
            until_full: Some(2_700),
            mode: Some(PowerMode::BestPerformance),
            plan: "Equilibrado".to_owned(),
        }
    }

    #[test]
    fn a_charging_battery_names_the_time_to_full_and_the_power_mode() {
        let text = lines(Some(&charging()), &words_for(Language::Es));
        assert_eq!(text.percent.as_deref(), Some("80%"));
        assert!(text.rows.iter().any(|row| row == "Cargando"));
        assert!(text.rows.iter().any(|row| row == "45 min para completar"));
        assert!(
            text.rows
                .iter()
                .any(|row| row == "Ahorro de energía desactivado")
        );
        assert!(text.rows.iter().any(|row| row == "Modo: Mejor rendimiento"));
        assert!(text.rows.iter().all(|row| !row.contains("Equilibrado")));
        assert_eq!(text.link, "Configuración de la batería");
    }

    #[test]
    fn time_remaining_and_a_plan_name_fill_in_when_the_slider_is_unknown() {
        let menu = BatteryMenu {
            percent: 40,
            charge: Charge::Discharging,
            saver: true,
            remaining: Some(8_100),
            until_full: Some(10),
            mode: None,
            plan: "Balanced".to_owned(),
        };
        let text = lines(Some(&menu), &words_for(Language::En));
        assert!(text.rows.iter().any(|row| row == "2 h 15 min remaining"));
        assert!(text.rows.iter().any(|row| row == "Energy saver on"));
        assert!(text.rows.iter().any(|row| row == "Plan: Balanced"));
        assert!(text.rows.iter().all(|row| !row.contains("until full")));
    }

    #[test]
    fn a_desktop_without_a_battery_still_offers_the_settings_link() {
        let text = lines(None, &words_for(Language::En));
        assert!(text.percent.is_none());
        assert_eq!(text.rows, ["No battery"]);
        assert_eq!(text.link, "Battery settings");
    }
}
