//! "A new version is out" notice and one-click update.
//!
//! On launch a background thread asks GitHub for the latest release (at most
//! every few hours; results are cached), unless updates are turned off or the
//! user skipped that version. Updating hands off to whatever installed
//! Vitreus (install script, Scoop, winget, Homebrew), then reopens the photo.

use std::{
    path::Path,
    process::{Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::settings::config_dir;

use anyhow::{Context as _, Result};

pub const CURRENT: &str = env!("CARGO_PKG_VERSION");
pub const RELEASES_URL: &str = "https://github.com/Nominy/vitreus/releases/latest";
const LATEST_URL: &str = "https://github.com/Nominy/vitreus/releases/latest/download/latest.json";
const INSTALL_SITE: &str = "https://nominy.github.io/vitreus";
const CHECK_EVERY_SECS: u64 = 6 * 60 * 60;

// ------------------------------------------------------------------- state

/// Persisted in `<config dir>/update`: when we last asked, what we heard, and
/// which version the user said "skip" to. Whether to check at all is the
/// `check_updates` setting.
#[derive(Default, Debug, PartialEq)]
struct State {
    checked: u64,
    latest: Option<String>,
    skipped: Option<String>,
}

impl State {
    fn parse(text: &str) -> Self {
        let mut state = Self::default();
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else { continue };
            let value = value.trim();
            match key.trim() {
                "checked" => state.checked = value.parse().unwrap_or(0),
                "latest" if !value.is_empty() => state.latest = Some(value.to_owned()),
                "skipped" if !value.is_empty() => state.skipped = Some(value.to_owned()),
                _ => {}
            }
        }
        state
    }

    fn render(&self) -> String {
        format!(
            "# Vitreus update check state. Turn checks off in Settings.\nchecked={}\nlatest={}\nskipped={}\n",
            self.checked,
            self.latest.as_deref().unwrap_or(""),
            self.skipped.as_deref().unwrap_or(""),
        )
    }
}

fn load() -> State {
    config_dir()
        .and_then(|dir| std::fs::read_to_string(dir.join("update")).ok())
        .map(|text| State::parse(&text))
        .unwrap_or_default()
}

fn save(state: &State) {
    if let Some(dir) = config_dir() {
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join("update"), state.render());
    }
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

// ------------------------------------------------------------------- check

/// Blocking; call off the UI thread. Returns the version to offer, if any.
pub fn check() -> Option<String> {
    if std::env::var_os("VITREUS_NO_UPDATE_CHECK").is_some_and(|v| !v.is_empty()) {
        return None;
    }
    if !crate::settings::Settings::load().check_updates {
        return None;
    }
    let mut state = load();
    let url = std::env::var("VITREUS_UPDATE_URL").unwrap_or_else(|_| LATEST_URL.to_owned());
    let due = now().saturating_sub(state.checked) >= CHECK_EVERY_SECS || url != LATEST_URL;
    if due && let Some(latest) = fetch(&url).as_deref().and_then(parse_version) {
        state.latest = Some(latest);
        state.checked = now();
        save(&state);
    }
    let latest = state.latest?;
    (is_newer(&latest, CURRENT) && state.skipped.as_deref() != Some(latest.as_str())).then_some(latest)
}

/// "Not this one": stay quiet until a newer version comes out.
pub fn skip(version: &str) {
    let mut state = load();
    state.skipped = Some(version.to_owned());
    save(&state);
}

/// Uses the system curl (built into Windows 10+, macOS and most Linux
/// desktops) so the app carries no TLS stack of its own.
fn fetch(url: &str) -> Option<String> {
    let mut cmd = Command::new(if cfg!(windows) { "curl.exe" } else { "curl" });
    cmd.args(["-fsSL", "--max-time", "10", url]).stdin(Stdio::null());
    hide_console(&mut cmd);
    let out = cmd.output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Reads `"version": "x.y.z"` out of latest.json.
fn parse_version(json: &str) -> Option<String> {
    let rest = &json[json.find("\"version\"")? + "\"version\"".len()..];
    let rest = rest.trim_start().strip_prefix(':')?.trim_start().strip_prefix('"')?;
    let version = rest[..rest.find('"')?].trim().trim_start_matches('v');
    (!version.is_empty()).then(|| version.to_owned())
}

/// Plain `major.minor.patch` comparison; pre-releases are never offered.
fn is_newer(candidate: &str, current: &str) -> bool {
    let parse = |v: &str| -> Option<Vec<u64>> {
        if v.contains('-') {
            return None;
        }
        v.split('.').map(|part| part.parse().ok()).collect()
    };
    match (parse(candidate), parse(current)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

// ------------------------------------------------------------------ update

/// What clicking "Update" does for this copy of Vitreus.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// Run an updater, then reopen.
    Run(Updater),
    /// Can't update unattended (Linux /usr/local needs sudo): copy the command
    /// for a terminal.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Copy(String),
    /// Not installed by anything we know (zip, cargo run): open the release page.
    Open(&'static str),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Updater {
    /// PowerShell commands; `elevated` runs them as administrator.
    #[cfg(windows)]
    PowerShell { commands: String, elevated: bool, relaunch: String },
    /// POSIX shell commands; `$1` is the photo to reopen.
    #[cfg(not(windows))]
    Shell { commands: String, relaunch: String },
}

impl Action {
    pub fn label(&self) -> &'static str {
        match self {
            Action::Run(_) => "Update",
            Action::Copy(_) => "Copy command",
            Action::Open(_) => "Download",
        }
    }
}

fn install_url(file: &str) -> String {
    let base = std::env::var("VITREUS_INSTALL_URL").unwrap_or_else(|_| INSTALL_SITE.to_owned());
    format!("{}/{file}", base.trim_end_matches('/'))
}

pub fn action() -> Action {
    let Ok(exe) = std::env::current_exe() else {
        return Action::Open(RELEASES_URL);
    };
    detect(&exe)
}

#[cfg(windows)]
fn detect(exe: &Path) -> Action {
    let path = exe.display().to_string().to_lowercase();
    let under = |var: &str, rest: &str| {
        std::env::var(var)
            .ok()
            .is_some_and(|base| path.starts_with(&format!("{}\\{rest}", base.to_lowercase())))
    };
    let quoted = ps_quote(&exe.display().to_string());
    if path.contains("\\scoop\\apps\\vitreus\\") {
        Action::Run(Updater::PowerShell {
            commands: "scoop update vitreus".into(),
            elevated: false,
            relaunch: "'vitreus'".into(),
        })
    } else if path.contains("\\microsoft\\winget\\") {
        Action::Run(Updater::PowerShell {
            commands: "winget upgrade --id Nominy.Vitreus --exact --accept-source-agreements --accept-package-agreements\nif ($LASTEXITCODE) { throw \"winget exited with $LASTEXITCODE\" }".into(),
            elevated: false,
            relaunch: "'vitreus'".into(),
        })
    } else if under("LOCALAPPDATA", "programs\\vitreus\\") || under("ProgramFiles", "vitreus\\") {
        let machine = under("ProgramFiles", "vitreus\\");
        let scope = if machine { "Machine" } else { "User" };
        Action::Run(Updater::PowerShell {
            commands: format!(
                "$env:VITREUS_NO_DEFAULT = '1'\n$env:VITREUS_SCOPE = '{scope}'\nirm {} | iex",
                ps_quote(&install_url("install.ps1"))
            ),
            elevated: machine,
            relaunch: quoted,
        })
    } else {
        Action::Open(RELEASES_URL)
    }
}

#[cfg(not(windows))]
fn detect(exe: &Path) -> Action {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from).unwrap_or_default();
    let script = format!(
        "curl -fsSL {} | sh -s -- --no-default",
        sh_quote(&install_url("install"))
    );
    if cfg!(target_os = "macos") {
        let in_bundle = exe.to_string_lossy().contains("/Vitreus.app/Contents/MacOS/");
        if !in_bundle {
            return Action::Open(RELEASES_URL);
        }
        let relaunch = "if [ -n \"$1\" ]; then open -a Vitreus \"$1\"; else open -a Vitreus; fi".to_owned();
        let brew = ["/opt/homebrew", "/usr/local"]
            .into_iter()
            .find(|prefix| Path::new(prefix).join("Caskroom/vitreus").exists());
        let commands = match brew {
            Some(prefix) => format!("{prefix}/bin/brew upgrade --cask vitreus"),
            None => script,
        };
        Action::Run(Updater::Shell { commands, relaunch })
    } else if exe == home.join(".local/bin/vitreus") {
        Action::Run(Updater::Shell {
            commands: script,
            relaunch: format!("{} \"$1\" >/dev/null 2>&1 &", sh_quote(&exe.to_string_lossy())),
        })
    } else if exe == Path::new("/usr/local/bin/vitreus") {
        Action::Copy(format!(
            "curl -fsSL {} | sh -s -- --system",
            install_url("install")
        ))
    } else {
        Action::Open(RELEASES_URL)
    }
}

/// Starts the updater in its own process; the caller should quit right after
/// so the old files can be replaced.
pub fn start(updater: &Updater, photo: Option<&Path>) -> Result<()> {
    match updater {
        #[cfg(windows)]
        Updater::PowerShell { commands, elevated, relaunch } => {
            let open_photo = match photo {
                Some(p) => format!(
                    "Start-Process -FilePath {relaunch} -ArgumentList {}",
                    ps_quote(&format!("\"{}\"", p.display()))
                ),
                None => format!("Start-Process -FilePath {relaunch}"),
            };
            let update = if *elevated {
                let inner = format!(
                    "try {{\n{commands}\nexit 0\n}} catch {{\nWrite-Host $_ -ForegroundColor Red\nRead-Host 'Update failed. Press Enter to close'\nexit 1\n}}"
                );
                format!(
                    "$p = Start-Process powershell -Verb RunAs -Wait -PassThru -ArgumentList '-NoProfile','-ExecutionPolicy','Bypass','-EncodedCommand','{}'\nif ($p.ExitCode) {{ throw 'The update did not finish.' }}",
                    encode_ps(&inner)
                )
            } else {
                commands.clone()
            };
            let script = format!(
                "$ErrorActionPreference = 'Stop'\n\
                 $Host.UI.RawUI.WindowTitle = 'Updating Vitreus'\n\
                 Write-Host 'Updating Vitreus...' -ForegroundColor Cyan\n\
                 Wait-Process -Id {pid} -ErrorAction SilentlyContinue\n\
                 try {{\n{update}\n{open_photo}\n}} catch {{\n\
                 Write-Host $_ -ForegroundColor Red\n\
                 Read-Host 'Update failed. Press Enter to close'\n}}",
                pid = std::process::id(),
            );
            Command::new("powershell.exe")
                .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-EncodedCommand"])
                .arg(encode_ps(&script))
                .spawn()
                .context("couldn't start PowerShell")?;
        }
        #[cfg(not(windows))]
        Updater::Shell { commands, relaunch } => {
            let log = config_dir().map(|d| d.join("update.log"));
            if let Some(dir) = config_dir() {
                let _ = std::fs::create_dir_all(dir);
            }
            let script = format!(
                "{redirect}\nwhile kill -0 {pid} 2>/dev/null; do sleep 0.2; done\n\
                 echo \"== $(date) updating Vitreus\"\n{commands} || {{ echo 'update failed'; exit 1; }}\n{relaunch}\n",
                redirect = log
                    .map(|l| format!("exec >>{} 2>&1", sh_quote(&l.to_string_lossy())))
                    .unwrap_or_default(),
                pid = std::process::id(),
            );
            Command::new("/bin/sh")
                .arg("-c")
                .arg(script)
                .arg("vitreus-update")
                .arg(photo.map(|p| p.as_os_str().to_owned()).unwrap_or_default())
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .context("couldn't start the updater")?;
        }
    }
    Ok(())
}

#[cfg(windows)]
fn ps_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

#[cfg(not(windows))]
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// `powershell -EncodedCommand` takes base64 of UTF-16LE: no quoting pitfalls.
#[cfg(any(windows, test))]
fn encode_ps(script: &str) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (chunk[0] as u32) << 16
            | (*chunk.get(1).unwrap_or(&0) as u32) << 8
            | *chunk.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(TABLE[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn hide_console(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = cmd;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offers_only_newer_stable_versions() {
        assert!(is_newer("0.2.0", "0.1.9"));
        assert!(is_newer("0.10.0", "0.9.0"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.1.0", "0.2.0"));
        assert!(!is_newer("0.3.0-beta.1", "0.2.0"));
        assert!(!is_newer("garbage", "0.2.0"));
    }

    #[test]
    fn reads_version_from_latest_json() {
        let json = r#"{ "tag": "v0.2.0", "version" : "v0.2.0", "url": "x" }"#;
        assert_eq!(parse_version(json).as_deref(), Some("0.2.0"));
        assert_eq!(parse_version("{}"), None);
    }

    #[test]
    fn encodes_powershell_like_dotnet() {
        // [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes('ab'))
        assert_eq!(encode_ps("ab"), "YQBiAA==");
        assert_eq!(encode_ps("abc"), "YQBiAGMA");
    }
}
