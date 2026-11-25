# Plan: glass.4 fixes (search, battery, panel look, rebrand)

Owner report from Windows 11, build v0.1.22-glass.4. Rule 1 holds for every
item: no polling, no timers, no always-on work; closed panels cost nothing.

## Root causes

### 1. Search shows an empty white bar and takes no keys

- `apps/dock/src/spotlight.rs:glass` called `DwmExtendFrameIntoClientArea`
  with `-1` margins on a window that also had a system backdrop. GDI writes
  no alpha, so every GDI pixel in the client area (the panel fill, the result
  rows, and the child EDIT control with its text, caret and cue banner) is
  composed as alpha 0 and added onto the acrylic. Dark text vanishes and the
  white `edit_brush` becomes a plain white bar.
- `spotlight.rs:show` and `spotlight.rs:take_focus` activated the panel
  twice: `ShowWindow(SW_SHOW)`, then `AttachThreadInput` plus
  `SetForegroundWindow`. Attaching to the foreground thread from a
  `WS_EX_NOACTIVATE` menu-bar click left the panel shown but not the
  foreground window. The flat grey in the screenshot is the backdrop's
  inactive fallback. Keys kept going to the previous app.

### 2. Battery needs three clicks

- `apps/dock/src/battery.rs:show` and `battery.rs:take_focus` had the same
  double activation. `battery.rs:panel_proc` handled `WM_ACTIVATE` by calling
  `hide(true)` synchronously, so the transient `WA_INACTIVE` produced while
  activation bounces closed the panel right after it opened. `hide(true)` also
  stamps `CLOSED_AT`, so the second click fell inside `REOPEN_GUARD` in
  `battery.rs:activate` and was swallowed. Only the third click opened it.

### 3. Battery and search look (flat grey, white text, white box)

- Same GDI-on-extended-frame blending as 1, in `battery.rs:glass` and
  `battery.rs:paint` (white text over the grey fallback, and the hot
  "Configuración de la batería" row drawn as a white box with invisible
  text).
- Both panels took `light` from `crate::appearance::current().light`, which
  `appearance.rs:resolve` always reports as `true` for the Glass appearance.
  The Windows app theme ("Choose your app mode") was never read.

### 4. "Muelle" still visible

- Settings strings, tray, notifications and the 8 locales were already clean;
  the only remaining "Muelle" in them is the required credit.
- `apps/settings/src-tauri/installer/hooks.nsh`: the installer's details list
  printed the internal names (`Extract: muelle.exe`,
  `Execute: …\Programs\Muelle\muelle.exe --restore`) from the hooks and the
  file copy.
- `apps/settings/src-tauri/tauri.bundle.conf.json` (`homepage`) and
  `apps/settings/src-tauri/src/main.rs` (`REPOSITORY`, shown in Settings)
  still pointed at `jhosepqr/macos`.

## Fixed in this change

| Issue | Fix | CPU / RAM impact |
| --- | --- | --- |
| 1, 2 | New `apps/dock/src/panel_theme.rs:take_focus`: one `SetForegroundWindow`, with `AttachThreadInput` only as a fallback if that fails, then `SetFocus` on the edit (search) or panel (battery). Both `show` functions use `SW_SHOWNA`, so activation happens once. If Windows still refuses, one log line (`buscador`/`batería: Windows no dio el foco al panel`). | One call per open. Zero while closed. |
| 2 | `WA_INACTIVE` now posts `CHECK_FOREGROUND` (`WM_APP + 41` battery, `WM_APP + 42` search). `closes_on_deactivate` hides only if the panel is visible and really not the foreground window. Transient deactivation no longer closes it or arms `REOPEN_GUARD`. The menu bar is `WS_EX_NOACTIVATE`, so a second icon click finds the panel open and closes it. | One posted message per deactivation. No timer. |
| 1, 3 | No more `DwmExtendFrameIntoClientArea`. Both panels are opaque cards painted with GDI, with DWM round corners, a DWM border colour and immersive dark mode (`panel_theme.rs:style_window`). Search draws a bordered field and centres a one-line EDIT inside it; `WM_CTLCOLOREDIT` uses an opaque text background. | Same paint work as before. |
| 3 | `muelle_shell::system::apps_use_light_theme` reads `AppsUseLightTheme`. `panel_theme.rs:Palette` has light and dark colours, rebuilt on open only if the theme changed. The test `text_is_readable_in_both_themes` checks WCAG contrast: text ≥ 7, link and secondary ≥ 4.5 on fill, selection and field. | One registry read per open. |
| 4 | `hooks.nsh` prints `${PRODUCTNAME}` and sets `SetDetailsPrint none` from the pre-install hook through the post-install hook (and the same for uninstall), so the details list says "Nikao Dock" and no internal file names. The homepage and `REPOSITORY` now point at `jhosepqr/Nikao-Dock`. | Installer only. |
| — | `.github/workflows/fork-installer.yml`: a push to `main` builds the installer artifact; only a manual run republishes `v0.1.22-glass.4`. | CI only. |

B1 and B2 are implemented. The glass colours are the two constants
`GlassTokens::LIGHT_GLASS` and `GlassTokens::DARK_GLASS` in
`apps/dock/src/panel_theme.rs`. F1 edits those and nothing else. When
`system::transparency_enabled()` is false the panels use the opaque `Palette`
instead (`GlassTokens::for_theme(..., false)`). Not verified on Windows yet.

## Kept internal identifiers (renaming them risks breaking upgrades)

`muelle.exe` and `muelle-settings.exe` (installer process detection, the
autostart task's path, the updater's `MainBinaryName`), the window classes
`MuelleWindow`, `MuelleSpotlight` and `MuelleBattery`, the event
`Muelle.ConfigChanged`, the `Local\Muelle.*` guard objects, the app manifest
name `CrafterStation.Muelle`, the crate names `muelle-*`, the `muelle:` log
prefix and log messages, and the legacy-cleanup names in `autostart.rs` and
`hooks.nsh` (the `Muelle` Run value, the `Muelle (<sid>)` task, and
`%LOCALAPPDATA%\Programs\Muelle`) that exist to remove the old install. The
MIT `LICENSE` copyright and the `credit` string in the 8 locales stay as they
are.

## BACKEND work list (Grok 4.7 xhigh)

Done in this change. Notes for F1 and for a Windows check:

- Background: `menu::draw_tinted_background` (the same HostBackdrop plus tint
  sprite as `draw_popup_background`), with the tint from `GlassTokens`.
- Rim: a 1px composition stroke in `panel_glass::Scene::paint`, not GDI.
  The DWM border is `DWMWA_COLOR_NONE` while glass is on, so it is not drawn
  twice.
- Text: `Gfx::text_brush_colored`, the same DirectWrite surfaces as the menu.
- Search: the EDIT control is unchanged (IME, clipboard). It sits on an
  opaque field sprite of `GlassTokens::field`. The frame is not extended.
- `take_focus`, `SW_SHOWNA` and `CHECK_FOREGROUND` are unchanged. On focus
  failure the log line now includes the foreground window class.
- The `DesktopWindowTarget` is created in `show` and dropped in `hide`.
  Redraws happen on open, on a key, on hover and on a power change.

B1. Blurred glass for both panels, Composition only.
- Give `spotlight.rs` and `battery.rs` a `DesktopWindowTarget` and a visual
  tree like `menu.rs`. Draw the background with
  `menu.rs:draw_popup_background(layers, dark, glass, w, h)`: HostBackdrop
  brush plus `popup_tint`. Draw the 1 px light rim as a sprite, not with GDI.
- Text and rows must also move to Composition (D2D/DirectWrite surfaces, the
  way `menu.rs` draws items), because GDI on that window cannot be
  translucent.
- Search input: keep the real EDIT child (IME, clipboard, accessibility).
  It stays an opaque GDI child on a solid field colour, with a field sprite
  of the same colour around it. Do not extend the frame.
- Keep `panel_theme.rs:take_focus`, `SW_SHOWNA` and `CHECK_FOREGROUND`
  unchanged; they are the fix for 1 and 2.
- `dark` comes from `Palette::current().light`, not
  `appearance::current()`. If transparency is off
  (`system::transparency_enabled`) or not Windows 11, fall back to the
  current opaque palette.
- Acceptance on Windows 11: one click opens, one click closes, Esc and
  click-outside close; typing filters immediately; no `no dio el foco` line
  in the log; Task Manager shows 0% CPU with both panels closed.
- CPU: Composition visuals are created on open and dropped on hide (as
  `menu.rs` does). Nothing animates. No timers.

B2. Optional, only if the owner still sees focus loss: log
`GetForegroundWindow` class name in `take_focus`'s failure line to identify
the app holding the foreground lock. Zero cost when focus works.

## FRONTEND work list (Gemini 4 Neon high)

F1. Glass tokens. Define the light and dark tint, rim and text colours for
the glass panels, matching the dock (`menu.rs:popup_tint`: dark 38,38,40 at
alpha 175; light 246,246,248 at alpha 215). Check text, secondary text, link
and selection contrast against the tint over both a white and a black
wallpaper (≥ 4.5:1). Hand the values to B1 as a table; B1 puts them in
`panel_theme.rs`.

F2. Rebrand check on a real install: run `Nikao-Dock-Setup.exe` over an old
"Muelle" install and confirm that the details list, the Apps list entry,
the Start-menu shortcut, the Settings title and About, the tray tooltip and
the notifications all say "Nikao Dock". Report any screen that still says
"Muelle" (other than the credit) with a screenshot.

CPU: none at runtime.

## Release

Do not publish until B1 lands and the owner confirms 1–4 on Windows 11.
Then run "Fork installer" by hand to republish the pre-release.
