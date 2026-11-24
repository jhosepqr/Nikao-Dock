# Plan: macOS-style glass dock

Goal: make the dock look like the real macOS dock. It should be light, highly
transparent glass with the wallpaper visible but blurred, a thin
semi-transparent light rim, soft rounded corners, a gentle shadow, thin light
separators, and small subtle running-app dots. Icon layout and behaviour stay
exactly as they are.

Two implementers work in sequence on branch `feat/glass-dock`:

- **Part A (backend)**: config schema, capability detection, the material
  engine (blur and transparency), and IPC. Ship this first.
- **Part B (frontend)**: the visual tuning of the dock scene and the menu bar,
  plus the Settings UI. This part builds on Part A.

---

## 1. What exists today (read this first)

The dock is **not** a webview. Only Settings (`apps/settings`) is Tauri and
React. The dock (`apps/dock`, binary `muelle.exe`) is plain Win32 plus
**Windows.UI.Composition**, and it draws through a `DesktopWindowTarget`.
In this plan, "painting the pill" means Rust code in `apps/dock/src/scene.rs`,
not CSS.

| Concern | Where | Notes |
|---|---|---|
| Dock window creation | `apps/dock/src/app/lifecycle.rs` ~L163-197 | Two windows: `visual` (`WS_EX_NOREDIRECTIONBITMAP \| TOPMOST \| TOOLWINDOW \| NOACTIVATE \| LAYERED \| TRANSPARENT`) and `input`. `DWMWA_USE_HOSTBACKDROPBRUSH = TRUE` is set on `visual`, and its result is ignored because Windows 10 lacks the attribute. |
| Compositor and device | `apps/dock/src/gfx.rs` (`Gfx::new`, `rounded_mask`) | Shared by the dock, menu bar, menus, and previews. |
| Pill, rim, sheen, shadow, separators, dots | `apps/dock/src/scene.rs` | `Theme::new(Look)` picks the colours. `Theme::fills()` stacks `HostBackdropBrush`, then the tint, then optional Aurora glow. `GlassSurface::build` adds a sheen gradient and a rim (`ShapeVisual` stroke with a white gradient). `Scene::build` adds a `DropShadow` (blur 22·s, offset 8·s, alpha 56/96), separators (`SpriteVisual` filled with `theme.separator`), and running dots (`4·s` ellipses, `theme.indicator`). Corner radius is `layout.height * 0.36`. |
| Theme selection | `apps/dock/src/appearance.rs` | `Appearance` (config) maps to `Look { material, light, glass }`. A process-wide `AtomicU8 CHOICE` is set from config. `current()` reads Windows light/dark and `transparency_enabled()`. |
| Capability detection | `crates/muelle-shell/src/system.rs` | `transparency_enabled()` = Windows 11 **and** "Transparency effects" on **and** energy saver off. On Windows 10 it always returns `false`, so every theme is drawn **solid** there. `is_windows_11()` checks build >= 22000. |
| Rebuild trigger | `apps/dock/src/app/rendering.rs::refresh` | `StageKey { look: appearance::current(), ... }`. Any change in `Look` rebuilds the scene. The config reload is in `app/preferences.rs` ~L160-210 (`appearance::set(config.appearance)` then `refresh()`). |
| Other users of `Look`/`Theme` | `menubar.rs` (L2047 `Theme::for_bar(Look{..})`, popups), `multi_window_view.rs` L874, `group_view.rs`, `previews.rs`, `menu.rs` L324 (`transparency_enabled()` directly) | Every `Look { .. }` struct literal must be updated when fields are added. |
| Config schema | `crates/muelle-config/src/lib.rs` | `Config` uses `#[serde(default, rename_all = "camelCase")]`. `Appearance` is a lowercase enum where `Auto` is both `#[default]` and `#[serde(other)]`, which must stay the last variant. `sanitized()` clamps values. `CURRENT_VERSION = 1`. |
| Settings IPC | `apps/settings/src-tauri/src/main.rs` | `get_config`, `default_config`, and `set_config(patch)` (generic JSON patch through `muelle_config::update_preferences`, then `dock_link::notify_config_changed()`). **New config fields need no new command.** |
| Settings UI | `apps/settings/src/{config.ts,App.tsx,visuals.tsx,controls.tsx,styles.css,i18n/*.json}`, tests in `apps/settings/tests/*.test.mjs` | Theme picker is `VisualChoices<Appearance>` with `THEMES`, `themeName()`, and `ThemePreview` (CSS `.theme-layer[data-theme=…]`, styles.css ~L561-579). `Slider` exists in `controls.tsx`. Saves are debounced (`SAVE_DELAY_MS = 60`). |
| Prior art | `spikes/src/bin/spike_glass.rs` | Already proves a rounded host-backdrop glass with a light rim on Windows 11. |

**Why the current dock looks like `actual.png`.** The default theme is `Auto`.
On a dark Windows theme it resolves to `Standard` dark: tint `rgba(28,28,30,82)`
over the host backdrop, plus a strong black shadow (`alpha 96`). If the user is
on **Windows 10**, has transparency effects off, or has energy saver on,
`transparency_enabled()` is false and the pill is fully opaque
`rgba(44,44,46,255)`. That matches the screenshot. Part A adds logging so the
real cause on the user's machine is visible.

### Options considered for blur

1. **`CreateHostBackdropBrush` (keep)**: this is real DWM blur of what is
   behind the window, already wired up, and clipped per visual. It works with
   our window that is larger than the pill (room for magnification) and
   per-pixel transparent. Its blur radius is fixed by the system, and it
   requires Windows 11.
2. `DWMWA_SYSTEMBACKDROP_TYPE` (Mica or Acrylic): applies to the **whole
   window rectangle**. We would need `SetWindowRgn` clipping, which would also
   clip magnified icons. Rejected.
3. `SetWindowCompositionAttribute(ACCENT_ENABLE_ACRYLICBLURBEHIND)`:
   undocumented, covers the whole window, and known to lag on Windows 10 1903+.
   Rejected.
4. A custom `GaussianBlurEffect` through `CompositionEffectFactory`: needs a
   hand-written `IGraphicsEffect` and `IGraphicsEffectD2D1Interop` in Rust
   (`#[implement]`). It can only add blur on top of the host backdrop. Out of
   scope, noted as possible future work.

**Chosen design:** keep option 1. Emulate **blur intensity** by setting the
**opacity of the host-backdrop sprite** (0 to 1). The window behind it is
transparent (`NOREDIRECTIONBITMAP`), so a partly opaque blurred layer
cross-fades between the sharp desktop and the fully blurred one. This is cheap,
and it changes no window styles. Emulate **transparency** through the alpha of
the tint. On Windows 10, or whenever the backdrop is unavailable but
transparency is allowed, draw a translucent tint **without blur**, using a
higher alpha floor for legibility.

---

## 2. Part A: BACKEND (do first)

### A1. Config schema (`crates/muelle-config/src/lib.rs`)

- Add `Appearance::Glass` (serialised `"glass"`) **before** `Auto`. `Auto` must
  remain the last variant, with `#[default]` and `#[serde(other)]`, so unknown
  future values still read as `Auto`.
- Add two fields to `Config`, with doc comments in the same style as the
  existing ones:
  - `glass_transparency: u8`: 0 to 100, where 100 means the most see-through.
    JSON key `glassTransparency`, default **65**.
  - `glass_blur: u8`: 0 to 100, where 0 means no blur. JSON key `glassBlur`,
    default **100**.
  - Both apply **only** to the Glass theme. Document that in the comments.
- Add constants `pub const GLASS_LEVEL_MAX: u8 = 100;`. Make
  `Config::sanitized()` clamp both fields to `0..=100`.
- `Config::default().appearance = Appearance::Glass`. Leave the enum's
  `#[default]` on `Auto`.
- **Migration**, so existing users actually get Glass by default. Every saved
  file already contains `"appearance":"auto"`, because the whole struct is
  serialised. Bump `CURRENT_VERSION` to `2`. In `load()`, after `sanitized()`,
  apply `migrated()`: if `version < 2`, then change `Auto` to `Glass` and set
  `version = 2`. Leave any other explicit choice untouched. Doing this in
  `load` means the dock and Settings see the same value, and the next write
  persists it. `update_preferences` must keep `"version"` protected, as it is
  today.
  - *Decision flag:* if the owner prefers not to touch existing users, delete
    `migrated()`. Then only new installs and Reset get Glass. Everything else
    is unaffected.
- Tests, following the existing style (`#[test]`, `-> Result<…>`, no `unwrap`):
  - `{"appearance":"glass"}` round-trips. `{}` gives `Glass` with defaults
    65/100. `{"appearance":"sepia"}` gives `Auto`.
  - `{"version":1,"appearance":"auto"}` becomes `Glass` with version 2.
    `{"version":1,"appearance":"dark"}` stays `Dark`.
    `{"version":2,"appearance":"auto"}` stays `Auto`, so a user who picks Auto
    again keeps it.
  - `glassTransparency: 250` clamps to 100. `update_preferences` with
    `glassBlur` changes only that field.
  - Update the existing `the_appearance_follows_windows_unless_chosen` test:
    `{}` now yields `Glass`, and unknown values yield `Auto`.
  - Check `load_fills_missing_fields_and_preserves_legacy_visibility` still
    passes (it compares against `Config::default()`).

### A2. Capability detection (`crates/muelle-shell/src/system.rs`)

- Keep `transparency_enabled()` with **unchanged semantics**, because
  `menu.rs`, the menu bar, and the existing themes rely on it. Add an alias or
  doc note so it can be read as "host backdrop available".
- Add `pub fn translucency_allowed() -> bool`: "Transparency effects" on
  **and** energy saver off, on **any** Windows version (no
  `is_windows_11()` gate). This allows translucent drawing without blur on
  Windows 10.
- Add `pub fn glass_support() -> GlassSupport { windows_11, transparency_effects, energy_saver }`.
  `muelle-shell` already depends on serde, so derive
  `Serialize` with `rename_all = "camelCase"`.

### A3. Look and appearance plumbing (`apps/dock/src/appearance.rs`)

- Add `Material::Glass`, and add it to `Material::ALL` (cfg(test)) so the
  existing ignored composition test covers it.
- Extend `Look`, which stays `Copy + Eq` because it is part of `StageKey`:
  - `translucent: bool`: from `translucency_allowed()`.
  - `transparency: u8` and `blur: u8`: copied from config. Store them in two
    new `AtomicU8`s, with `set_glass_levels(transparency, blur)` beside `set()`.
  - Update **every** `Look { .. }` literal:
    `scene.rs` (`for_popup`, attention test), `menubar.rs` ~L2047,
    `multi_window_view.rs` ~L874. The menu bar keeps its **own** `Look`
    struct (`menubar.rs` L208) with copies of `material`, `light`, and
    `glass`, filled in at ~L1914. Either add the new fields there too, or
    store an `appearance::Look` inside it. Otherwise slider changes will not
    rebuild the bar. Add a `Look::solid(material, light)` or
    `..Look::current_levels()` helper if that keeps the call sites small.
- `resolve()`: `Appearance::Glass` gives `(Material::Glass, light = true)`.
  The target is light glass with dark content, and it does **not** follow
  Windows dark mode. Add `Glass` to `CHOICES` and to the decode test. Use an
  exhaustive `match` with no wildcard arms, so new variants fail to compile.
- Call `set_glass_levels` wherever `appearance::set` is called:
  `app/lifecycle.rs` ~L202 and `app/preferences.rs` ~L184. Changing a slider
  then changes `Look`, which changes `StageKey`, which triggers a rebuild.
  This is the same path theme changes take. Settings debounces saves at 60 ms.
- Add pure, unit-tested mapping helpers in `appearance.rs`:
  - `glass_tint_alpha(transparency: u8) -> u8`: linear from 235 at 0 to 20 at
    100. The default of 65 gives about 95.
  - `glass_backdrop_opacity(blur: u8) -> f32`: `blur / 100`.
  - `GLASS_UNBLURRED_ALPHA_FLOOR: u8 = 150`: the minimum tint alpha when there
    is no backdrop, used on Windows 10 to keep icons readable.
  - Tests: the endpoints, monotonicity, and that the floor is applied.

### A4. Material engine (`apps/dock/src/scene.rs`, structure only)

Part A owns the **mechanics**. Part B owns the **art direction**, meaning the
colour and size constants.

- Add `backdrop_opacity: f32` to `Theme`, where 1.0 is used for every existing
  material. In `Theme::fills()`, when `glass && backdrop_opacity > 0`, push
  the `HostBackdropBrush` as before. Have `fills` return
  `(brush, opacity)` pairs, so its three callers call
  `fill.SetOpacity(opacity)` on the backdrop sprite: `GlassSurface::build`,
  `menubar.rs` ~L2053, and the other `CreateHostBackdropBrush` users in
  `menubar.rs` L3571/L3632, if they should follow the Glass levels.
- `Theme::new` for `Material::Glass`:
  - With `glass` (backdrop available): tint `rgba(255,255,255, glass_tint_alpha(t))`
    and `backdrop_opacity = glass_backdrop_opacity(b)`.
  - Without `glass` but with `translucent` (Windows 10, or backdrop missing):
    no backdrop, tint alpha `max(glass_tint_alpha(t), FLOOR)`.
  - With neither: solid `rgba(236,238,242,255)`, the same as Frosted solid,
    following "Reduce transparency" behaviour.
  - Use temporary values for `sheen`, `rim`, `separator`, `shadow`, and
    `indicator`. Copy the Frosted values; Part B tunes them.
- `Theme::for_bar`: add a `Material::Glass` arm. A denser light tint is enough,
  for example alpha `min(tint+60, 235)`. Part B tunes it.
- Also add `Material::Glass` arms anywhere else that matches on `Material`
  (search `Material::`).

### A5. Window and DWM (`apps/dock/src/app/lifecycle.rs`)

- No style changes are needed. Keep `DWMWA_USE_HOSTBACKDROPBRUSH`.
- Log the capabilities once at startup with `muelle_shell::log::line`:
  Windows 11 yes/no, transparency effects, energy saver, and the resolved
  `Look`. This is how we confirm on the user's PC why the dock was opaque.
- Verify that `transparency_enabled` and `translucency_allowed` are
  re-evaluated on the existing triggers: the theme or setting change
  (`WM_SETTINGCHANGE` "ImmersiveColorSet") and the power/energy-saver
  notification in `app.rs` ~L671. `appearance::current()` is called on every
  `refresh()`, so nothing new should be needed. Confirm it.

### A6. IPC (`apps/settings/src-tauri/src/main.rs`)

- No new command is needed for saving: `set_config` already accepts
  `{appearance:"glass", glassTransparency, glassBlur}`.
- Add a read-only command `glass_support()` that returns
  `{ windows11: bool, transparencyEffects: bool, energySaver: bool }` (serde
  camelCase struct), and register it in `generate_handler!`. Part B uses it to
  explain why the blur slider has no effect, for example on Windows 10.
- Optional: `open_transparency_settings()` opening
  `ms-settings:colors`. That page holds "Transparency effects" on both
  versions. Mirror `open_animation_settings`.

### A7. Part A risks

- **`SetOpacity` on a HostBackdrop sprite.** This should composite correctly
  over the transparent window, but it must be checked on real Windows 11
  hardware. If it does not work, fall back to a fixed backdrop plus a
  "frost" white overlay whose alpha follows `blur`, and document the
  slider as "frost".
- **Host backdrop shows black** when DWM disables it, for example with
  transparency off or energy saver on. This is already handled by
  `transparency_enabled()`. Do not bypass it for Glass.
- **Exhaustive matches:** adding a variant must break the build until it is
  handled. Do not add `_ =>` arms.
- **Migration** changes the theme of existing users from Auto to Glass once.
  Mention it in `CHANGELOG.md`.
- The Settings TypeScript `Config` type does not have the new fields yet.
  This is harmless until Part B, because extra JSON fields are ignored.

### A8. Part A acceptance checks

On the Linux VM, from the repo root, using the Windows MSVC target through
`cargo-xwin`:

```bash
# one-time setup
rustup target add x86_64-pc-windows-msvc
cargo install cargo-xwin --locked
sudo apt-get update && sudo apt-get install -y llvm-18 lld-18   # gives llvm-lib / lld-link
export PATH=/usr/lib/llvm-18/bin:$PATH

# checks (all verified working on this VM before the change)
cargo fmt --all --check
cargo xwin clippy --workspace --all-targets --target x86_64-pc-windows-msvc -- -D warnings
cargo xwin check  -p muelle -p muelle-settings --target x86_64-pc-windows-msvc
cargo test -p muelle-config            # pure Rust, runs natively on Linux
cargo test -p muelle-core              # pure Rust, if touched
```

A plain `cargo check --target x86_64-pc-windows-msvc` fails because
`aws-lc-sys` (pulled in by reqwest and rustls) needs `windows.h`. Use
`cargo xwin`, which downloads the MSVC CRT and SDK. Building
`muelle-settings` needs `apps/settings/dist`, so run
`pnpm -C apps/settings install && pnpm -C apps/settings build` first if it
is missing. The dock's unit tests (`appearance`, `scene`) only compile for
Windows. Run them in CI (`windows-latest`: fmt, clippy `-D warnings`,
`cargo test --workspace`), or check them on the VM with
`cargo xwin test --no-run -p muelle --target x86_64-pc-windows-msvc`.

Functional checks, for the owner on Windows:

- Windows 11 with transparency on: the dock is light glass and the wallpaper
  is blurred behind it.
- `glassBlur: 0` in config makes the wallpaper sharp through the tint.
- `glassTransparency: 0` makes the dock almost opaque white.
- Turning off transparency effects or turning on energy saver makes the dock
  solid light, with no black rectangle.
- Windows 10: a translucent light tint without blur, no crash, and no black.
- `config.json` saved with `"appearance":"auto"` and `version 1` loads as
  Glass, and the file is rewritten with `version 2`.
- The startup log line shows the capabilities.
- Icons, magnification, auto-hide, drag, and menus behave exactly as before.

---

## 3. Part B: FRONTEND (after Part A is merged into the branch)

Part B covers everything you can *see*. For the dock, that is Rust composition
constants in `scene.rs` and `menubar.rs`. For Settings, it is React, CSS, and
i18n.

### B1. Dock visuals for `Material::Glass` (`apps/dock/src/scene.rs`)

Tune `Theme::new`'s Glass values against `objetivo.png`. Change them only for
Glass, so the old styles look identical.

- **Tint**: keep Part A's formula. If needed, add a very slight cool cast
  (`rgb(250,251,255)`).
- **Sheen**: soft, about alpha 40 fading by 0.55 (the existing gradient).
- **Rim**: thin and light. Use the existing 1·scale stroke with
  `rim ≈ [170, 90, 70, 130]`, so it is brighter at the top-left corner and
  never darker than the tint. The goal is the "thin semi-transparent light
  border", **not** the thick dark border from `actual.png`.
- **Shadow** (`Scene::build`): add Glass-specific fields to `Theme`, for
  example `shadow_blur` and `shadow_offset`, instead of changing the shared
  constants. Use roughly blur 30·s, offset 6·s, colour `rgba(0,0,0,40)`.
- **Corner radius**: currently `layout.height * 0.36`, which is already close
  to macOS. Optionally make it a `Theme` field, about 0.30 to 0.33 for Glass,
  only if the comparison calls for it.
- **Separators**: for Glass, a light hairline, `rgba(255,255,255,110)` over
  `rgba(0,0,0,28)` or just the light colour, so they read as thin light
  lines on glass. The width stays `Metrics.separator_width` (1 px·s, in
  `muelle-core/layout.rs`). Do **not** change the layout, so icon positions
  stay identical.
- **Running dots**: smaller and subtler for Glass. Use about `3.5·s` (keep
  `round()`), colour `rgba(0,0,0,140)` on light glass, and keep the
  `indicator_y` gap. To keep this per-theme, add `indicator_size: f32` to
  `Theme`. Existing themes keep 4.0.
- **Labels and menus**: Glass resolves `light = true`, so they already use
  dark text. Check legibility of hover labels and popups (`Theme::for_popup`,
  `menu::popup_tint`).
- Check that group panels (`Theme::panel`, group view, window picker) look
  right with Glass. `panel_tint` for Glass should be the tint with alpha
  `max(tint, 150)` so text stays readable, like `Clear` does.

### B2. Menu bar (`apps/dock/src/menubar.rs`, `Theme::for_bar`)

- Tune the Glass bar tint so the clock and status items stay readable over
  bright and dark wallpapers, for example light alpha around 120 plus
  backdrop. Confirm the island (notch or bubble) still reads well.

### B3. Settings UI (`apps/settings/src`)

- `config.ts`: `Appearance` gets `"glass"`. Add `glassTransparency: number`
  and `glassBlur: number` to `Config`. Add `export const GLASS_LEVEL = { min: 0, max: 100 } as const;`.
  Add `glassSupport()` calling `invoke("glass_support")`, plus
  `openTransparencySettings` if Part A added it.
- `App.tsx`:
  - `THEMES` = `["glass", "auto", "light", "dark", "frosted", "clear", "aurora"]`,
    with Glass first as the new default. Add a `themeName` case. The switch is
    already exhaustive by return type; keep it that way, with no `default`.
  - `config?.appearance ?? "auto"` and `config.appearance ?? "auto"` become
    `?? "glass"` (L249 and L364).
  - `windowTheme("glass")` returns light.
  - Under the theme picker, show **only when Glass is selected**: a
    "Transparency" `Slider` and a "Blur" `Slider` (`unit="%"`, 0 to 100,
    `update({ glassTransparency })` and `update({ glassBlur })`).
  - Use `glassSupport()`: if `!windows11`, disable or annotate Blur with a
    hint ("Windows 10: transparency without blur"). If
    `!transparencyEffects || energySaver`, show a hint with a link to
    Windows settings.
- `visuals.tsx`: `Theme` already excludes `"auto"`, so `"glass"` flows
  through. Optionally pass the slider levels to `ThemePreview`, so the
  preview reflects them through CSS variables.
- `styles.css` (~L561-579): add `.theme-layer[data-theme="glass"]` with
  `--dock: rgba(255,255,255,.30)`, `--bar: rgba(255,255,255,.45)`,
  `--rim: rgba(255,255,255,.6)`, `--blur: blur(8px) saturate(1.6)`, a softer
  shadow, and light separators and dots if they are drawn. When levels are
  passed, drive `--dock` alpha and the `--blur` radius from inline CSS
  variables.
- `i18n/*.json`: all **8** files (`es en fr pt zh hi ar bn`) need
  `themeGlass`, `glassTransparency`, `glassBlur`, `glassBlurUnavailable`,
  `glassTransparencyOff`. Also update `themeHint` to mention Glass. The
  `i18n.test.mjs` test checks that keys match across languages.
- `icons.tsx`: optionally add a `glass` setting icon for the slider rows.

### B4. Settings tests (`apps/settings/tests/app.test.mjs`)

- Update the theme picker test: the expected list includes `"glass"` first.
  A fixture without `appearance` reads as `"glass"`. Choosing it saves only
  `{ appearance: "glass" }`.
- New test: the sliders render only for Glass. Moving one saves only
  `{ glassTransparency }` or `{ glassBlur }`.
- New test: with `glass_support` returning `windows11: false`, the blur hint
  appears. Stub the invoke the same way existing commands are stubbed.
- Update the base fixture (L35) so it has `appearance: "glass"`,
  `glassTransparency: 65`, and `glassBlur: 100`.

### B5. Part B risks

- **Visual regressions in the old themes.** Every change must be scoped to
  `Material::Glass`. Compare the old themes before and after on Windows.
- **Legibility** of the light glass over a bright or white wallpaper. The
  rim, shadow, and transparency floor must keep the outline visible.
- **Layout invariance**: do not touch `muelle-core` metrics. Separators and
  dots change only colour and size inside the slots they already have.
- The website copy in `muelle/src/styles/dock.css` is out of scope.

### B6. Part B acceptance checks

```bash
export PATH=/usr/lib/llvm-18/bin:$PATH
cargo fmt --all --check
cargo xwin clippy --workspace --all-targets --target x86_64-pc-windows-msvc -- -D warnings
cargo xwin test --no-run -p muelle --target x86_64-pc-windows-msvc
pnpm -C apps/settings install --frozen-lockfile
pnpm -C apps/settings build        # tsc --noEmit + vite build
pnpm -C apps/settings test         # node --test (21 tests pass at baseline)
```

Visual checks, on Windows 11, compared against `objetivo.png`:

- The dock is light, see-through glass with the wallpaper blurred behind it.
- The rim is thin and light. There is no dark outline.
- The shadow is soft. Separators are thin light lines. Dots are small and
  subtle.
- Every old theme looks identical to before.
- In Settings, Glass is selected by default and the sliders update the dock
  live, within about 100 ms.
- On Windows 10 the dock shows translucent glass without blur and Settings
  shows the hint.
- Screenshots of the before and after go in the PR.

---

## 4. Order and handoff

1. Part A lands as commits on `feat/glass-dock`. CI must be green and the A8
   checks must pass. The Glass theme then works with placeholder visuals.
2. Part B starts from the tip of the branch and touches only the files listed
   in section 3. The mechanics from A4 are already there; B only changes
   Glass constants and adds per-theme fields.
3. Add one `CHANGELOG.md` entry, written in Part B: "New Glass theme
   (default), with transparency and blur controls".
