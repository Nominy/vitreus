//! User settings, stored as `key=value` lines in `<config dir>/settings`.

use std::path::PathBuf;

/// How photos are resampled to the screen.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Scaling {
    /// Bicubic (Catmull-Rom): soft, no ringing. The classic photo look.
    #[default]
    Smooth,
    /// Lanczos: crisper when big photos are shrunk to the screen.
    Sharp,
    /// Nearest neighbour, every photo pixel a hard-edged block. For pixel
    /// art, screenshots and pixel peeping.
    Pixel,
}

impl Scaling {
    pub const ALL: [Scaling; 3] = [Scaling::Smooth, Scaling::Sharp, Scaling::Pixel];

    pub fn label(self) -> &'static str {
        match self {
            Scaling::Smooth => "Smooth",
            Scaling::Sharp => "Sharp",
            Scaling::Pixel => "Pixel perfect",
        }
    }

    fn key(self) -> &'static str {
        match self {
            Scaling::Smooth => "smooth",
            Scaling::Sharp => "sharp",
            Scaling::Pixel => "pixel",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    pub scaling: Scaling,
    /// "Fit" also enlarges photos smaller than the window.
    pub enlarge_small: bool,
    /// Look for new releases on launch (see `update`).
    pub check_updates: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            scaling: Scaling::default(),
            enlarge_small: true,
            check_updates: true,
        }
    }
}

impl Settings {
    fn parse(text: &str) -> Self {
        let mut settings = Self::default();
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else { continue };
            let value = value.trim();
            match key.trim() {
                "scaling" => {
                    if let Some(s) = Scaling::ALL.into_iter().find(|s| s.key() == value) {
                        settings.scaling = s;
                    }
                }
                "enlarge_small" => settings.enlarge_small = value != "false",
                "check_updates" => settings.check_updates = value != "false",
                _ => {}
            }
        }
        settings
    }

    fn render(&self) -> String {
        format!(
            "# Vitreus settings (also editable in the app: gear button or Ctrl+,)\n\
             # scaling: smooth | sharp | pixel\n\
             scaling={}\nenlarge_small={}\ncheck_updates={}\n",
            self.scaling.key(),
            self.enlarge_small,
            self.check_updates,
        )
    }

    pub fn load() -> Self {
        config_dir()
            .and_then(|dir| std::fs::read_to_string(dir.join("settings")).ok())
            .map(|text| Self::parse(&text))
            .unwrap_or_default()
    }

    pub fn save(&self) {
        if let Some(dir) = config_dir() {
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::write(dir.join("settings"), self.render());
        }
    }
}

/// `%APPDATA%\Vitreus`, `~/Library/Application Support/Vitreus` or `~/.config/vitreus`.
pub fn config_dir() -> Option<PathBuf> {
    let env = |key| std::env::var_os(key).filter(|v| !v.is_empty()).map(PathBuf::from);
    if cfg!(windows) {
        env("APPDATA").map(|d| d.join("Vitreus"))
    } else if cfg!(target_os = "macos") {
        env("HOME").map(|h| h.join("Library/Application Support/Vitreus"))
    } else {
        env("XDG_CONFIG_HOME")
            .or_else(|| env("HOME").map(|h| h.join(".config")))
            .map(|d| d.join("vitreus"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_ignores_garbage() {
        let settings = Settings {
            scaling: Scaling::Pixel,
            enlarge_small: false,
            check_updates: false,
        };
        assert_eq!(Settings::parse(&settings.render()), settings);
        // Unknown keys and values fall back to defaults instead of failing.
        assert_eq!(Settings::parse("scaling=blurry\nfoo=bar\n"), Settings::default());
    }
}
