use std::path::PathBuf;

const APP_DIR: &str = "blink";
const CONFIG_FILE: &str = "config.toml";

/// Returns the default config file path, or `None` when the per-user config
/// directory is unknown.
///
/// The path is `%APPDATA%\blink\config.toml` on Windows, and
/// `$XDG_CONFIG_HOME/blink/config.toml` (falling back to `~/.config`)
/// elsewhere.
#[must_use]
pub fn default_config_path() -> Option<PathBuf> {
    config_root().map(|root| root.join(APP_DIR).join(CONFIG_FILE))
}

/// Returns the default clip folder: the user's Videos folder joined with
/// `blink`, falling back to `blink` in the home folder. `None` when neither
/// folder can be determined.
#[must_use]
pub fn default_output_dir() -> Option<PathBuf> {
    videos_root()
        .or_else(home_dir)
        .map(|root| root.join(APP_DIR))
}

#[cfg(windows)]
fn config_root() -> Option<PathBuf> {
    known_folders::get_known_folder_path(known_folders::KnownFolder::RoamingAppData)
}

#[cfg(windows)]
fn videos_root() -> Option<PathBuf> {
    known_folders::get_known_folder_path(known_folders::KnownFolder::Videos)
}

#[cfg(windows)]
fn home_dir() -> Option<PathBuf> {
    known_folders::get_known_folder_path(known_folders::KnownFolder::Profile)
}

#[cfg(not(windows))]
fn config_root() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .or_else(|| home_dir().map(|home| home.join(".config")))
}

#[cfg(not(windows))]
fn videos_root() -> Option<PathBuf> {
    home_dir().map(|home| home.join("Videos"))
}

#[cfg(not(windows))]
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_path_when_known_is_absolute_app_file() {
        if let Some(path) = default_config_path() {
            assert!(path.is_absolute());
            assert!(path.ends_with("blink/config.toml"));
        }
    }

    #[test]
    fn output_dir_when_known_is_absolute_blink_folder() {
        if let Some(dir) = default_output_dir() {
            assert!(dir.is_absolute());
            assert!(dir.ends_with(APP_DIR));
        }
    }
}
