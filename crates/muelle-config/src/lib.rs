//! Persistent configuration stored in `%APPDATA%\Nikao Dock\config.json`.

use std::{
    env,
    fs::{self, File, TryLockError},
    io,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const CURRENT_VERSION: u32 = 4;

/// Folder under `%APPDATA%` and `%LOCALAPPDATA%`.
/// The previous folder was `Muelle`; [`migrate_legacy_folder`] copies it once.
pub const DATA_DIR: &str = "Nikao Dock";
const LEGACY_DATA_DIR: &str = "Muelle";
const OLDER_DATA_DIR: &str = "WinDock";

/// Error reading or saving the configuration.
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("no se pudo acceder a `{path}`")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("`{path}` no es una configuración válida")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("no se pudo serializar la configuración")]
    Serialize(#[from] serde_json::Error),
}

/// User settings. Missing fields use their default values, so adding fields
/// does not break existing configuration files.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    pub version: u32,
    /// Pinned items in dock order: apps, and the ids of `groups`.
    pub pinned: Vec<String>,
    /// Pinned apps gathered in one dock icon each.
    pub groups: Vec<DockGroup>,
    pub recent: Vec<String>,
    /// The apps pinned to the Windows taskbar when the dock last looked, to
    /// bring pins and unpins made there (even while Muelle was closed) to the
    /// dock. `None` until it first looks.
    pub taskbar_pins: Option<Vec<String>>,
    #[serde(alias = "auto_hide")]
    pub auto_hide: bool,
    /// Hide only when the foreground app overlaps the dock; leave desktop visible.
    /// Missing in an existing file means the legacy always-visible mode. New
    /// installations and Reset use Config::default(), which enables this mode.
    #[serde(default)]
    pub smart_hide: bool,
    /// Icon side at rest, in logical pixels.
    pub icon_size: u32,
    pub magnification: bool,
    /// Side of the icon under the cursor, in logical pixels.
    pub magnified_size: u32,
    /// The screen edge the dock sits on. Bottom by default, as on macOS;
    /// never the top, which belongs to the menu bar.
    pub dock_position: DockPosition,
    pub dock_density: DockDensity,
    pub dock_auto_hide_speed: DockAutoHideSpeed,
    pub show_start: bool,
    pub show_trash: bool,
    pub show_downloads: bool,
    /// Keep Explorer pinned; otherwise it appears only while its windows are open.
    pub pin_explorer: bool,
    /// Genie effect when minimizing. On by default; while enabled, the dock
    /// turns off the system's own window transitions (see plan, "Efecto genio").
    pub genie: bool,
    /// How long the genie takes (issue #27). `Normal` is the original speed.
    pub genie_speed: GenieSpeed,
    /// Minimized windows get a live thumbnail in the right section of the
    /// dock. Off by default: they go into their app's icon instead.
    pub show_minimized: bool,
    /// Include shell-cloaked windows from Windows/tiling-WM workspaces.
    /// Off by default to preserve the current-desktop taskbar behavior.
    pub show_other_workspaces: bool,
    /// An app's right-click menu starts with its jump list (pinned and recent
    /// items, tasks), as on the Windows taskbar. Off, the menu has only the
    /// dock's own items and no jump list is read.
    pub jump_lists: bool,
    /// macOS-style bar across the top with the clock and status items.
    pub menu_bar: bool,
    /// Keep the menu bar off screen until the pointer touches the top edge.
    pub menu_bar_auto_hide: bool,
    /// Show supported AI providers' plan usage in the menu bar.
    pub ai_usage: bool,
    /// Show what is playing (title, controls) in the menu bar. With this and
    /// `ai_usage` off there is no island at all (issue #28).
    pub menu_bar_music: bool,
    /// Visual style and placement of the music and AI usage island in the menu bar.
    pub menu_bar_island_style: IslandStyle,
    /// A verse of encouragement in the menu bar, a different one each day.
    pub menu_bar_verse: bool,
    /// Opt-in peripheral monitoring. Off means no accessory reader or queries.
    pub accessory_battery: bool,
    /// The dot under apps that have windows open.
    pub show_indicators: bool,
    /// The section of recently used apps that are not pinned.
    pub show_recents: bool,
    /// Icons bounce while the app they open is starting.
    pub bounce_on_open: bool,
    /// The crab comes out for moments worth it (an AI plan resetting or
    /// running out, the charger, energy saver).
    pub mascot: bool,
    /// Settings asks GitHub Releases for a newer version when it opens. The
    /// only network access in Muelle, so it can be turned off.
    pub check_updates: bool,
    /// With `check_updates`: once a day, while the user is away from the
    /// computer, new versions are downloaded and installed without asking.
    pub auto_update: bool,
    pub language: Language,
    /// The look of the dock, the menu bar and their menus and panels.
    /// New installs and Reset use Glass. `Auto` follows Windows' light or dark mode.
    pub appearance: Appearance,
    /// How see-through the Glass theme is, from 0 to 100. 100 is the most
    /// transparent. Only the Glass theme uses it.
    pub glass_transparency: u8,
    /// How much of the system blur the Glass theme shows, from 0 to 100.
    /// 0 leaves the wallpaper sharp. Only the Glass theme uses it.
    pub glass_blur: u8,
    /// A folder shown at the right of the dock whose contents open as a
    /// menu, with a submenu for each folder inside, like a Windows 10 taskbar
    /// toolbar. Only one: the user arranges the folder as they like.
    pub shortcuts_folder: Option<String>,
    /// Where the menu-bar magnifier opens. Not `menuBarSearch`: that name was
    /// a retired boolean for showing the Windows taskbar search button.
    pub search_mode: SearchMode,
    /// Indicator on app icons when Windows requests attention. Their icon
    /// always bounces too, and window previews always have a close button:
    /// those two switches were retired, and their saved values are ignored.
    pub notification_badges: bool,
}

/// Pinned apps the user gathered in one dock icon by dropping one on
/// another. Its `id` stands for it in `pinned`, in its place.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
#[serde(rename_all = "camelCase")]
pub struct DockGroup {
    pub id: String,
    pub apps: Vec<String>,
}

/// Speed of the genie. Three steps, not a slider: a duration in milliseconds
/// would ask the user to guess, and these three cover what anyone asked for.
/// The screen edge the dock sits on: the three macOS offers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DockPosition {
    Left,
    Right,
    /// Also what an unknown value from a newer version reads as (serde
    /// needs this fallback last).
    #[default]
    #[serde(other)]
    Bottom,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GenieSpeed {
    Faster,
    Normal,
    /// Also what an unknown value from a newer version reads as (serde needs
    /// the fallback variant last).
    #[default]
    #[serde(other)]
    Fast,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DockDensity {
    Compact,
    Spacious,
    #[default]
    #[serde(other)]
    Normal,
}

impl DockDensity {
    /// Icon spacing and inner padding, in logical pixels, independent of icon size.
    #[must_use]
    pub const fn spacing(self) -> (f32, f32) {
        match self {
            Self::Compact => (8.0, 6.0),
            Self::Normal => (12.0, 9.0),
            Self::Spacious => (16.0, 12.0),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DockAutoHideSpeed {
    Smooth,
    Fast,
    #[default]
    #[serde(other)]
    Normal,
}

impl DockAutoHideSpeed {
    /// Frequency of the existing critically damped slide spring; dwell is unchanged.
    #[must_use]
    pub const fn omega(self) -> f32 {
        match self {
            Self::Smooth => 16.0,
            Self::Normal => 24.0,
            Self::Fast => 36.0,
        }
    }
}

impl GenieSpeed {
    /// How long one genie flight lasts, minimizing or restoring.
    #[must_use]
    pub fn seconds(self) -> f32 {
        match self {
            Self::Normal => 0.5,
            Self::Fast => 0.35,
            Self::Faster => 0.25,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IslandStyle {
    /// Compact rounded bubble placed next to the right-side status indicators.
    Bubble,
    /// Centered curved notch hanging from the top of the menu bar.
    #[default]
    #[serde(other)]
    Notch,
}

/// Where the menu-bar magnifier opens.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SearchMode {
    /// The Windows Search flyout (Win+S), which opens at the bottom.
    Windows,
    /// Muelle's own panel, under the menu bar. Also what an unknown value reads as.
    #[default]
    #[serde(other)]
    Spotlight,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    Es,
    En,
    Fr,
    Zh,
    Hi,
    Ar,
    Bn,
    Pt,
    /// Follow the Windows display language. Also what a language added by a
    /// newer version reads as, so the file stays valid for older versions
    /// (serde requires this fallback to be the last variant).
    #[default]
    #[serde(other)]
    Auto,
}

/// Appearance values understood in saved settings. While
/// transparency effects are off, or energy saver is on, they are drawn solid.
/// On Windows 10, Glass can still be a translucent tint without blur when
/// transparency is allowed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    Light,
    Dark,
    /// Milky glass with a fine grain (issue #50). Dark content.
    Frosted,
    /// Barely tinted glass with a bright rim. Light content.
    Clear,
    /// Glass tinted with the colours of the northern lights. Light content.
    Aurora,
    /// Light glass with dark content. It does not follow Windows dark mode.
    /// The dock uses this appearance.
    Glass,
    /// Follow Windows' light or dark mode. Also what a theme added by a newer
    /// version reads as (serde needs this fallback last).
    #[default]
    #[serde(other)]
    Auto,
}

pub const ICON_SIZE_RANGE: (u32, u32) = (32, 96);
pub const MAX_MAGNIFIED_SIZE: u32 = 160;
/// Upper bound of [`Config::glass_transparency`] and [`Config::glass_blur`].
pub const GLASS_LEVEL_MAX: u8 = 100;
/// Default [`Config::glass_transparency`]. 100 is the most see-through.
/// 90 is clear Liquid Glass (low tint, ~10% white) over soft blur. Earlier
/// Glass builds shipped 65 (version 2) and 80 (version 3); those exact stored
/// defaults move here once on load.
pub const GLASS_TRANSPARENCY_DEFAULT: u8 = 90;
/// Default [`Config::glass_blur`]. 0 is no blur. 45 keeps a Clear-iOS soft
/// blur instead of heavy frosting; a stored 100 from version 3 moves here once.
pub const GLASS_BLUR_DEFAULT: u8 = 45;

impl Config {
    /// Either policy needs a reveal strip and leaves maximized windows the full work area.
    pub fn hides_automatically(&self) -> bool {
        self.auto_hide || self.smart_hide
    }

    /// Brings out-of-range values (a hand-edited file) back into range.
    #[must_use]
    pub fn sanitized(mut self) -> Self {
        self.icon_size = self.icon_size.clamp(ICON_SIZE_RANGE.0, ICON_SIZE_RANGE.1);
        self.magnified_size = self
            .magnified_size
            .clamp(self.icon_size, MAX_MAGNIFIED_SIZE);
        self.shortcuts_folder = self
            .shortcuts_folder
            .map(|folder| folder.trim().to_owned())
            .filter(|folder| !folder.is_empty());
        self.glass_transparency = self.glass_transparency.min(GLASS_LEVEL_MAX);
        self.glass_blur = self.glass_blur.min(GLASS_LEVEL_MAX);
        self
    }

    /// One-time moves onto the current Glass defaults.
    ///
    /// Older themes switch to Glass, the appearance now offered by Settings.
    /// Version 2 stored transparency 65; version 3 stored transparency 80 and
    /// blur 100. Only those exact shipped defaults move onto the current Clear
    /// Liquid Glass defaults; any other value is a user choice and stays.
    /// Returns whether the file should be written back.
    fn migrated(mut self) -> (Self, bool) {
        let mut changed = self.appearance != Appearance::Glass;
        self.appearance = Appearance::Glass;
        if self.version < 2 {
            self.version = 2;
            changed = true;
        }
        if self.version < 3 {
            if self.glass_transparency == 65 {
                self.glass_transparency = GLASS_TRANSPARENCY_DEFAULT;
            }
            self.version = 3;
            changed = true;
        }
        if self.version < 4 {
            if self.glass_transparency == 80 {
                self.glass_transparency = GLASS_TRANSPARENCY_DEFAULT;
            }
            if self.glass_blur == 100 {
                self.glass_blur = GLASS_BLUR_DEFAULT;
            }
            self.version = 4;
            changed = true;
        }
        (self, changed)
    }

    /// Scale of the icon under the cursor (1.0 = no magnification).
    #[must_use]
    pub fn max_scale(&self) -> f32 {
        if self.magnification {
            self.magnified_size as f32 / self.icon_size as f32
        } else {
            1.0
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: CURRENT_VERSION,
            pinned: Vec::new(),
            groups: Vec::new(),
            recent: Vec::new(),
            taskbar_pins: None,
            auto_hide: false,
            smart_hide: true,
            icon_size: 48,
            magnification: true,
            dock_position: DockPosition::Bottom,
            dock_density: DockDensity::Normal,
            dock_auto_hide_speed: DockAutoHideSpeed::Normal,
            show_start: true,
            show_trash: true,
            show_downloads: true,
            pin_explorer: true,
            magnified_size: 77,
            genie: true,
            genie_speed: GenieSpeed::Fast,
            show_minimized: false,
            show_other_workspaces: false,
            jump_lists: true,
            menu_bar: true,
            menu_bar_auto_hide: false,
            ai_usage: true,
            menu_bar_music: true,
            menu_bar_island_style: IslandStyle::Notch,
            menu_bar_verse: true,
            accessory_battery: false,
            show_indicators: true,
            show_recents: true,
            bounce_on_open: true,
            mascot: true,
            check_updates: true,
            auto_update: true,
            language: Language::Auto,
            appearance: Appearance::Glass,
            glass_transparency: GLASS_TRANSPARENCY_DEFAULT,
            glass_blur: GLASS_BLUR_DEFAULT,
            shortcuts_folder: None,
            search_mode: SearchMode::Spotlight,
            notification_badges: true,
        }
    }
}

/// Copies settings and pins into [`DATA_DIR`].
///
/// `WinDock` is renamed to `Muelle` first, matching the older move. `Muelle`
/// is then copied, not renamed, so an old install that is still on disk keeps
/// its folder until the user removes it. A destination that already exists is
/// left alone. Pin paths inside `config.json` are rewritten to the copy.
pub fn migrate_legacy_folder() {
    let roaming = PathBuf::from(env::var_os("APPDATA").unwrap_or_default());
    if roaming.as_os_str().is_empty() {
        return;
    }
    migrate_roaming(&roaming);
}

fn migrate_roaming(roaming: &Path) {
    let older = roaming.join(OLDER_DATA_DIR);
    let legacy = roaming.join(LEGACY_DATA_DIR);
    if older.is_dir() && !legacy.exists() {
        let _ = fs::rename(&older, &legacy);
    }
    let current = roaming.join(DATA_DIR);
    if legacy.is_dir() && !current.exists() {
        let _ = copy_tree(&legacy, &current);
    } else if current.is_dir() {
        let _ = rewrite_config_paths(&current.join("config.json"), &legacy, &current);
    }
}

/// `Ok(true)` when this call created `to`. `Ok(false)` when `to` already
/// existed, including when another process won the race.
fn copy_tree(from: &Path, to: &Path) -> io::Result<bool> {
    if to.exists() {
        return Ok(false);
    }
    let Some(name) = to.file_name() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "destination has no name",
        ));
    };
    let staging = to.with_file_name(format!(
        ".{}.partial-{}",
        name.to_string_lossy(),
        std::process::id()
    ));
    if staging.exists() {
        fs::remove_dir_all(&staging)?;
    }
    if let Err(error) = copy_children(from, &staging) {
        let _ = fs::remove_dir_all(&staging);
        return Err(error);
    }
    // Rewrite before the rename so the published folder already points at `to`.
    let _ = rewrite_config_paths(&staging.join("config.json"), from, to);
    match fs::rename(&staging, to) {
        Ok(()) => Ok(true),
        Err(_) if to.exists() => {
            let _ = fs::remove_dir_all(&staging);
            Ok(false)
        }
        Err(error) => {
            let _ = fs::remove_dir_all(&staging);
            Err(error)
        }
    }
}

fn copy_children(from: &Path, to: &Path) -> io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let target = to.join(entry.file_name());
        if kind.is_dir() {
            copy_children(&entry.path(), &target)?;
        } else if kind.is_file() {
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

fn rewrite_config_paths(file: &Path, from: &Path, to: &Path) -> io::Result<()> {
    let text = match fs::read_to_string(file) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let updated = replace_ci(&text, &json_path(from), &json_path(to));
    let updated = replace_ci(
        &updated,
        from.to_string_lossy().as_ref(),
        to.to_string_lossy().as_ref(),
    );
    if updated != text {
        fs::write(file, updated)?;
    }
    Ok(())
}

fn json_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "\\\\")
}

/// ASCII case-insensitive replace. Windows stores the same folder with either case.
fn replace_ci(text: &str, from: &str, to: &str) -> String {
    if from.is_empty() {
        return text.to_owned();
    }
    let folded = text.to_ascii_lowercase();
    let needle = from.to_ascii_lowercase();
    let mut out = String::with_capacity(text.len());
    let mut start = 0;
    while let Some(offset) = folded[start..].find(&needle) {
        let at = start + offset;
        out.push_str(&text[start..at]);
        out.push_str(to);
        start = at + from.len();
    }
    out.push_str(&text[start..]);
    out
}

#[must_use]
pub fn default_path() -> PathBuf {
    PathBuf::from(env::var_os("APPDATA").unwrap_or_default())
        .join(DATA_DIR)
        .join("config.json")
}

/// Reads the configuration. Returns `Ok(None)` if the file does not exist yet.
///
/// A version-1 `Auto` theme becomes `Glass` in memory. This does not write:
/// [`update`] calls it while holding the file lock. [`load_and_persist`]
/// saves that one-time migration.
pub fn load(path: &Path) -> Result<Option<Config>, ConfigError> {
    Ok(read(path)?.map(|(config, _)| config))
}

/// Reads the configuration and, when it was still on an older schema, writes
/// the migrated value back. The write re-reads under the same lock as other
/// saves, so a settings change made in between is not replaced by the copy
/// this function read first.
pub fn load_and_persist(path: &Path) -> Result<Option<Config>, ConfigError> {
    let Some((config, migrated)) = read(path)? else {
        return Ok(None);
    };
    if !migrated {
        return Ok(Some(config));
    }
    // A failed write must not discard the settings we already read. The next
    // successful save persists the migration.
    match update(path, |saved| Ok(saved.unwrap_or_else(|| config.clone()))) {
        Ok(saved) => Ok(Some(saved)),
        Err(error) => {
            eprintln!("muelle: no se pudo guardar la migración del tema: {error}");
            Ok(Some(config))
        }
    }
}

fn read(path: &Path) -> Result<Option<(Config, bool)>, ConfigError> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(ConfigError::Io {
                path: path.to_owned(),
                source,
            });
        }
    };
    let config: Config = serde_json::from_str(&text).map_err(|source| ConfigError::Parse {
        path: path.to_owned(),
        source,
    })?;
    Ok(Some(config.sanitized().migrated()))
}

/// Replaces the configuration under the same lock used by incremental edits.
pub fn save(path: &Path, config: &Config) -> Result<(), ConfigError> {
    let _lock = lock_for_write(path)?;
    write_config(path, config)
}

/// Reads, changes and publishes one version while other writers wait. Readers
/// can still load the last complete version without acquiring the lock.
pub fn update(
    path: &Path,
    edit: impl FnOnce(Option<Config>) -> Result<Config, ConfigError>,
) -> Result<Config, ConfigError> {
    let _lock = lock_for_write(path)?;
    let config = edit(load(path)?)?.sanitized();
    write_config(path, &config)?;
    Ok(config)
}

/// Updates only preferences explicitly changed in Settings. The roster and
/// schema version belong to the dock and may have changed since Settings opened.
pub fn update_preferences(
    path: &Path,
    patch: serde_json::Map<String, serde_json::Value>,
) -> Result<Config, ConfigError> {
    update(path, |saved| {
        let mut fields = serde_json::to_value(saved.unwrap_or_default())?;
        if let Some(fields) = fields.as_object_mut() {
            for (key, value) in patch {
                if fields.contains_key(&key)
                    && !matches!(
                        key.as_str(),
                        "version" | "appearance" | "pinned" | "groups" | "recent" | "taskbarPins"
                    )
                {
                    fields.insert(key, value);
                }
            }
        }
        serde_json::from_value(fields).map_err(|source| ConfigError::Parse {
            path: path.to_owned(),
            source,
        })
    })
}

fn lock_for_write(path: &Path) -> Result<File, ConfigError> {
    let io_error = |source| ConfigError::Io {
        path: path.to_owned(),
        source,
    };
    if let Some(folder) = path.parent() {
        fs::create_dir_all(folder).map_err(io_error)?;
    }
    // Lock a separate file so the atomic rename cannot replace the lock.
    // Closing the handle releases it, including when a process exits.
    let lock = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path.with_extension("json.lock"))
        .map_err(io_error)?;
    let start = Instant::now();
    loop {
        match lock.try_lock() {
            Ok(()) => return Ok(lock),
            Err(TryLockError::WouldBlock) if start.elapsed() < Duration::from_millis(80) => {
                thread::sleep(Duration::from_millis(4));
            }
            Err(TryLockError::WouldBlock) => {
                return Err(io_error(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "Ajustes y el dock están guardando la configuración",
                )));
            }
            Err(TryLockError::Error(error)) => return Err(io_error(error)),
        }
    }
}

fn write_config(path: &Path, config: &Config) -> Result<(), ConfigError> {
    let io_error = |source| ConfigError::Io {
        path: path.to_owned(),
        source,
    };
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, serde_json::to_string_pretty(config)?).map_err(io_error)?;
    fs::rename(&temporary, path).map_err(io_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retired_bluetooth_visibility_does_not_override_system_status()
    -> Result<(), serde_json::Error> {
        for old in [false, true] {
            let config: Config = serde_json::from_value(
                serde_json::json!({"menuBarBluetooth": old, "showStart": false}),
            )?;
            assert!(!config.show_start);
            assert!(
                serde_json::to_value(config)?
                    .get("menuBarBluetooth")
                    .is_none()
            );
        }
        Ok(())
    }

    #[test]
    fn other_workspaces_are_opt_in_and_preserved_when_saving() -> Result<(), serde_json::Error> {
        let legacy: Config = serde_json::from_str("{}")?;
        assert!(!legacy.show_other_workspaces);
        let enabled: Config = serde_json::from_str(r#"{"showOtherWorkspaces":true}"#)?;
        let restored: Config = serde_json::from_str(&serde_json::to_string(&enabled)?)?;
        assert!(restored.show_other_workspaces);
        Ok(())
    }

    #[test]
    fn new_installs_use_intelligent_hiding_and_existing_modes_are_preserved()
    -> Result<(), serde_json::Error> {
        let fresh = Config::default();
        assert!(!fresh.auto_hide && fresh.smart_hide);
        let round_trip: Config = serde_json::from_str(&serde_json::to_string(&fresh)?)?;
        assert_eq!(round_trip, fresh);
        for (json, auto, smart) in [
            (r#"{}"#, false, false),
            (r#"{"autoHide":false}"#, false, false),
            (r#"{"autoHide":true}"#, true, false),
            (r#"{"auto_hide":true}"#, true, false),
            (r#"{"autoHide":false,"smartHide":false}"#, false, false),
            (r#"{"autoHide":false,"smartHide":true}"#, false, true),
            (r#"{"autoHide":true,"smartHide":true}"#, true, true),
        ] {
            let existing: Config = serde_json::from_str(json)?;
            assert_eq!(
                (existing.auto_hide, existing.smart_hide),
                (auto, smart),
                "{json}"
            );
        }
        Ok(())
    }

    #[test]
    fn legacy_visible_mode_and_retired_banner_preferences_are_preserved()
    -> Result<(), serde_json::Error> {
        let old: Config = serde_json::from_value(serde_json::json!({
            "notifications": true, "notificationPosition": "top-right",
            "notificationDuration": 5, "notificationDnd": true,
            "notificationBadges": false, "autoHide": false,
            "notificationBounce": false, "showPreviewCloseButton": false
        }))?;
        assert!(!old.smart_hide && !old.hides_automatically());
        assert!(!old.notification_badges);
        let saved = serde_json::to_value(&old)?;
        assert!(saved.get("notifications").is_none());
        assert!(saved.get("notificationBounce").is_none());
        assert!(saved.get("showPreviewCloseButton").is_none());
        assert_eq!(saved.get("smartHide"), Some(&serde_json::json!(false)));
        Ok(())
    }

    #[test]
    fn explicitly_enabled_intelligent_hiding_survives_round_trip() -> Result<(), serde_json::Error>
    {
        let enabled: Config = serde_json::from_str(r#"{"smartHide":true,"autoHide":false}"#)?;
        let restored: Config = serde_json::from_str(&serde_json::to_string(&enabled)?)?;
        assert!(restored.smart_hide && restored.hides_automatically());
        Ok(())
    }

    #[test]
    fn older_configs_keep_the_original_dock_defaults() -> Result<(), serde_json::Error> {
        let config: Config = serde_json::from_str(r#"{"iconSize":64}"#)?;
        assert!(
            config.show_start && config.show_trash && config.show_downloads && config.pin_explorer
        );
        assert_eq!(config.dock_density.spacing(), (12.0, 9.0));
        assert_eq!(config.dock_auto_hide_speed.omega(), 24.0);
        Ok(())
    }

    #[test]
    fn dock_preferences_round_trip_and_unknown_presets_fall_back() -> Result<(), serde_json::Error>
    {
        let config: Config = serde_json::from_str(
            r#"{
            "showStart":false,"showTrash":false,"showDownloads":false,"pinExplorer":false,
            "dockDensity":"compact","dockAutoHideSpeed":"fast"
        }"#,
        )?;
        assert!(
            !config.show_start
                && !config.show_trash
                && !config.show_downloads
                && !config.pin_explorer
        );
        assert_eq!(config.dock_density.spacing(), (8.0, 6.0));
        assert_eq!(config.dock_auto_hide_speed.omega(), 36.0);
        assert_eq!(
            serde_json::from_str::<Config>(&serde_json::to_string(&config)?)?,
            config
        );
        let unknown: Config =
            serde_json::from_str(r#"{"dockDensity":"future","dockAutoHideSpeed":"future"}"#)?;
        assert_eq!(unknown.dock_density, DockDensity::Normal);
        assert_eq!(unknown.dock_auto_hide_speed, DockAutoHideSpeed::Normal);
        Ok(())
    }

    fn scratch(name: &str) -> PathBuf {
        let folder = env::temp_dir().join(format!("muelle-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&folder);
        folder.join("config.json")
    }

    #[test]
    fn load_should_return_none_when_file_is_missing() {
        assert_eq!(load(&scratch("missing")).ok(), Some(None));
    }

    #[test]
    fn save_then_load_should_round_trip() {
        let path = scratch("round-trip");
        let config = Config {
            pinned: vec!["explorer".into()],
            ..Config::default()
        };
        save(&path, &config).ok();
        assert_eq!(load(&path).ok(), Some(Some(config)));
    }

    #[test]
    fn settings_patch_keeps_dock_edits_made_since_opening() -> Result<(), ConfigError> {
        let path = scratch("settings-patch");
        let initial = Config::default();
        save(&path, &initial)?;
        update(&path, |saved| {
            let mut config = saved.unwrap_or_default();
            config.auto_hide = true;
            config.pinned.push("browser".into());
            Ok(config)
        })?;
        let patch = serde_json::Map::from_iter([
            ("iconSize".into(), 64.into()),
            ("pinned".into(), serde_json::Value::Array(Vec::new())),
        ]);
        let saved = update_preferences(&path, patch)?;
        assert!(saved.auto_hide);
        assert_eq!(saved.pinned, ["browser"]);
        assert_eq!(saved.icon_size, 64);
        Ok(())
    }

    #[test]
    fn accessory_monitoring_is_opt_in_and_its_preference_round_trips() -> Result<(), ConfigError> {
        let path = scratch("accessory-monitoring");
        assert!(!Config::default().accessory_battery);
        for enabled in [true, false] {
            let saved = update_preferences(
                &path,
                serde_json::Map::from_iter([("accessoryBattery".into(), enabled.into())]),
            )?;
            assert_eq!(saved.accessory_battery, enabled);
            assert_eq!(
                load(&path)?.map(|config| config.accessory_battery),
                Some(enabled)
            );
        }
        Ok(())
    }

    #[test]
    fn search_mode_defaults_to_spotlight_and_an_unknown_value_stays_there() {
        assert_eq!(Config::default().search_mode, SearchMode::Spotlight);
        let fresh: Config = serde_json::from_str("{}").unwrap_or_default();
        assert_eq!(fresh.search_mode, SearchMode::Spotlight);
        let windows: Config =
            serde_json::from_str(r#"{"searchMode":"windows"}"#).unwrap_or_default();
        assert_eq!(windows.search_mode, SearchMode::Windows);
        let unknown: Config = serde_json::from_str(r#"{"searchMode":"bing"}"#).unwrap_or_default();
        assert_eq!(unknown.search_mode, SearchMode::Spotlight);
        // The retired boolean must not be read as this setting.
        let retired: Config = serde_json::from_str(r#"{"menuBarSearch":true}"#).unwrap_or_default();
        assert_eq!(retired.search_mode, SearchMode::Spotlight);
    }

    #[test]
    fn retired_status_overrides_are_ignored_without_changing_other_preferences()
    -> Result<(), ConfigError> {
        let path = scratch("retired-status-overrides");
        save(&path, &Config::default())?;
        for old in [serde_json::Value::Null, false.into(), true.into()] {
            let mut patch = serde_json::Map::from_iter([
                ("menuBarSearch".into(), old.clone()),
                ("menuBarTaskView".into(), old.clone()),
                ("menuBarWidgets".into(), old.clone()),
                ("menuBarBatteryPercent".into(), old),
                ("showStart".into(), false.into()),
                ("accessoryBattery".into(), true.into()),
            ]);
            // Exercise loading an existing file, then saving a Settings patch.
            fs::write(&path, serde_json::to_vec(&patch)?).map_err(|source| ConfigError::Io {
                path: path.clone(),
                source,
            })?;
            let loaded = load(&path)?.unwrap_or_default();
            assert!(!loaded.show_start && loaded.accessory_battery);
            patch.insert("iconSize".into(), 64.into());
            let saved = update_preferences(&path, patch)?;
            assert!(!saved.show_start && saved.accessory_battery);
            assert_eq!(saved.icon_size, 64);
            let json = serde_json::to_value(load(&path)?.unwrap_or_default())?;
            for key in [
                "menuBarSearch",
                "menuBarTaskView",
                "menuBarWidgets",
                "menuBarBatteryPercent",
            ] {
                assert!(json.get(key).is_none(), "retired override {key}");
            }
        }
        Ok(())
    }

    #[test]
    fn settings_patch_keeps_the_dock_groups() -> Result<(), ConfigError> {
        let path = scratch("settings-patch-groups");
        let group = DockGroup {
            id: "muelle:group:1".into(),
            apps: vec!["a".into(), "b".into()],
        };
        save(
            &path,
            &Config {
                pinned: vec![group.id.clone()],
                groups: vec![group.clone()],
                ..Config::default()
            },
        )?;
        let patch = serde_json::Map::from_iter([
            ("iconSize".into(), 64.into()),
            ("groups".into(), serde_json::Value::Array(Vec::new())),
        ]);
        let saved = update_preferences(&path, patch)?;
        assert_eq!(saved.groups, [group]);
        assert_eq!(saved.icon_size, 64);
        Ok(())
    }

    #[test]
    fn a_busy_writer_keeps_the_last_complete_configuration() -> Result<(), ConfigError> {
        let path = scratch("write-contention");
        let original = Config::default();
        save(&path, &original)?;
        let _writer = lock_for_write(&path)?;
        let result = update(&path, |_| panic!("contending writer cannot edit"));
        assert!(
            matches!(result, Err(ConfigError::Io { source, .. }) if source.kind() == io::ErrorKind::WouldBlock)
        );
        assert_eq!(load(&path)?, Some(original));
        Ok(())
    }

    #[test]
    fn load_fills_missing_fields_and_preserves_legacy_visibility() {
        let path = scratch("partial");
        save(&path, &Config::default()).ok();
        fs::write(&path, "{}").ok();
        assert_eq!(
            load(&path).ok(),
            Some(Some(Config {
                smart_hide: false,
                ..Config::default()
            }))
        );
    }

    #[test]
    fn load_should_give_older_files_the_new_behavior_defaults() {
        let path = scratch("before-behavior");
        save(&path, &Config::default()).ok();
        fs::write(&path, r#"{"version":1,"iconSize":56}"#).ok();
        let config = load(&path).ok().flatten().unwrap_or_default();
        assert_eq!(config.icon_size, 56);
        assert!(config.show_indicators && config.show_recents && config.bounce_on_open);
        assert!(config.check_updates);
        // Files from before automatic updates get them, like new installs.
        assert!(config.auto_update);
        assert!(config.ai_usage);
        assert!(config.menu_bar_music);
        assert!(config.menu_bar_verse);
        assert!(!config.accessory_battery);
        assert!(!config.show_minimized);
        assert!(config.jump_lists);
    }

    #[test]
    fn load_should_read_dock_groups() {
        let path = scratch("groups");
        save(&path, &Config::default()).ok();
        fs::write(
            &path,
            r#"{"pinned":["a","muelle:group:1"],"groups":[{"id":"muelle:group:1","apps":["b","c"]}]}"#,
        )
        .ok();
        let config = load(&path).ok().flatten().unwrap_or_default();
        assert_eq!(
            config.groups,
            [DockGroup {
                id: "muelle:group:1".into(),
                apps: vec!["b".into(), "c".into()],
            }]
        );
    }

    #[test]
    fn load_should_read_show_minimized() {
        let path = scratch("show-minimized");
        save(&path, &Config::default()).ok();
        fs::write(&path, r#"{"showMinimized":true}"#).ok();
        let config = load(&path).ok().flatten().unwrap_or_default();
        assert!(config.show_minimized);
    }

    #[test]
    fn load_should_read_turned_off_update_checks() {
        let path = scratch("no-update-checks");
        save(&path, &Config::default()).ok();
        fs::write(&path, r#"{"checkUpdates":false}"#).ok();
        let config = load(&path).ok().flatten().unwrap_or_default();
        assert!(!config.check_updates);
    }

    #[test]
    fn load_should_read_the_behavior_settings() {
        let path = scratch("behavior");
        save(&path, &Config::default()).ok();
        fs::write(
            &path,
            r#"{"activeAppClick":"nothing","showRecents":false,"bounceOnOpen":false}"#,
        )
        .ok();
        let config = load(&path).ok().flatten().unwrap_or_default();
        assert!(!config.show_recents && !config.bounce_on_open);
    }

    #[test]
    fn supported_languages_round_trip_without_changing_other_settings()
    -> Result<(), serde_json::Error> {
        for (code, language) in [
            ("auto", Language::Auto),
            ("en", Language::En),
            ("es", Language::Es),
            ("fr", Language::Fr),
            ("zh", Language::Zh),
            ("hi", Language::Hi),
            ("ar", Language::Ar),
            ("bn", Language::Bn),
            ("pt", Language::Pt),
        ] {
            let value = serde_json::json!({"language": code, "iconSize": 56});
            let config: Config = serde_json::from_value(value)?;
            assert_eq!(config.language, language);
            assert_eq!(config.icon_size, 56);
            assert_eq!(serde_json::to_value(&config)?["language"], code);
        }
        Ok(())
    }

    #[test]
    fn load_should_read_french() {
        let path = scratch("french");
        save(&path, &Config::default()).ok();
        fs::write(&path, r#"{"language":"fr"}"#).ok();
        let config = load(&path).ok().flatten().unwrap_or_default();
        assert_eq!(config.language, Language::Fr);
    }

    #[test]
    fn load_should_read_an_unknown_language_as_auto() {
        let path = scratch("unknown-language");
        save(&path, &Config::default()).ok();
        fs::write(&path, r#"{"language":"de","iconSize":56}"#).ok();
        let config = load(&path).ok().flatten();
        assert_eq!(
            config.map(|config| (config.language, config.icon_size)),
            Some((Language::Auto, 56))
        );
    }

    #[test]
    fn load_should_read_the_shortcuts_folder() {
        let path = scratch("shortcuts-folder");
        save(&path, &Config::default()).ok();
        fs::write(&path, r#"{"shortcutsFolder":"D:\\Racc"}"#).ok();
        let config = load(&path).ok().flatten().unwrap_or_default();
        assert_eq!(config.shortcuts_folder.as_deref(), Some(r"D:\Racc"));
    }

    #[test]
    fn sanitized_should_forget_a_blank_shortcuts_folder() {
        let config = Config {
            shortcuts_folder: Some("  ".into()),
            ..Config::default()
        };
        assert_eq!(config.sanitized().shortcuts_folder, None);
    }

    #[test]
    fn load_should_fail_on_invalid_json() {
        let path = scratch("invalid");
        save(&path, &Config::default()).ok();
        fs::write(&path, "esto no es json").ok();
        assert!(matches!(load(&path), Err(ConfigError::Parse { .. })));
    }

    #[test]
    fn sanitized_should_clamp_icon_size() {
        let config = Config {
            icon_size: 500,
            ..Config::default()
        };
        assert_eq!(config.sanitized().icon_size, ICON_SIZE_RANGE.1);
    }

    #[test]
    fn sanitized_should_never_magnify_below_the_icon_size() {
        let config = Config {
            icon_size: 80,
            magnified_size: 40,
            ..Config::default()
        };
        assert_eq!(config.sanitized().magnified_size, 80);
    }

    #[test]
    fn max_scale_should_be_one_when_magnification_is_off() {
        let config = Config {
            magnification: false,
            ..Config::default()
        };
        assert_eq!(config.max_scale(), 1.0);
    }

    #[test]
    fn genie_should_be_on_by_default() {
        assert!(Config::default().genie);
        assert_eq!(Config::default().genie_speed, GenieSpeed::Fast);
    }

    #[test]
    fn the_dock_stays_at_the_bottom_unless_moved() {
        for json in [r#"{}"#, r#"{"dockPosition":"top"}"#] {
            let config: Config = serde_json::from_str(json).unwrap_or_default();
            assert_eq!(config.dock_position, DockPosition::Bottom);
        }
        let left: Config = serde_json::from_str(r#"{"dockPosition":"left"}"#).unwrap_or_default();
        assert_eq!(left.dock_position, DockPosition::Left);
    }

    #[test]
    fn deserialization_recognizes_legacy_appearance_values() -> Result<(), serde_json::Error> {
        // A file with no theme uses Glass. A theme this version does not know
        // stays `Auto`, which is what a newer build's value reads as.
        let fresh: Config = serde_json::from_str("{}")?;
        assert_eq!(fresh.appearance, Appearance::Glass);
        assert_eq!(
            (fresh.glass_transparency, fresh.glass_blur),
            (GLASS_TRANSPARENCY_DEFAULT, GLASS_BLUR_DEFAULT)
        );
        let unknown: Config = serde_json::from_str(r#"{"appearance":"sepia"}"#)?;
        assert_eq!(unknown.appearance, Appearance::Auto);
        let frosted: Config = serde_json::from_str(r#"{"appearance":"frosted"}"#)?;
        assert_eq!(frosted.appearance, Appearance::Frosted);
        Ok(())
    }

    #[test]
    fn glass_appearance_round_trips() -> Result<(), serde_json::Error> {
        let config: Config = serde_json::from_str(
            r#"{"appearance":"glass","glassTransparency":90,"glassBlur":45}"#,
        )?;
        assert_eq!(config.appearance, Appearance::Glass);
        assert_eq!(config.glass_transparency, GLASS_TRANSPARENCY_DEFAULT);
        assert_eq!(config.glass_blur, GLASS_BLUR_DEFAULT);
        let encoded = serde_json::to_value(&config)?;
        assert_eq!(
            encoded.get("appearance").and_then(|value| value.as_str()),
            Some("glass")
        );
        assert_eq!(
            encoded
                .get("glassTransparency")
                .and_then(|value| value.as_u64()),
            Some(u64::from(GLASS_TRANSPARENCY_DEFAULT))
        );
        assert_eq!(
            encoded.get("glassBlur").and_then(|value| value.as_u64()),
            Some(u64::from(GLASS_BLUR_DEFAULT))
        );
        let restored: Config = serde_json::from_value(encoded)?;
        assert_eq!(restored.appearance, Appearance::Glass);
        assert_eq!(restored.glass_transparency, config.glass_transparency);
        assert_eq!(restored.glass_blur, config.glass_blur);
        Ok(())
    }

    fn overwrite(path: &Path, json: &str) -> Result<(), ConfigError> {
        fs::write(path, json).map_err(|source| ConfigError::Io {
            path: path.to_owned(),
            source,
        })
    }

    fn required(path: &Path) -> Result<Config, ConfigError> {
        load(path)?.ok_or_else(|| ConfigError::Io {
            path: path.to_owned(),
            source: io::Error::new(io::ErrorKind::NotFound, "falta la configuración"),
        })
    }

    #[test]
    fn legacy_themes_switch_to_glass() -> Result<(), ConfigError> {
        let auto_path = scratch("migrate-auto");
        save(&auto_path, &Config::default())?;
        overwrite(
            &auto_path,
            r#"{"version":1,"appearance":"auto","iconSize":56}"#,
        )?;
        let migrated = required(&auto_path)?;
        assert_eq!(migrated.appearance, Appearance::Glass);
        assert_eq!(migrated.version, CURRENT_VERSION);
        assert_eq!(migrated.icon_size, 56);
        assert_eq!(migrated.glass_transparency, GLASS_TRANSPARENCY_DEFAULT);
        assert_eq!(migrated.glass_blur, GLASS_BLUR_DEFAULT);

        let dark_path = scratch("migrate-dark");
        save(&dark_path, &Config::default())?;
        overwrite(&dark_path, r#"{"version":1,"appearance":"dark"}"#)?;
        let dark = required(&dark_path)?;
        assert_eq!(dark.appearance, Appearance::Glass);
        assert_eq!(dark.version, CURRENT_VERSION);

        let kept = scratch("keep-auto");
        save(&kept, &Config::default())?;
        overwrite(&kept, r#"{"version":2,"appearance":"auto"}"#)?;
        let chosen = required(&kept)?;
        assert_eq!(chosen.appearance, Appearance::Glass);
        assert_eq!(chosen.version, CURRENT_VERSION);
        Ok(())
    }

    #[test]
    fn the_glass_migration_is_written_back_once() -> Result<(), ConfigError> {
        let path = scratch("persist-glass");
        save(&path, &Config::default())?;
        overwrite(
            &path,
            r#"{"version":4,"appearance":"aurora","iconSize":56}"#,
        )?;
        let config = load_and_persist(&path)?.ok_or_else(|| ConfigError::Io {
            path: path.clone(),
            source: io::Error::new(io::ErrorKind::NotFound, "falta la configuración"),
        })?;
        assert_eq!(config.appearance, Appearance::Glass);
        assert_eq!(config.version, CURRENT_VERSION);
        assert_eq!(config.icon_size, 56);
        let raw = fs::read_to_string(&path).map_err(|source| ConfigError::Io {
            path: path.clone(),
            source,
        })?;
        let stored: serde_json::Value =
            serde_json::from_str(&raw).map_err(|source| ConfigError::Parse {
                path: path.clone(),
                source,
            })?;
        assert_eq!(
            stored.get("appearance").and_then(|value| value.as_str()),
            Some("glass")
        );
        assert_eq!(
            stored.get("version").and_then(|value| value.as_u64()),
            Some(u64::from(CURRENT_VERSION))
        );
        assert_eq!(
            stored.get("iconSize").and_then(|value| value.as_u64()),
            Some(56)
        );

        let again = load_and_persist(&path)?.ok_or_else(|| ConfigError::Io {
            path: path.clone(),
            source: io::Error::new(io::ErrorKind::NotFound, "falta la configuración"),
        })?;
        assert_eq!(again.appearance, Appearance::Glass);
        let raw = fs::read_to_string(&path).map_err(|source| ConfigError::Io {
            path: path.clone(),
            source,
        })?;
        let stored: serde_json::Value =
            serde_json::from_str(&raw).map_err(|source| ConfigError::Parse {
                path: path.clone(),
                source,
            })?;
        assert_eq!(
            stored.get("appearance").and_then(|value| value.as_str()),
            Some("glass")
        );
        Ok(())
    }

    #[test]
    fn the_first_glass_transparency_becomes_the_clearer_default() -> Result<(), ConfigError> {
        let path = scratch("migrate-glass-clear");
        save(&path, &Config::default())?;
        overwrite(
            &path,
            r#"{"version":2,"appearance":"glass","glassTransparency":65,"glassBlur":40,"iconSize":56}"#,
        )?;
        let migrated = load_and_persist(&path)?.ok_or_else(|| ConfigError::Io {
            path: path.clone(),
            source: io::Error::new(io::ErrorKind::NotFound, "falta la configuración"),
        })?;
        assert_eq!(migrated.glass_transparency, GLASS_TRANSPARENCY_DEFAULT);
        assert_eq!(migrated.glass_blur, 40);
        assert_eq!(migrated.version, CURRENT_VERSION);
        assert_eq!(migrated.icon_size, 56);
        assert_eq!(migrated.appearance, Appearance::Glass);

        overwrite(
            &path,
            r#"{"version":2,"appearance":"glass","glassTransparency":40,"glassBlur":40}"#,
        )?;
        let kept = load_and_persist(&path)?.ok_or_else(|| ConfigError::Io {
            path: path.clone(),
            source: io::Error::new(io::ErrorKind::NotFound, "falta la configuración"),
        })?;
        assert_eq!(kept.glass_transparency, 40);
        assert_eq!(kept.glass_blur, 40);
        assert_eq!(kept.version, CURRENT_VERSION);

        overwrite(
            &path,
            r#"{"version":3,"appearance":"glass","glassTransparency":80,"glassBlur":100}"#,
        )?;
        let liquid = load_and_persist(&path)?.ok_or_else(|| ConfigError::Io {
            path: path.clone(),
            source: io::Error::new(io::ErrorKind::NotFound, "falta la configuración"),
        })?;
        assert_eq!(liquid.glass_transparency, GLASS_TRANSPARENCY_DEFAULT);
        assert_eq!(liquid.glass_blur, GLASS_BLUR_DEFAULT);
        assert_eq!(liquid.version, CURRENT_VERSION);

        overwrite(
            &path,
            r#"{"version":3,"appearance":"glass","glassTransparency":75,"glassBlur":70}"#,
        )?;
        let custom = load_and_persist(&path)?.ok_or_else(|| ConfigError::Io {
            path: path.clone(),
            source: io::Error::new(io::ErrorKind::NotFound, "falta la configuración"),
        })?;
        assert_eq!(custom.glass_transparency, 75);
        assert_eq!(custom.glass_blur, 70);
        assert_eq!(custom.version, CURRENT_VERSION);
        Ok(())
    }

    #[test]
    fn glass_levels_clamp_and_a_blur_edit_changes_only_that_field() -> Result<(), ConfigError> {
        let path = scratch("glass-levels");
        save(&path, &Config::default())?;
        overwrite(&path, r#"{"glassTransparency":250,"glassBlur":250}"#)?;
        let clamped = required(&path)?;
        assert_eq!(clamped.glass_transparency, GLASS_LEVEL_MAX);
        assert_eq!(clamped.glass_blur, GLASS_LEVEL_MAX);

        save(&path, &Config::default())?;
        let saved = update_preferences(
            &path,
            serde_json::Map::from_iter([
                ("glassBlur".into(), 40.into()),
                ("version".into(), 1.into()),
            ]),
        )?;
        assert_eq!(saved.glass_blur, 40);
        assert_eq!(saved.glass_transparency, GLASS_TRANSPARENCY_DEFAULT);
        assert_eq!(saved.appearance, Appearance::Glass);
        assert_eq!(saved.version, CURRENT_VERSION);
        assert_eq!(saved.icon_size, Config::default().icon_size);
        Ok(())
    }

    #[test]
    fn genie_speed_defaults_to_fast_and_preserves_saved_choices() {
        let missing: Config = serde_json::from_str(r#"{}"#).unwrap_or_default();
        assert_eq!(missing.genie_speed, GenieSpeed::Fast);

        let unknown: Config = serde_json::from_str(r#"{"genieSpeed":"warp"}"#).unwrap_or_default();
        assert_eq!(unknown.genie_speed, GenieSpeed::Fast);

        let normal: Config = serde_json::from_str(r#"{"genieSpeed":"normal"}"#).unwrap_or_default();
        assert_eq!(normal.genie_speed, GenieSpeed::Normal);

        let fast: Config = serde_json::from_str(r#"{"genieSpeed":"fast"}"#).unwrap_or_default();
        assert_eq!(fast.genie_speed, GenieSpeed::Fast);

        let faster: Config = serde_json::from_str(r#"{"genieSpeed":"faster"}"#).unwrap_or_default();
        assert_eq!(faster.genie_speed, GenieSpeed::Faster);
        assert!(GenieSpeed::Faster.seconds() < GenieSpeed::Fast.seconds());
        assert!(GenieSpeed::Fast.seconds() < GenieSpeed::Normal.seconds());
    }

    #[test]
    fn load_should_read_files_written_before_camel_case_keys() {
        let path = scratch("legacy-keys");
        save(&path, &Config::default()).ok();
        fs::write(&path, r#"{"version":1,"pinned":["a"],"auto_hide":true}"#).ok();
        let loaded = load(&path).ok().flatten();
        assert_eq!(loaded.map(|config| config.auto_hide), Some(true));
    }

    #[test]
    fn migrate_roaming_copies_pins_and_rewrites_their_paths() {
        let root = env::temp_dir().join(format!("muelle-migrate-{}-copy", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let legacy = root.join(LEGACY_DATA_DIR);
        let pins = legacy.join("Pins");
        fs::create_dir_all(&pins).ok();
        let pin = pins.join("Notas.lnk");
        fs::write(&pin, b"pin").ok();
        let escaped = pin.to_string_lossy().replace('\\', "\\\\");
        fs::write(
            legacy.join("config.json"),
            format!(r#"{{"pinned":["{escaped}"]}}"#),
        )
        .ok();
        migrate_roaming(&root);
        let current = root.join(DATA_DIR);
        assert!(current.join("Pins").join("Notas.lnk").is_file());
        assert!(pin.is_file());
        let text = fs::read_to_string(current.join("config.json")).unwrap_or_default();
        assert!(text.contains(DATA_DIR), "{text}");
        assert!(
            !text.contains("Muelle\\Pins") && !text.contains("Muelle/Pins"),
            "{text}"
        );
        fs::write(current.join("config.json"), "{}").ok();
        migrate_roaming(&root);
        assert_eq!(
            fs::read_to_string(current.join("config.json")).unwrap_or_default(),
            "{}"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn rewrite_config_paths_moves_windows_pin_paths() {
        let root = env::temp_dir().join(format!("muelle-migrate-{}-rewrite", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).ok();
        let file = root.join("config.json");
        fs::write(
            &file,
            r#"{"pinned":["C:\\Users\\a\\AppData\\Roaming\\Muelle\\Pins\\Notas.lnk","C:\\Users\\a\\AppData\\Roaming\\muelle\\Pins\\Calc.lnk"]}"#,
        )
        .ok();
        rewrite_config_paths(
            &file,
            Path::new(r"C:\Users\a\AppData\Roaming\Muelle"),
            Path::new(r"C:\Users\a\AppData\Roaming\Nikao Dock"),
        )
        .ok();
        let text = fs::read_to_string(&file).unwrap_or_default();
        assert!(text.contains(r"Nikao Dock\\Pins\\Notas.lnk"), "{text}");
        assert!(text.contains(r"Nikao Dock\\Pins\\Calc.lnk"), "{text}");
        assert!(
            !text.to_ascii_lowercase().contains(r"roaming\\muelle\\"),
            "{text}"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn migrate_roaming_moves_windock_before_copying() {
        let root = env::temp_dir().join(format!("muelle-migrate-{}-windock", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let older = root.join(OLDER_DATA_DIR);
        fs::create_dir_all(&older).ok();
        fs::write(older.join("config.json"), r#"{"iconSize":48}"#).ok();
        migrate_roaming(&root);
        assert!(!older.exists());
        assert!(root.join(LEGACY_DATA_DIR).join("config.json").is_file());
        let text = fs::read_to_string(root.join(DATA_DIR).join("config.json")).unwrap_or_default();
        assert!(text.contains("48"), "{text}");
        let _ = fs::remove_dir_all(&root);
    }
}
