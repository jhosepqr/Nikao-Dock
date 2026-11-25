//! Menu-bar search. The Windows Search flyout always opens at the bottom of
//! the screen, so the magnifier opens this panel instead: a small card
//! under the bar, over Start-menu shortcuts and a few Settings pages.
//!
//! Nothing here runs while the panel is closed. There is no timer and no
//! watcher. The shortcut list is read on the UI thread when the panel opens,
//! then filtering is an in-memory scan of that list on each keystroke. The
//! window is hidden, not polled, until the next click.

use std::{
    cell::{Cell, RefCell},
    fs,
    path::Path,
    sync::atomic::{AtomicIsize, Ordering},
    time::{Duration, Instant},
};

use muelle_config::{Language, SearchMode};
use muelle_shell::{launcher, system};
use windows::{
    Win32::{
        Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
        Graphics::Gdi::{
            CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, CreateFontW, CreateSolidBrush, DEFAULT_CHARSET,
            DeleteObject, HBRUSH, HDC, HFONT, OUT_DEFAULT_PRECIS, SetBkColor, SetTextColor,
        },
        System::{Com::CoTaskMemFree, LibraryLoader::GetModuleHandleW},
        UI::{
            Controls::SetWindowTheme,
            Input::KeyboardAndMouse::{
                SetFocus, VIRTUAL_KEY, VK_DOWN, VK_ESCAPE, VK_RETURN, VK_UP,
            },
            Shell::{
                FOLDERID_CommonPrograms, FOLDERID_Programs, KF_FLAG_DEFAULT, SHGetKnownFolderPath,
            },
            WindowsAndMessaging::{
                CallWindowProcW, CreateWindowExW, DefWindowProcW, DestroyWindow, ES_AUTOHSCROLL,
                GWLP_WNDPROC, GetWindowTextW, HWND_TOPMOST, IDC_ARROW, IsWindowVisible,
                LoadCursorW, PostMessageW, RegisterClassW, SW_HIDE, SW_SHOWNA, SWP_NOACTIVATE,
                SendMessageW, SetWindowLongPtrW, SetWindowPos, SetWindowTextW, ShowWindow,
                WA_INACTIVE, WINDOW_EX_STYLE, WINDOW_STYLE, WM_ACTIVATE, WM_APP, WM_CHAR,
                WM_COMMAND, WM_CTLCOLOREDIT, WM_DESTROY, WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONDOWN,
                WM_LBUTTONUP, WM_MOUSEMOVE, WM_SETFOCUS, WM_SETFONT, WNDCLASSW, WNDPROC, WS_CHILD,
                WS_CLIPCHILDREN, WS_EX_NOREDIRECTIONBITMAP, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
                WS_POPUP, WS_TABSTOP, WS_VISIBLE,
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

const MAX_SHORTCUTS: usize = 4_000;
const MAX_DEPTH: u8 = 6;
const MAX_HITS: usize = 8;
/// Logical size of the query, placeholder and caret line.
const FIELD_FONT: f32 = 15.0;
const EM_GETSEL: u32 = 0x00B0;
/// A click that closes the panel by deactivating it is followed by the
/// magnifier's own click. Ignore a reopen that close just caused.
const REOPEN_GUARD: Duration = Duration::from_millis(250);
/// Posted on `WA_INACTIVE`; the panel closes only if it really lost the
/// foreground once activation has settled.
const CHECK_FOREGROUND: u32 = WM_APP + 42;

static MODE: AtomicIsize = AtomicIsize::new(0);
static EDIT_PROC: AtomicIsize = AtomicIsize::new(0);

thread_local! {
    static PANEL: RefCell<Option<Panel>> = const { RefCell::new(None) };
    static BUSY: Cell<bool> = const { Cell::new(false) };
    static CLOSED_AT: Cell<Option<Instant>> = const { Cell::new(None) };
}

#[derive(Clone, Copy)]
enum Kind {
    App,
    Setting,
}

struct Entry {
    title: String,
    /// Extra names that should match. Empty means `title` is the only name.
    aliases: Vec<String>,
    target: String,
    kind: Kind,
    /// Folder name under the Start menu, when it adds something.
    detail: String,
    /// `fold`ed names, one per alias (or just the title), so typing does not
    /// re-fold the whole catalog on every keystroke.
    folded: Vec<String>,
}

impl Entry {
    fn new(
        title: String,
        aliases: Vec<String>,
        target: String,
        kind: Kind,
        detail: String,
    ) -> Self {
        let folded = if aliases.is_empty() {
            vec![fold(&title)]
        } else {
            aliases.iter().map(|name| fold(name)).collect()
        };
        Self {
            title,
            aliases,
            target,
            kind,
            detail,
            folded,
        }
    }
}

#[derive(Clone)]
struct Hit {
    target: String,
    label: String,
    detail: String,
}

struct Panel {
    /// Dropped on close, before `gfx`, so the tree releases first.
    scene: Option<panel_glass::Scene>,
    gfx: Gfx,
    hwnd: HWND,
    edit: HWND,
    font: HFONT,
    font_scale: f32,
    edit_brush: HBRUSH,
    edit_text: COLORREF,
    field_color: COLORREF,
    tokens: GlassTokens,
    scale: f32,
    anchor: POINT,
    /// The search field around the edit control, in client pixels.
    field: RECT,
    width: i32,
    height: i32,
    row_top: i32,
    row_height: i32,
    entries: Vec<Entry>,
    hits: Vec<Hit>,
    selected: Option<usize>,
    /// What the EDIT holds and where its caret is (UTF-16 units). The EDIT
    /// cannot draw in a `WS_EX_NOREDIRECTIONBITMAP` window, so the field's
    /// text, placeholder and caret are drawn from these.
    query: String,
    caret: usize,
}

struct Words {
    placeholder: &'static str,
    files: &'static str,
    app: &'static str,
    settings: &'static str,
}

/// Remembered when settings are loaded. A click only reads this.
pub fn set_mode(mode: SearchMode) {
    let stored = match mode {
        SearchMode::Spotlight => 0,
        SearchMode::Windows => 1,
    };
    MODE.store(stored, Ordering::Relaxed);
}

/// Magnifier click. `anchor` is the button's centre, in screen pixels, on the
/// bottom edge of the menu bar.
pub fn activate(gfx: &Gfx, anchor: POINT) {
    match MODE.load(Ordering::Relaxed) {
        1 => launcher::open_search(),
        _ => toggle(gfx, anchor),
    }
}

fn toggle(gfx: &Gfx, anchor: POINT) {
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
        eprintln!("muelle: buscador: {error}");
        return;
    }
    // Cleared before the tree is attached, so the `EN_CHANGE` it raises does
    // not paint a frame that `render` below paints again.
    if let Some(edit) = PANEL.with(|slot| slot.borrow().as_ref().map(|panel| panel.edit)) {
        unsafe {
            let _ = SetWindowTextW(edit, w!(""));
        }
    }
    let Some((hwnd, edit)) = PANEL.with(|slot| {
        let mut guard = slot.borrow_mut();
        let panel = guard.as_mut()?;
        panel.gfx = gfx.clone();
        panel.tokens = GlassTokens::current();
        panel.entries = catalog();
        panel.hits.clear();
        panel.selected = None;
        panel.query.clear();
        panel.caret = 0;
        place(panel, anchor, 0);
        sync_edit(panel);
        crate::panel_theme::style_window(panel.hwnd, panel.tokens);
        if panel.scene.is_none() {
            let attached = panel_glass::Scene::attach(&panel.gfx, panel.hwnd);
            match attached {
                Ok(scene) => panel.scene = Some(scene),
                Err(error) => {
                    muelle_shell::log::line(&format!("buscador: vidrio: {error:#}"));
                    return None;
                }
            }
        }
        render(panel);
        Some((panel.hwnd, panel.edit))
    }) else {
        return;
    };
    unsafe {
        // SW_SHOWNA: activation comes once, from `take_focus`, not twice.
        let _ = ShowWindow(hwnd, SW_SHOWNA);
    }
    crate::panel_theme::take_focus(hwnd, edit, "buscador");
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
    // The tree and the catalog exist only while the panel is open.
    PANEL.with(|slot| {
        if let Some(panel) = slot.borrow_mut().as_mut() {
            panel.scene = None;
            panel.entries = Vec::new();
            panel.hits = Vec::new();
            panel.query = String::new();
            panel.selected = None;
            panel.caret = 0;
        }
    });
    if guard_reopen {
        CLOSED_AT.with(|closed| closed.set(Some(Instant::now())));
    }
    BUSY.with(|busy| busy.set(false));
}

fn create(gfx: &Gfx, anchor: POINT) -> Result<(), windows::core::Error> {
    let class = w!("MuelleSpotlight");
    let instance = unsafe { GetModuleHandleW(None)? };
    unsafe {
        let _ = RegisterClassW(&WNDCLASSW {
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
    let edit = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("EDIT"),
            w!(""),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(ES_AUTOHSCROLL as u32),
            0,
            0,
            10,
            10,
            Some(hwnd),
            None,
            Some(instance.into()),
            None,
        )
    };
    let edit = match edit {
        Ok(edit) => edit,
        Err(error) => {
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            return Err(error);
        }
    };
    unsafe {
        // Themed edits ignore the colour we return and paint an opaque bar.
        let _ = SetWindowTheme(edit, w!(""), w!(""));
        let previous =
            SetWindowLongPtrW(edit, GWLP_WNDPROC, edit_proc as *const () as usize as isize);
        EDIT_PROC.store(previous, Ordering::Relaxed);
    }
    let font = font_for(1.0);
    unsafe {
        let _ = SendMessageW(
            edit,
            WM_SETFONT,
            Some(WPARAM(font.0 as usize)),
            Some(LPARAM(0)),
        );
    }
    let panel = Panel {
        scene: None,
        gfx: gfx.clone(),
        hwnd,
        edit,
        font,
        font_scale: 0.0,
        edit_brush: unsafe { CreateSolidBrush(COLORREF(0x00FF_FFFF)) },
        edit_text: COLORREF(0),
        field_color: COLORREF(0),
        tokens: GlassTokens::for_theme(true, false),
        scale: 1.0,
        anchor,
        field: RECT::default(),
        width: 10,
        height: 10,
        row_top: 0,
        row_height: 1,
        entries: Vec::new(),
        hits: Vec::new(),
        selected: None,
        query: String::new(),
        caret: 0,
    };
    PANEL.with(|slot| *slot.borrow_mut() = Some(panel));
    Ok(())
}

fn font_for(scale: f32) -> HFONT {
    let height = -px(scale, 15.0);
    unsafe {
        CreateFontW(
            height,
            0,
            0,
            0,
            400,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            0,
            w!("Segoe UI"),
        )
    }
}

/// The EDIT stays a real control, so its font and opaque fill follow the
/// theme. Called when the panel opens; keystrokes do not rebuild the font.
fn sync_edit(panel: &mut Panel) {
    if panel.font_scale != panel.scale {
        let next = font_for(panel.scale);
        unsafe {
            let _ = SendMessageW(
                panel.edit,
                WM_SETFONT,
                Some(WPARAM(next.0 as usize)),
                Some(LPARAM(0)),
            );
            let _ = DeleteObject(panel.font.into());
        }
        panel.font = next;
        panel.font_scale = panel.scale;
    }
    let field = panel.tokens.field.colorref();
    let text = panel.tokens.text.colorref();
    if field != panel.field_color || text != panel.edit_text {
        let brush = unsafe { CreateSolidBrush(field) };
        unsafe {
            let _ = DeleteObject(panel.edit_brush.into());
        }
        panel.edit_brush = brush;
        panel.field_color = field;
        panel.edit_text = text;
    }
}

fn place(panel: &mut Panel, anchor: POINT, rows: usize) {
    panel.anchor = anchor;
    let monitor = system::monitor_at(anchor).or_else(system::primary_monitor);
    let scale = monitor.map(|monitor| monitor.scale).unwrap_or(1.0);
    panel.scale = scale;
    let pad = px(scale, 16.0);
    let edit_height = px(scale, 38.0);
    panel.row_top = pad + edit_height + px(scale, 10.0);
    panel.row_height = px(scale, 38.0).max(1);
    let width = px(scale, 520.0).max(280);
    let body = if rows == 0 {
        px(scale, 6.0)
    } else {
        rows as i32 * panel.row_height + px(scale, 14.0)
    };
    let height = panel.row_top + body;
    panel.width = width;
    panel.height = height;
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
        // A single-line EDIT does not centre its text vertically, so the
        // control is only one line tall and sits centred inside the field.
        let inset = px(scale, 10.0);
        let line = px(scale, 22.0).min(edit_height);
        let _ = SetWindowPos(
            panel.edit,
            None,
            pad + inset,
            pad + (edit_height - line) / 2,
            (width - pad * 2 - inset * 2).max(1),
            line,
            SWP_NOACTIVATE,
        );
    }
    panel.field = RECT {
        left: pad,
        top: pad,
        right: width - pad,
        bottom: pad + edit_height,
    };
}

fn refilter() {
    PANEL.with(|slot| {
        let mut guard = slot.borrow_mut();
        let Some(panel) = guard.as_mut() else {
            return;
        };
        let query = window_text(panel.edit);
        let hits = filter(&panel.entries, &query);
        let rows = hits.len();
        panel.selected = if hits.is_empty() { None } else { Some(0) };
        panel.hits = hits;
        panel.query = query;
        panel.caret = caret_of(panel.edit);
        let anchor = panel.anchor;
        place(panel, anchor, rows);
        render(panel);
    });
}

fn nudge(delta: isize) {
    PANEL.with(|slot| {
        let mut guard = slot.borrow_mut();
        let Some(panel) = guard.as_mut() else {
            return;
        };
        let len = panel.hits.len() as isize;
        if len == 0 {
            return;
        }
        let current = panel.selected.unwrap_or(0) as isize;
        panel.selected = Some(current.wrapping_add(delta).rem_euclid(len) as usize);
        render(panel);
    });
}

fn hover(y: i32) {
    PANEL.with(|slot| {
        let mut guard = slot.borrow_mut();
        let Some(panel) = guard.as_mut() else {
            return;
        };
        let next = row_at(panel, y);
        if next == panel.selected || next.is_none() {
            return;
        }
        panel.selected = next;
        render(panel);
    });
}

fn row_at(panel: &Panel, y: i32) -> Option<usize> {
    if panel.hits.is_empty() || y < panel.row_top {
        return None;
    }
    let index = (y - panel.row_top) / panel.row_height;
    (index >= 0 && (index as usize) < panel.hits.len()).then_some(index as usize)
}

fn launch_selected() {
    let target = PANEL.with(|slot| {
        let guard = slot.borrow();
        let panel = guard.as_ref()?;
        panel
            .hits
            .get(panel.selected?)
            .map(|hit| hit.target.clone())
    });
    if let Some(target) = target
        && launcher::open(&target).is_ok()
    {
        hide(false);
    }
}

fn launch_at(index: usize) {
    PANEL.with(|slot| {
        if let Some(panel) = slot.borrow_mut().as_mut()
            && index < panel.hits.len()
        {
            panel.selected = Some(index);
        }
    });
    launch_selected();
}

/// End of the EDIT's selection, which is where its caret sits.
fn caret_of(edit: HWND) -> usize {
    let mut end = 0u32;
    unsafe {
        let _ = SendMessageW(edit, EM_GETSEL, None, Some(LPARAM(&raw mut end as isize)));
    }
    end as usize
}

/// Redraws the field when the caret moved without a text change (arrows,
/// Home, End, a click). `try_borrow_mut`: the EDIT also gets messages sent
/// while the panel is borrowed (font and position changes).
fn sync_caret(edit: HWND) {
    PANEL.with(|slot| {
        let Ok(mut guard) = slot.try_borrow_mut() else {
            return;
        };
        let Some(panel) = guard.as_mut() else {
            return;
        };
        let caret = caret_of(edit);
        if caret != panel.caret {
            panel.caret = caret;
            render(panel);
        }
    });
}

fn window_text(hwnd: HWND) -> String {
    let mut buffer = [0u16; 240];
    let length = unsafe { GetWindowTextW(hwnd, &mut buffer) };
    let length = usize::try_from(length).unwrap_or(0).min(buffer.len());
    String::from_utf16_lossy(&buffer[..length])
}

fn px(scale: f32, value: f32) -> i32 {
    (value * scale).round() as i32
}

unsafe extern "system" fn panel_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_ERASEBKGND => LRESULT(1),
        WM_CTLCOLOREDIT => {
            let hdc = HDC(wparam.0 as *mut core::ffi::c_void);
            let brush = PANEL.with(|slot| {
                slot.borrow().as_ref().map(|panel| {
                    // Opaque text background: a transparent one leaves stale
                    // glyphs behind when the EDIT scrolls or deletes.
                    unsafe {
                        let _ = SetTextColor(hdc, panel.edit_text);
                        let _ = SetBkColor(hdc, panel.field_color);
                    }
                    panel.edit_brush
                })
            });
            LRESULT(brush.map(|brush| brush.0 as isize).unwrap_or(0))
        }
        WM_COMMAND => {
            let code = (wparam.0 >> 16) as u32;
            if code == EN_CHANGE {
                refilter();
            }
            LRESULT(0)
        }
        WM_SETFOCUS => {
            let edit = PANEL.with(|slot| slot.borrow().as_ref().map(|panel| panel.edit));
            if let Some(edit) = edit {
                unsafe {
                    let _ = SetFocus(Some(edit));
                }
            }
            LRESULT(0)
        }
        WM_KEYDOWN => {
            let _ = handle_key(wparam);
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            hover(hi_word(lparam));
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let y = hi_word(lparam);
            let index =
                PANEL.with(|slot| slot.borrow().as_ref().and_then(|panel| row_at(panel, y)));
            if let Some(index) = index {
                launch_at(index);
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
            PANEL.with(|slot| {
                if let Some(panel) = slot.borrow_mut().take() {
                    drop_panel(panel);
                }
            });
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

const EN_CHANGE: u32 = 768;

fn handle_key(wparam: WPARAM) -> bool {
    let key = VIRTUAL_KEY(wparam.0 as u16);
    if key == VK_ESCAPE {
        hide(false);
        return true;
    }
    if key == VK_RETURN {
        launch_selected();
        return true;
    }
    if key == VK_DOWN {
        nudge(1);
        return true;
    }
    if key == VK_UP {
        nudge(-1);
        return true;
    }
    false
}

fn hi_word(lparam: LPARAM) -> i32 {
    i32::from((lparam.0 as u32 >> 16) as u16 as i16)
}

unsafe extern "system" fn edit_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_KEYDOWN && handle_key(wparam) {
        return LRESULT(0);
    }
    // Enter, Esc and Tab were handled as keys; a single-line EDIT beeps on
    // their characters.
    if message == WM_CHAR && matches!(wparam.0, 0x0D | 0x1B | 0x09) {
        return LRESULT(0);
    }
    let previous = EDIT_PROC.load(Ordering::Relaxed);
    if previous == 0 {
        return LRESULT(0);
    }
    let previous: WNDPROC = unsafe { std::mem::transmute(previous) };
    let result = unsafe { CallWindowProcW(previous, hwnd, message, wparam, lparam) };
    if matches!(message, WM_KEYDOWN | WM_LBUTTONDOWN | WM_LBUTTONUP) {
        sync_caret(hwnd);
    }
    result
}

fn render(panel: &mut Panel) {
    let Some(scene) = panel.scene.clone() else {
        return;
    };
    let hits = panel.hits.clone();
    let selected = panel.selected;
    let query = panel.query.clone();
    let caret = panel.caret;
    let scale = panel.scale;
    let width = panel.width as f32;
    let row_top = panel.row_top as f32;
    let row_height = panel.row_height as f32;
    let field = panel.field;
    let canvas = Canvas {
        width,
        height: panel.height as f32,
        scale,
        tokens: panel.tokens,
    };
    if let Err(error) = scene.paint(&panel.gfx, &canvas, |surface| {
        let tokens = surface.tokens();
        let field_box = Round {
            x: field.left as f32,
            y: field.top as f32,
            w: (field.right - field.left) as f32,
            h: (field.bottom - field.top) as f32,
            radius: 10.0 * scale,
        };
        surface.control(tokens.field, tokens.field_border, field_box)?;
        let input = Line {
            x: field_box.x + (12.0 * scale).round(),
            top: field_box.y,
            height: field_box.h,
            max_width: (field_box.w - (24.0 * scale).round()).max(1.0),
        };
        let caret_x = if query.is_empty() {
            surface.text(
                words().placeholder,
                Line {
                    x: input.x + (4.0 * scale).round(),
                    max_width: input.max_width - (4.0 * scale).round(),
                    ..input
                },
                FIELD_FONT,
                false,
                tokens.secondary,
            )?;
            0.0
        } else {
            let drawn = surface.text(&query, input, FIELD_FONT, false, tokens.text)?;
            let units: Vec<u16> = query.encode_utf16().collect();
            if caret >= units.len() {
                drawn
            } else {
                let before = String::from_utf16_lossy(&units[..caret]);
                surface.measure(&before, FIELD_FONT, false)?.min(drawn)
            }
        };
        // Static caret: no blink, so no timer while the panel is open.
        let caret_h = (18.0 * scale).round();
        surface.fill(
            tokens.text,
            Round {
                x: input.x + caret_x.round(),
                y: field_box.y + ((field_box.h - caret_h) / 2.0).round(),
                w: (1.5 * scale).max(1.0),
                h: caret_h,
                radius: 0.0,
            },
        )?;
        let pad = (16.0 * scale).round();
        let text_x = pad + (10.0 * scale).round();
        for (index, hit) in hits.iter().enumerate() {
            let top = row_top + index as f32 * row_height;
            if selected == Some(index) {
                let row_box = Round {
                    x: pad,
                    y: top,
                    w: width - pad * 2.0,
                    h: row_height,
                    radius: 10.0 * scale,
                };
                surface.control(tokens.select, tokens.select_border, row_box)?;
            }
            let label_right = if hit.detail.is_empty() {
                width - text_x
            } else {
                width - (168.0 * scale).round()
            };
            surface.text(
                &hit.label,
                Line {
                    x: text_x,
                    top,
                    height: row_height,
                    max_width: (label_right - text_x).max(1.0),
                },
                14.5,
                selected == Some(index),
                tokens.text,
            )?;
            if !hit.detail.is_empty() {
                surface.text(
                    &hit.detail,
                    Line {
                        x: label_right,
                        top,
                        height: row_height,
                        max_width: (width - text_x - label_right).max(1.0),
                    },
                    12.5,
                    false,
                    tokens.secondary,
                )?;
            }
        }
        Ok(())
    }) {
        muelle_shell::log::line(&format!("buscador: vidrio: {error:#}"));
    }
}

fn drop_panel(panel: Panel) {
    unsafe {
        let _ = DeleteObject(panel.edit_brush.into());
        let _ = DeleteObject(panel.font.into());
    }
}

fn catalog() -> Vec<Entry> {
    let mut entries = Vec::new();
    for root in [
        known_folder(&FOLDERID_Programs),
        known_folder(&FOLDERID_CommonPrograms),
    ]
    .into_iter()
    .flatten()
    {
        walk(&root, 0, &mut entries);
    }
    entries.extend(settings_pages());
    entries
}

fn known_folder(id: &windows::core::GUID) -> Option<std::path::PathBuf> {
    unsafe {
        let raw = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None).ok()?;
        let path = raw.to_string().ok().map(std::path::PathBuf::from);
        CoTaskMemFree(Some(raw.as_ptr().cast()));
        path
    }
}

fn walk(dir: &Path, depth: u8, into: &mut Vec<Entry>) {
    if depth > MAX_DEPTH || into.len() >= MAX_SHORTCUTS {
        return;
    }
    let Ok(read) = fs::read_dir(dir) else {
        return;
    };
    for entry in read.flatten() {
        if into.len() >= MAX_SHORTCUTS {
            return;
        }
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            walk(&entry.path(), depth + 1, into);
            continue;
        }
        if !kind.is_file() {
            continue;
        }
        let path = entry.path();
        let Some(extension) = path.extension() else {
            continue;
        };
        if !extension.to_string_lossy().eq_ignore_ascii_case("lnk") {
            continue;
        }
        let Some(title) = path
            .file_stem()
            .map(|name| name.to_string_lossy().into_owned())
        else {
            continue;
        };
        if title.is_empty() {
            continue;
        }
        let target = path.to_string_lossy().into_owned();
        into.push(Entry::new(
            title,
            Vec::new(),
            target,
            Kind::App,
            parent_label(&path),
        ));
    }
}

fn parent_label(path: &Path) -> String {
    let Some(name) = path.parent().and_then(|parent| parent.file_name()) else {
        return String::new();
    };
    let name = name.to_string_lossy();
    let folded = fold(&name);
    if folded == "programs" || folded == "start menu" {
        String::new()
    } else {
        name.into_owned()
    }
}

fn settings_pages() -> Vec<Entry> {
    const PAGES: &[(&str, &[&str])] = &[
        (
            "ms-settings:",
            &[
                "Settings",
                "Configuración",
                "Ajustes",
                "Paramètres",
                "设置",
                "सेटिंग्स",
                "الإعدادات",
                "সেটিংস",
                "Configurações",
            ],
        ),
        ("ms-settings:bluetooth", &["Bluetooth"]),
        (
            "ms-settings:display",
            &["Display", "Pantalla", "Écran", "显示", "Tela", "الشاشة"],
        ),
        (
            "ms-settings:sound",
            &["Sound", "Sonido", "Son", "声音", "Som", "الصوت"],
        ),
        ("ms-settings:network-wifi", &["Wi-Fi", "Wifi", "WLAN"]),
        (
            "ms-settings:appsfeatures",
            &[
                "Apps",
                "Aplicaciones",
                "Applications",
                "应用",
                "Aplicativos",
            ],
        ),
        (
            "ms-settings:powersleep",
            &["Power", "Energía", "Alimentation", "电源", "Energia"],
        ),
        (
            "ms-settings:windowsupdate",
            &[
                "Windows Update",
                "Actualización",
                "Mise à jour",
                "Windows 更新",
                "Atualização",
            ],
        ),
    ];
    PAGES
        .iter()
        .filter_map(|(target, names)| {
            let title = (*names.first()?).to_owned();
            Some(Entry::new(
                title,
                names.iter().map(|name| (*name).to_owned()).collect(),
                (*target).to_owned(),
                Kind::Setting,
                String::new(),
            ))
        })
        .collect()
}

struct Rank {
    score: i32,
    index: usize,
    alias: usize,
}

/// Apps and Settings pages that match, best first, then one row that hands
/// the query to Explorer's indexed file search. Nothing is queried until
/// that row is opened.
fn filter(entries: &[Entry], query: &str) -> Vec<Hit> {
    let trimmed = query.trim();
    let folded = fold(trimmed);
    if folded.is_empty() {
        return Vec::new();
    }
    let mut ranked = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let mut best: Option<(i32, usize)> = None;
        for (alias, name) in entry.folded.iter().enumerate() {
            let Some(score) = folded_score(&folded, name) else {
                continue;
            };
            match best {
                Some((current, _)) if score <= current => {}
                _ => best = Some((score, alias)),
            }
        }
        if let Some((score, alias)) = best {
            ranked.push(Rank {
                score,
                index,
                alias,
            });
        }
    }
    ranked.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| left.index.cmp(&right.index))
    });
    ranked.truncate(MAX_HITS - 1);
    let mut hits: Vec<Hit> = ranked
        .into_iter()
        .map(|rank| {
            let entry = &entries[rank.index];
            let label = if entry.aliases.is_empty() {
                entry.title.clone()
            } else {
                entry.aliases[rank.alias].clone()
            };
            Hit {
                target: entry.target.clone(),
                label,
                detail: shown_detail(entry),
            }
        })
        .collect();
    hits.push(Hit {
        target: format!("search-ms:query={}", encode_query(trimmed)),
        label: format!("{} \u{201c}{trimmed}\u{201d}", words().files),
        detail: String::new(),
    });
    hits
}

/// Percent-encodes `text` for a `search-ms:` URI.
fn encode_query(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn shown_detail(entry: &Entry) -> String {
    if !entry.detail.is_empty() {
        return entry.detail.clone();
    }
    match entry.kind {
        Kind::App => words().app.to_owned(),
        Kind::Setting => words().settings.to_owned(),
    }
}

/// Higher is a closer match. `None` when `query` is blank or does not fit.
#[cfg(test)]
fn score(query: &str, candidate: &str) -> Option<i32> {
    folded_score(fold(query).trim(), &fold(candidate))
}

/// `score` for a query and candidate that are already `fold`ed and trimmed.
fn folded_score(query: &str, candidate: &str) -> Option<i32> {
    if query.is_empty() {
        return None;
    }
    if let Some(pos) = candidate.find(query) {
        let mut value = 10_000 - i32::try_from(pos).unwrap_or(0) * 8;
        if pos == 0 {
            value += 800;
        } else if candidate[..pos]
            .chars()
            .next_back()
            .is_some_and(|letter| !letter.is_alphanumeric())
        {
            value += 400;
        }
        return Some(value);
    }
    subsequence(query, candidate)
}

fn subsequence(query: &str, candidate: &str) -> Option<i32> {
    let mut wanted = query
        .chars()
        .filter(|letter| !letter.is_whitespace())
        .peekable();
    wanted.peek()?;
    let mut value = 0;
    let mut run = false;
    let mut previous: Option<char> = None;
    for letter in candidate.chars() {
        let starts_word = previous.is_none_or(char::is_whitespace);
        previous = Some(letter);
        if letter.is_whitespace() {
            run = false;
            continue;
        }
        if wanted.next_if_eq(&letter).is_some() {
            value += 12;
            if run {
                value += 18;
            }
            if starts_word {
                value += 24;
            }
            run = true;
        } else {
            run = false;
        }
    }
    wanted.peek().is_none().then_some(value)
}

fn fold(text: &str) -> String {
    text.chars()
        .flat_map(char::to_lowercase)
        .map(|letter| match letter {
            'á' | 'à' | 'ä' | 'â' | 'ã' => 'a',
            'é' | 'è' | 'ë' | 'ê' => 'e',
            'í' | 'ì' | 'ï' | 'î' => 'i',
            'ó' | 'ò' | 'ö' | 'ô' | 'õ' => 'o',
            'ú' | 'ù' | 'ü' | 'û' => 'u',
            'ñ' => 'n',
            'ç' => 'c',
            other => other,
        })
        .collect()
}

fn words() -> Words {
    match strings::language() {
        Language::Es => Words {
            placeholder: "Buscar aplicaciones y ajustes",
            files: "Buscar archivos:",
            app: "Aplicación",
            settings: "Ajustes",
        },
        Language::Fr => Words {
            placeholder: "Rechercher des applications et des réglages",
            files: "Rechercher des fichiers :",
            app: "Application",
            settings: "Réglages",
        },
        Language::Zh => Words {
            placeholder: "搜索应用和设置",
            files: "搜索文件：",
            app: "应用",
            settings: "设置",
        },
        Language::Hi => Words {
            placeholder: "ऐप और सेटिंग खोजें",
            files: "फ़ाइलें खोजें:",
            app: "ऐप",
            settings: "सेटिंग्स",
        },
        Language::Ar => Words {
            placeholder: "ابحث عن التطبيقات والإعدادات",
            files: "البحث في الملفات:",
            app: "تطبيق",
            settings: "الإعدادات",
        },
        Language::Bn => Words {
            placeholder: "অ্যাপ ও সেটিংস খুঁজুন",
            files: "ফাইল খুঁজুন:",
            app: "অ্যাপ",
            settings: "সেটিংস",
        },
        Language::Pt => Words {
            placeholder: "Procurar aplicações e definições",
            files: "Procurar ficheiros:",
            app: "Aplicação",
            settings: "Definições",
        },
        Language::En | Language::Auto => Words {
            placeholder: "Search apps and settings",
            files: "Search files for",
            app: "Application",
            settings: "Settings",
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(title: &str) -> Entry {
        Entry::new(
            title.to_owned(),
            Vec::new(),
            title.to_owned(),
            Kind::App,
            String::new(),
        )
    }

    #[test]
    fn a_blank_query_matches_nothing() {
        assert!(score("   ", "Visual Studio Code").is_none());
        assert!(filter(&[app("Code")], "  ").is_empty());
    }

    #[test]
    fn the_last_row_searches_files_even_without_an_app_match() {
        let hits = filter(&[app("Code")], " año 2026 ");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].target, "search-ms:query=a%C3%B1o%202026");
    }

    #[test]
    fn a_prefix_beats_a_later_substring_and_accents_fold() {
        let code = score("code", "Code");
        let visual = score("code", "Visual Studio Code");
        assert!(code > visual);
        assert!(score("configuracion", "Configuración").is_some());
        assert!(score("zzz", "Code").is_none());
    }

    #[test]
    fn initials_match_in_order_and_only_the_closest_eight_are_kept() {
        assert!(score("vsc", "Visual Studio Code").is_some());
        let mut entries: Vec<Entry> = (0..12).map(|index| app(&format!("App {index}"))).collect();
        entries.push(app("Notes"));
        let hits = filter(&entries, "app");
        assert_eq!(hits.len(), MAX_HITS);
        let (apps, files) = hits.split_at(MAX_HITS - 1);
        assert!(apps.iter().all(|hit| hit.label.starts_with("App")));
        assert_eq!(files[0].target, "search-ms:query=app");
    }
}
