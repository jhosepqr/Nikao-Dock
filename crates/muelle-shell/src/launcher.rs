use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

use thiserror::Error;
use windows::{
    Win32::{
        Foundation::{CloseHandle, HANDLE, HWND, LPARAM, RECT, WAIT_OBJECT_0, WPARAM},
        Graphics::Gdi::{MONITOR_DEFAULTTONULL, MonitorFromRect},
        Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation},
        System::{
            Com::{
                CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
                CoUninitialize,
            },
            Diagnostics::Debug::{SEM_FAILCRITICALERRORS, SEM_NOOPENFILEERRORBOX, SetErrorMode},
            Threading::{OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION},
        },
        UI::{
            Accessibility::{
                CUIAutomation, HWINEVENTHOOK, IUIAutomation, IUIAutomationInvokePattern,
                IUIAutomationLegacyIAccessiblePattern, SetWinEventHook, TreeScope_Descendants,
                UIA_AutomationIdPropertyId, UIA_ClassNamePropertyId, UIA_InvokePatternId,
                UIA_LegacyIAccessiblePatternId, UIA_NamePropertyId, UnhookWinEvent,
            },
            Input::KeyboardAndMouse::{
                INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_KEYUP,
                SendInput, VIRTUAL_KEY, VK_A, VK_B, VK_D, VK_ESCAPE, VK_LCONTROL, VK_LWIN, VK_N,
                VK_RETURN, VK_S, VK_TAB, VK_V, VK_W,
            },
            Shell::{SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW, ShellExecuteExW},
            WindowsAndMessaging::{
                ASFW_ANY, AllowSetForegroundWindow, DispatchMessageW, EVENT_OBJECT_CLOAKED,
                EVENT_OBJECT_FOCUS, EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_SHOW,
                EVENT_OBJECT_UNCLOAKED, EVENT_SYSTEM_FOREGROUND, FindWindowW, GUITHREADINFO,
                GWL_EXSTYLE, GetForegroundWindow, GetGUIThreadInfo, GetShellWindow, GetWindowLongW,
                GetWindowRect, GetWindowThreadProcessId, IsChild, IsIconic, IsWindow,
                IsWindowVisible, MSG, MWMO_INPUTAVAILABLE, MsgWaitForMultipleObjectsEx, PM_REMOVE,
                PeekMessageW, PostMessageW, QS_ALLINPUT, SMTO_ABORTIFHUNG, SW_MINIMIZE, SW_RESTORE,
                SW_SHOWNORMAL, SWP_ASYNCWINDOWPOS, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER,
                SWP_NOSIZE, SWP_NOZORDER, SendMessageTimeoutW, SetForegroundWindow, SetWindowPos,
                ShowWindowAsync, SwitchToThisWindow, WINEVENT_OUTOFCONTEXT, WM_CLOSE, WM_NULL,
                WS_EX_TOPMOST,
            },
        },
    },
    core::{HSTRING, PCWSTR, w},
};

#[derive(Debug, Error)]
#[error("no se pudo abrir `{id}` (código {code} del shell)")]
pub struct LaunchError {
    id: String,
    code: isize,
}

/// The dock never takes the focus, so Windows may refuse the foreground to the
/// app it starts and open it behind other windows. The click that asked for it
/// was the dock's, which lets the dock hand that right on.
pub fn allow_foreground() {
    // SAFETY: No pointers; failure only means Windows keeps its default.
    unsafe {
        let _ = AllowSetForegroundWindow(ASFW_ANY);
    }
}

pub fn open(id: &str) -> Result<(), LaunchError> {
    shell_execute(w!("open"), id, None)
}

/// Opens `id` as administrator, with the UAC prompt, as the taskbar's
/// "Run as administrator" and Ctrl+Shift+click do.
pub fn open_as_admin(id: &str) -> Result<(), LaunchError> {
    shell_execute(w!("runas"), id, None)
}

fn shell_execute(verb: PCWSTR, id: &str, arguments: Option<&str>) -> Result<(), LaunchError> {
    allow_foreground();
    // SAFETY: Suppresses system modal error popups (e.g. broken shell extensions or missing ordinals).
    unsafe {
        let _ = SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOOPENFILEERRORBOX);
    }
    let file = HSTRING::from(id);
    let arguments = arguments.map(HSTRING::from);
    let mut request = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        // A disposable caller must finish activation (including DDE) before
        // its thread exits. This does not wait for the launched app to close.
        // SEE_MASK_FLAG_NO_UI suppresses shell error dialogs without hiding UAC prompts.
        fMask: SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
        lpVerb: verb,
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: arguments
            .as_ref()
            .map_or(PCWSTR::null(), |value| PCWSTR(value.as_ptr())),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    // SAFETY: The strings live for the duration of the call.
    unsafe { ShellExecuteExW(&mut request) }.map_err(|error| LaunchError {
        id: id.to_owned(),
        code: error.code().0 as isize,
    })
}

/// Opens `files` with the executable `app`.
pub fn open_with(app: &str, files: &[String]) -> Result<(), LaunchError> {
    let arguments: Vec<String> = files.iter().map(|file| format!("\"{file}\"")).collect();
    shell_execute(w!("open"), app, Some(&arguments.join(" ")))
}

/// `true` if the window in front belongs to a program running as
/// administrator (Task Manager, an admin terminal, an installer), or to one
/// the dock cannot even inspect, which is never less privileged.
fn foreground_is_elevated() -> bool {
    // SAFETY: Read-only queries on the foreground window.
    let pid = unsafe {
        let window = GetForegroundWindow();
        if window.is_invalid() {
            return false;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(window, Some(&mut pid));
        pid
    };
    process_is_elevated(pid)
}

/// See [`foreground_is_elevated`].
#[must_use]
pub fn process_is_elevated(pid: u32) -> bool {
    // SAFETY: Read-only queries; both handles are closed before returning.
    unsafe {
        let Ok(process) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return true;
        };
        let mut token = HANDLE::default();
        let opened = OpenProcessToken(process, TOKEN_QUERY, &mut token).is_ok();
        let _ = CloseHandle(process);
        if !opened {
            return true;
        }
        let mut elevation = TOKEN_ELEVATION::default();
        let mut size = 0u32;
        let read = GetTokenInformation(
            token,
            TokenElevation,
            Some((&raw mut elevation).cast()),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut size,
        )
        .is_ok();
        let _ = CloseHandle(token);
        !read || elevation.TokenIsElevated != 0
    }
}

/// Presses the given keys together and releases them.
///
/// Windows drops simulated input aimed at a program running as administrator
/// (UIPI), so with one in front the Windows key did nothing. In that case the
/// dock first takes the focus itself, which it may because the user just
/// clicked it, and the keys then go to an ordinary process.
fn tap(keys: &[VIRTUAL_KEY]) {
    if foreground_is_elevated() {
        // SAFETY: Looks up the dock's own window and focuses it.
        unsafe {
            if let Ok(dock) = FindWindowW(crate::dock_link::DOCK_WINDOW_CLASS, None) {
                bring_to_front(dock);
            }
        }
    }
    send_keys(keys);
}

fn send_keys(keys: &[VIRTUAL_KEY]) -> bool {
    let key = |vk: VIRTUAL_KEY, flags: KEYBD_EVENT_FLAGS| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                dwFlags: flags,
                ..Default::default()
            },
        },
    };
    let inputs: Vec<INPUT> = keys
        .iter()
        .map(|&vk| key(vk, KEYBD_EVENT_FLAGS(0)))
        .chain(keys.iter().rev().map(|&vk| key(vk, KEYEVENTF_KEYUP)))
        .collect();
    // SAFETY: The array lives for the duration of the call.
    unsafe { SendInput(&inputs, size_of::<INPUT>() as i32) as usize == inputs.len() }
}

/// Opens the Windows Start menu, exactly as the Windows key does.
pub fn open_start_menu() {
    tap(&[VK_LWIN]);
}

/// Opens the Windows 11 quick settings panel (network, volume, battery): Win+A.
pub fn open_quick_settings() {
    tap(&[VK_LWIN, VK_A]);
}

/// Opens the notification center and calendar: Win+N on Windows 11. Windows
/// 10 keeps notifications in the Action Center, which is Win+A there.
pub fn open_notifications() {
    if crate::system::is_windows_11() {
        tap(&[VK_LWIN, VK_N]);
    } else {
        tap(&[VK_LWIN, VK_A]);
    }
}

/// Windows Search, as the taskbar's search button: Win+S.
pub fn open_search() {
    tap(&[VK_LWIN, VK_S]);
}

/// Task View (open windows and virtual desktops): Win+Tab.
pub fn open_task_view() {
    tap(&[VK_LWIN, VK_TAB]);
}

/// The Widgets board: Win+W.
pub fn open_widgets() {
    tap(&[VK_LWIN, VK_W]);
}

/// The sound output page of quick settings (Windows 11 22H2+): Win+Ctrl+V.
/// Windows 10 has no shortcut for its volume flyout, so it opens the sound
/// page of Settings instead.
pub fn open_sound() {
    if crate::system::is_windows_11() {
        tap(&[VK_LWIN, VK_LCONTROL, VK_V]);
    } else {
        let _ = open("ms-settings:sound");
    }
}

/// Opens the Windows Bluetooth settings page.
pub fn open_bluetooth() {
    let _ = open("ms-settings:bluetooth");
}

/// Shows the desktop, or brings the windows back: what the taskbar's corner
/// button does (Win+D).
pub fn show_desktop() {
    tap(&[VK_LWIN, VK_D]);
}

/// How long a pressed "^" button gets to show its flyout before Win+B is tried.
const HIDDEN_ICONS_INVOKE_WAIT: Duration = Duration::from_millis(1200);
/// How long Win+B gets to hand the hidden taskbar the focus.
const HIDDEN_ICONS_FOCUS_WAIT: Duration = Duration::from_millis(1000);
/// How long Explorer may reposition the flyout during its opening animation.
const HIDDEN_ICONS_HOLD: Duration = Duration::from_millis(1200);
/// Space between the menu bar and the flyout, in physical pixels.
const HIDDEN_ICONS_GAP: i32 = 6;
/// The hidden-icons flyout: Windows 11, then Windows 10.
const OVERFLOW_CLASSES: [PCWSTR; 2] = [
    w!("TopLevelWindowForOverflowXamlIsland"),
    w!("NotifyIconOverflowWindow"),
];

/// The notification area's hidden icons, the taskbar's "^" button. The
/// taskbar is only transparent and auto-hidden while Muelle runs, so the
/// button is still there to press through UI Automation; when that fails,
/// Win+B moves the focus to it and Enter opens it. Only the flyout shows.
///
/// Windows opens the flyout at the bottom of the screen, above where the
/// taskbar would be, so it is then moved under the arrow that opened it:
/// `anchor` is the arrow's centre and the bottom of the menu bar.
///
/// A second click closes it, as with the taskbar's own button. Win+B with the
/// flyout open would instead move the focus, and the pointer, to its first
/// icon.
pub fn toggle_hidden_icons(anchor: (i32, i32)) {
    let was_open = OVERFLOW_OPEN_AT_PRESS.swap(false, Ordering::Relaxed);
    if let Some(flyout) = open_overflow() {
        // SAFETY: Focuses the flyout so that Escape reaches it and not the
        // app the user was working in.
        unsafe {
            if GetForegroundWindow() != flyout {
                bring_to_front(flyout);
            }
            if GetForegroundWindow() == flyout {
                send_keys(&[VK_ESCAPE]);
            }
        }
        crate::log::line("iconos ocultos: clic para cerrar el menú visible");
        return;
    }
    if was_open {
        // The press itself already dismissed it.
        crate::log::line("iconos ocultos: el menú se cerró al presionar");
        return;
    }
    if OPENING_HIDDEN_ICONS.swap(true, Ordering::Relaxed) {
        crate::log::line("iconos ocultos: apertura en curso");
        return;
    }
    let front = crate::tracker::foreground_class();
    // SAFETY: Read the shell PID and hand it foreground rights while the
    // dock still has the user's click, before Invoke crosses into Explorer.
    let explorer = unsafe {
        FindWindowW(w!("Shell_TrayWnd"), None).ok().map(|tray| {
            let mut pid = 0;
            GetWindowThreadProcessId(tray, Some(&mut pid));
            (pid, pid != 0 && AllowSetForegroundWindow(pid).is_ok())
        })
    };
    let Some((pid, granted)) = explorer.filter(|(pid, _)| *pid != 0) else {
        crate::log::line(&format!(
            "iconos ocultos: barra de tareas no disponible (frente: {front})"
        ));
        OPENING_HIDDEN_ICONS.store(false, Ordering::Relaxed);
        return;
    };
    // UI Automation calls into Explorer. Run them off the dock's UI thread.
    std::thread::spawn(move || {
        open_hidden_icons(&TrayEvents::new(pid), anchor, &front, granted);
        OPENING_HIDDEN_ICONS.store(false, Ordering::Relaxed);
    });
}

fn open_hidden_icons(events: &TrayEvents, anchor: (i32, i32), front: &str, granted: bool) {
    let invoke = invoke_chevron();
    let invoked = invoke.is_ok();
    let mut flyout = if invoked {
        events.wait_for(HIDDEN_ICONS_INVOKE_WAIT, open_overflow)
    } else {
        open_overflow()
    };
    let mut path = "invocar";
    let mut entered = false;
    if flyout.is_none() {
        path = "Win+B";
        tap(&[VK_LWIN, VK_B]);
        // Foreground can change before the shell gives its tray keyboard focus.
        // Also accept a flyout that finished opening while the focus changed.
        events.wait_for(HIDDEN_ICONS_FOCUS_WAIT, || {
            open_overflow().or_else(tray_keyboard_focus)
        });
        flyout = open_overflow();
        if flyout.is_none() && tray_keyboard_focus().is_some() {
            // Focus is already in the tray. The generic shortcut helper can
            // activate Muelle first, so keep this Enter on Explorer's queue.
            entered = send_keys(&[VK_RETURN]);
            if entered {
                flyout = events.wait_for(HIDDEN_ICONS_INVOKE_WAIT, open_overflow);
            }
        }
    }
    crate::log::line(&format!(
        "iconos ocultos: {path}, permiso de foco: {granted}, invocación: {invoke:?}, Enter encolado: {entered}, menú visible: {} (frente inicial: {front}, final: {})",
        flyout.is_some(),
        crate::tracker::foreground_class()
    ));
    if let Some(flyout) = flyout {
        events.wait_for(HIDDEN_ICONS_HOLD, || {
            if open_overflow() != Some(flyout) {
                return Some(());
            }
            keep_under(flyout, anchor);
            None
        });
    }
}

fn tray_keyboard_focus() -> Option<HWND> {
    let mut info = GUITHREADINFO {
        cbSize: size_of::<GUITHREADINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: Read the foreground input queue into a valid output buffer.
    unsafe {
        GetGUIThreadInfo(0, &mut info).ok()?;
        let tray = info.hwndActive;
        (tray == GetForegroundWindow()
            && crate::taskbar::is_hidden(tray)
            && !info.hwndFocus.is_invalid()
            && (info.hwndFocus == tray || IsChild(tray, info.hwndFocus).as_bool()))
        .then_some(info.hwndFocus)
    }
}

/// While the click worker is opening and positioning the flyout.
static OPENING_HIDDEN_ICONS: AtomicBool = AtomicBool::new(false);

/// Whether the taskbar has the focus because Muelle is opening its
/// hidden-icons flyout (see `toggle_hidden_icons`).
#[must_use]
pub fn opening_hidden_icons() -> bool {
    OPENING_HIDDEN_ICONS.load(Ordering::Relaxed)
}

/// Gives the focus to the first of `windows` (front to back) that is on
/// screen, or to the desktop when none is, as a click on it would.
pub fn focus_front_or_desktop(windows: &[isize]) {
    // SAFETY: Read-only checks and one focus request; they have no effect on
    // a window that is gone, or when Windows refuses the request.
    unsafe {
        let front = windows
            .iter()
            .map(|&raw| HWND(raw as *mut _))
            .find(|&hwnd| {
                IsWindowVisible(hwnd).as_bool()
                    && !IsIconic(hwnd).as_bool()
                    && !crate::tracker::is_cloaked(hwnd)
            })
            .unwrap_or_else(|| GetShellWindow());
        if !front.is_invalid() {
            bring_to_front(front);
        }
    }
}

/// Whether the hidden-icons flyout was open when the mouse button went down
/// on the menu bar: if that press dismissed it, the click must not reopen it.
static OVERFLOW_OPEN_AT_PRESS: AtomicBool = AtomicBool::new(false);

/// Call on mouse-button down over the menu bar, before its click.
pub fn note_menu_bar_press() {
    OVERFLOW_OPEN_AT_PRESS.store(open_overflow().is_some(), Ordering::Relaxed);
}

/// Presses the XAML chevron or the legacy button by its specific identity.
/// SystemTrayIcon by itself can also resolve to an unrelated tray icon.
fn invoke_chevron() -> windows::core::Result<()> {
    // SAFETY: COM is initialized for this short-lived thread and every
    // interface is released before `CoUninitialize`.
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        let invoked = (|| -> windows::core::Result<()> {
            let taskbar = FindWindowW(w!("Shell_TrayWnd"), None)?;
            let automation: IUIAutomation =
                CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)?;
            let root = automation.ElementFromHandle(taskbar)?;
            let chevron = automation.CreatePropertyCondition(
                UIA_ClassNamePropertyId,
                &"SystemTray.ChevronIconView".into(),
            )?;
            let found = root
                .FindFirst(TreeScope_Descendants, &chevron)
                .or_else(|_| {
                    // Some builds expose the chevron's friendly name and the
                    // generic SystemTrayIcon ID, without the specific class.
                    let labels = [
                        "Show hidden icons",
                        "Mostrar iconos ocultos",
                        "Mostrar íconos ocultos",
                    ];
                    let [english, spanish, spanish_accented] = labels.map(|label| {
                        automation.CreatePropertyCondition(UIA_NamePropertyId, &label.into())
                    });
                    let legacy = automation
                        .CreatePropertyCondition(UIA_AutomationIdPropertyId, &"1502".into())?;
                    let conditions = [
                        Some(english?),
                        Some(spanish?),
                        Some(spanish_accented?),
                        Some(legacy),
                    ];
                    root.FindFirst(
                        TreeScope_Descendants,
                        &automation.CreateOrConditionFromNativeArray(&conditions)?,
                    )
                })?;
            let default_action = || {
                found
                    .GetCurrentPatternAs::<IUIAutomationLegacyIAccessiblePattern>(
                        UIA_LegacyIAccessiblePatternId,
                    )?
                    .DoDefaultAction()
            };
            // The match can be the glyph inside the button: climb to the
            // nearest element that can be pressed.
            let walker = automation.ControlViewWalker()?;
            let mut element = found.clone();
            for _ in 0..4 {
                if let Ok(invoke) =
                    element.GetCurrentPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId)
                {
                    return invoke
                        .Invoke()
                        .or_else(|error| default_action().map_err(|_| error));
                }
                let Ok(parent) = walker.GetParentElement(&element) else {
                    break;
                };
                element = parent;
            }
            default_action()
        })();
        CoUninitialize();
        invoked
    }
}

/// Explorer hooks belong to the click worker and are removed before it exits.
struct TrayEvents([HWINEVENTHOOK; 4]);

impl TrayEvents {
    fn new(pid: u32) -> Self {
        let ranges = [
            (EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND),
            (EVENT_OBJECT_SHOW, EVENT_OBJECT_FOCUS),
            (EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_LOCATIONCHANGE),
            (EVENT_OBJECT_CLOAKED, EVENT_OBJECT_UNCLOAKED),
        ];
        Self(ranges.map(|(first, last)| {
            // SAFETY: Out-of-context events wake the hook thread's queue.
            unsafe {
                SetWinEventHook(
                    first,
                    last,
                    None,
                    Some(on_tray_event),
                    pid,
                    0,
                    WINEVENT_OUTOFCONTEXT,
                )
            }
        }))
    }

    fn wait_for<T>(&self, limit: Duration, mut ready: impl FnMut() -> Option<T>) -> Option<T> {
        let deadline = Instant::now() + limit;
        loop {
            let result = ready();
            let remaining = deadline.saturating_duration_since(Instant::now());
            if result.is_some() || remaining.is_zero() {
                return result;
            }
            // SAFETY: No handles. Wait for Windows messages or one deadline.
            unsafe {
                let timeout = remaining.as_millis().max(1) as u32;
                if MsgWaitForMultipleObjectsEx(None, timeout, QS_ALLINPUT, MWMO_INPUTAVAILABLE)
                    != WAIT_OBJECT_0
                {
                    return ready();
                }
                let mut msg = MSG::default();
                if PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    DispatchMessageW(&msg);
                }
            }
        }
    }
}

impl Drop for TrayEvents {
    fn drop(&mut self) {
        for hook in self.0 {
            if !hook.is_invalid() {
                // SAFETY: This is the thread that installed the hook.
                unsafe {
                    let _ = UnhookWinEvent(hook);
                }
            }
        }
    }
}

// The notification wakes the queue. The worker checks the current state
// after dispatch because Explorer can change it again before delivery.
unsafe extern "system" fn on_tray_event(
    _: HWINEVENTHOOK,
    _: u32,
    _: HWND,
    _: i32,
    _: i32,
    _: u32,
    _: u32,
) {
}

/// The taskbar and its overflow can briefly own the foreground while opening
/// the flyout. They do not provide the user's active input context.
pub fn is_tray_window(hwnd: HWND) -> bool {
    matches!(
        crate::tracker::class_name(hwnd).as_str(),
        "Shell_TrayWnd"
            | "Shell_SecondaryTrayWnd"
            | "TopLevelWindowForOverflowXamlIsland"
            | "NotifyIconOverflowWindow"
    )
}

/// Whether the hidden-icons flyout is on screen.
#[must_use]
pub fn hidden_icons_open() -> bool {
    open_overflow().is_some()
}

/// The hidden-icons flyout, if it is on screen. Its host window can stay
/// "visible" while closed but cloaked or empty; that is not open.
fn open_overflow() -> Option<HWND> {
    OVERFLOW_CLASSES.iter().find_map(|&class| {
        // SAFETY: Read-only window lookups; `rect` is a valid output.
        unsafe {
            FindWindowW(class, None).ok().filter(|&window| {
                let mut rect = RECT::default();
                IsWindowVisible(window).as_bool()
                    && !crate::tracker::is_cloaked(window)
                    && GetWindowRect(window, &mut rect).is_ok()
                    && rect.right > rect.left
                    && rect.bottom > rect.top
                    && !MonitorFromRect(&rect, MONITOR_DEFAULTTONULL).is_invalid()
            })
        }
    })
}

/// Moves `flyout` (keeping its size) so it hangs centred under `anchor`,
/// inside the primary monitor. Does nothing once it is there.
fn keep_under(flyout: HWND, (center, top): (i32, i32)) {
    let Some(monitor) = crate::system::primary_monitor() else {
        return;
    };
    let mut rect = RECT::default();
    // SAFETY: `rect` is a valid out parameter.
    if unsafe { GetWindowRect(flyout, &mut rect) }.is_err() {
        return;
    }
    let width = rect.right - rect.left;
    let screen = monitor.screen;
    let min_left = screen.left + HIDDEN_ICONS_GAP;
    let max_left = (screen.right - HIDDEN_ICONS_GAP - width).max(min_left);
    let left = (center - width / 2).clamp(min_left, max_left);
    let top = top + HIDDEN_ICONS_GAP;
    if (rect.left, rect.top) == (left, top) {
        return;
    }
    // SAFETY: Moves a window of the same user session; its size and
    // z-order stay as Windows set them.
    unsafe {
        let _ = SetWindowPos(
            flyout,
            None,
            left,
            top,
            0,
            0,
            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS,
        );
    }
}

pub fn reveal(path: &str) -> Result<(), LaunchError> {
    shell_execute(
        w!("open"),
        "explorer.exe",
        Some(&format!("/select,\"{path}\"")),
    )
}

/// Asks the windows to close, as the close button does; each app can prompt
/// the user to save changes.
pub fn close(windows: &[isize]) {
    for &hwnd in windows {
        // SAFETY: Only a message is sent; if the window no longer exists, the call has no effect.
        unsafe {
            let _ = PostMessageW(Some(HWND(hwnd as *mut _)), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }
}

/// Brings an app's windows to the foreground. `windows` is ordered from front
/// to back; the first window receives focus.
///
/// If all windows are minimized, restores only the first one, as macOS does.
pub fn activate(windows: &[isize]) {
    activate_with(windows, |front| {
        // SAFETY: The activation helper validates the handle before calling us.
        unsafe { bring_to_front(front) }
    });
}

fn activate_with(windows: &[isize], request_focus: impl FnOnce(HWND) -> bool) {
    let handles = || windows.iter().map(|&raw| HWND(raw as *mut _));
    // SAFETY: Requests targeting another process are queued, so its hung UI
    // cannot block the dock. Calls on a closed window fail without effect.
    unsafe {
        let valid = |hwnd: HWND| IsWindow(Some(hwnd)).as_bool() && IsWindowVisible(hwnd).as_bool();
        let Some(front) = handles()
            .find(|&hwnd| {
                valid(hwnd) && !IsIconic(hwnd).as_bool() && !crate::tracker::is_cloaked(hwnd)
            })
            .or_else(|| handles().find(|&hwnd| valid(hwnd) && !IsIconic(hwnd).as_bool()))
            .or_else(|| handles().find(|&hwnd| valid(hwnd)))
        else {
            return;
        };
        if IsIconic(front).as_bool() && !ShowWindowAsync(front, SW_RESTORE).as_bool() {
            // As in native Alt+Tab, this also restores elevated applications.
            SwitchToThisWindow(front, true);
        }
        // Restoration and focus are separate requests. A transient shell
        // flyout can reject activation, but must not leave the app minimized.
        if !request_focus(front) {
            return;
        }
        // Foreground activation is asynchronous across input queues. Wait
        // briefly for it to settle; never stack background peers on refusal.
        if SendMessageTimeoutW(
            front,
            WM_NULL,
            WPARAM(0),
            LPARAM(0),
            SMTO_ABORTIFHUNG,
            100,
            None,
        )
        .0 == 0
        {
            return;
        }
        stack_group_behind(front, windows, || GetForegroundWindow());
    }
}

/// Requests focus once. A later user action, such as Alt+Tab, must win over
/// this request; synthetic mouse input must not force it back.
unsafe fn bring_to_front(hwnd: HWND) -> bool {
    // SAFETY: Calls on another window; they fail without effect if it is gone.
    unsafe { SetForegroundWindow(hwnd).as_bool() }
}

/// Stack peers beneath the confirmed foreground window, never above it.
/// This also keeps all peers behind the next app when native Alt+Tab wins.
fn stack_group_behind(front: HWND, windows: &[isize], foreground: impl Fn() -> HWND) {
    // SAFETY: Read-only checks and queued positioning of live top-level windows.
    unsafe {
        if foreground() != front || GetWindowLongW(front, GWL_EXSTYLE) as u32 & WS_EX_TOPMOST.0 != 0
        {
            return;
        }
        let mut previous = front;
        for &raw in windows {
            let hwnd = HWND(raw as *mut _);
            if foreground() != front {
                break;
            }
            if hwnd == front
                || !IsWindow(Some(hwnd)).as_bool()
                || !IsWindowVisible(hwnd).as_bool()
                || IsIconic(hwnd).as_bool()
                || crate::tracker::is_cloaked(hwnd)
                || GetWindowLongW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_TOPMOST.0 != 0
            {
                continue;
            }
            if SetWindowPos(
                hwnd,
                Some(previous),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER | SWP_ASYNCWINDOWPOS,
            )
            .is_ok()
            {
                previous = hwnd;
            }
        }
    }
}

/// Minimizes an app's windows, as clicking the active app's taskbar button
/// does. Asynchronous, so an app that is not responding cannot block the dock.
pub fn minimize(windows: &[isize]) {
    for &raw in windows {
        let hwnd = HWND(raw as *mut _);
        // SAFETY: Calls on another window; they fail without effect if it is gone.
        unsafe {
            if IsWindowVisible(hwnd).as_bool()
                && !IsIconic(hwnd).as_bool()
                && !crate::tracker::is_cloaked(hwnd)
            {
                let _ = ShowWindowAsync(hwnd, SW_MINIMIZE);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::{
        Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DestroyWindow, EnumWindows, HWND_TOP, HWND_TOPMOST, SW_SHOWNOACTIVATE,
            ShowWindow, WS_EX_TOOLWINDOW, WS_OVERLAPPEDWINDOW,
        },
        core::BOOL,
    };

    struct Desktop {
        windows: Vec<HWND>,
    }

    impl Desktop {
        fn new() -> windows::core::Result<Self> {
            let mut desktop = Self {
                windows: Vec::new(),
            };
            // Only test-owned windows are moved or activated. The third covers
            // both side-by-side peers, like a maximized browser.
            for (index, (x, width)) in [(40, 240), (280, 240), (40, 480)].into_iter().enumerate() {
                let hwnd = unsafe {
                    CreateWindowExW(
                        WS_EX_TOOLWINDOW,
                        w!("STATIC"),
                        &HSTRING::from(format!("Muelle stacking test {index}")),
                        WS_OVERLAPPEDWINDOW,
                        x,
                        40,
                        width,
                        240,
                        None,
                        None,
                        None,
                        None,
                    )?
                };
                desktop.windows.push(hwnd);
                unsafe {
                    let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                }
            }
            Ok(desktop)
        }

        fn order(&self) -> windows::core::Result<Vec<isize>> {
            unsafe extern "system" fn collect(hwnd: HWND, data: LPARAM) -> BOOL {
                // SAFETY: The vector outlives the synchronous enumeration.
                unsafe {
                    (*(data.0 as *mut Vec<isize>)).push(hwnd.0 as isize);
                }
                true.into()
            }
            let mut order: Vec<isize> = Vec::new();
            unsafe {
                EnumWindows(Some(collect), LPARAM(&raw mut order as isize))?;
            }
            order.retain(|raw| self.windows.iter().any(|hwnd| hwnd.0 as isize == *raw));
            Ok(order)
        }
    }

    impl Drop for Desktop {
        fn drop(&mut self) {
            unsafe {
                for &hwnd in &self.windows {
                    let _ = DestroyWindow(hwnd);
                }
            }
        }
    }

    #[test]
    #[ignore = "requires a Windows desktop; cloaks only a test-owned window without taking focus"]
    fn native_workspace_activation_prefers_visible_peers_and_preserves_background_cloaks()
    -> windows::core::Result<()> {
        use std::cell::Cell;
        use windows::Win32::Graphics::Dwm::{DWMWA_CLOAK, DwmSetWindowAttribute};

        let desktop = Desktop::new()?;
        let [cloaked, visible, _] = desktop.windows[..] else {
            unreachable!("three test windows");
        };
        let cloak = windows::core::BOOL::from(true);
        // SAFETY: Only the fixture's own window is cloaked, never user apps.
        unsafe {
            DwmSetWindowAttribute(
                cloaked,
                DWMWA_CLOAK,
                (&raw const cloak).cast(),
                size_of::<windows::core::BOOL>() as u32,
            )?;
        }
        assert!(crate::tracker::is_cloaked(cloaked));
        let group = [cloaked.0 as isize, visible.0 as isize];
        let selected = Cell::new(HWND::default());
        activate_with(&group, |target| {
            selected.set(target);
            false // Record the request without changing the user's foreground.
        });
        assert_eq!(selected.get(), visible);
        activate_with(&group[..1], |target| {
            selected.set(target);
            false
        });
        assert_eq!(
            selected.get(),
            cloaked,
            "a remote-only window remains activatable"
        );
        let before = desktop.order()?;
        stack_group_behind(visible, &group, || visible);
        assert_eq!(desktop.order()?, before, "do not reposition a cloaked peer");
        minimize(&group[..1]);
        assert!(!unsafe { IsIconic(cloaked) }.as_bool());
        assert!(crate::tracker::is_cloaked(cloaked));
        Ok(())
    }

    #[test]
    #[ignore = "Requires a Windows desktop; briefly restores only a test-owned window"]
    fn native_minimized_window_restores_even_when_the_focus_request_is_refused()
    -> windows::core::Result<()> {
        use windows::Win32::UI::WindowsAndMessaging::{
            DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, SW_SHOWMINNOACTIVE,
        };
        let desktop = Desktop::new()?;
        let target = desktop.windows[0];
        // SAFETY: Minimize only a window this fixture owns without activating it.
        unsafe {
            let _ = ShowWindow(target, SW_SHOWMINNOACTIVE);
        }
        assert!(unsafe { IsIconic(target) }.as_bool());
        activate_with(&[target.0 as isize], |_| false);
        let deadline = Instant::now() + Duration::from_millis(300);
        let mut message = MSG::default();
        // SAFETY: Drain this test thread's queue so asynchronous restoration
        // can complete. All windows are destroyed by the fixture afterwards.
        while unsafe { IsIconic(target) }.as_bool() && Instant::now() < deadline {
            unsafe {
                while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                    DispatchMessageW(&message);
                }
            }
            std::thread::yield_now();
        }
        assert!(!unsafe { IsIconic(target) }.as_bool());
        Ok(())
    }

    #[test]
    #[ignore = "requires an interactive Windows desktop; positions only test-owned windows"]
    fn native_group_should_not_restack_after_foreground_changes() -> windows::core::Result<()> {
        let desktop = Desktop::new()?;
        let [left, right, cover] = desktop.windows[..] else {
            unreachable!("three test windows");
        };
        let group = [right.0 as isize, left.0 as isize];
        // Inject the foreground identity to exercise the real native stacking
        // without fighting the foreground lock or interrupting the user's app.
        unsafe {
            SetWindowPos(
                right,
                Some(HWND_TOP),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            )?;
        }
        stack_group_behind(right, &group, || right);
        assert_eq!(
            desktop.order()?,
            [right.0 as isize, left.0 as isize, cover.0 as isize]
        );
        // The next app's position must cover both side-by-side peers.
        unsafe {
            SetWindowPos(
                cover,
                Some(HWND_TOP),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            )?;
        }
        let order = desktop.order()?;
        assert_eq!(order[0], cover.0 as isize);
        // A stale activation must not restack the old group over the browser.
        stack_group_behind(right, &group, || cover);
        assert_eq!(desktop.order()?, order);
        // An app's explicit always-on-top choice must not spread to peers.
        unsafe {
            SetWindowPos(
                left,
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            )?;
        }
        stack_group_behind(left, &[left.0 as isize, right.0 as isize], || left);
        assert_eq!(
            unsafe { GetWindowLongW(right, GWL_EXSTYLE) } as u32 & WS_EX_TOPMOST.0,
            0
        );
        Ok(())
    }
}
