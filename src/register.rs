//! `vitreus --register` / `--unregister`: makes Vitreus a photo viewer on
//! Windows ("Open with", Default Apps, `vitreus` in Run). Every way of
//! installing (install.ps1, Scoop, winget, a plain zip) shares this code, and
//! the list of types comes straight from `catalog::EXTENSIONS`.

#[cfg(windows)]
mod imp {
    use std::path::Path;

    use anyhow::{Context as _, Result};
    use windows_registry::{CURRENT_USER, Key, LOCAL_MACHINE, Type};

    use crate::catalog::EXTENSIONS;

    const APP: &str = "Vitreus";
    const PROG_ID: &str = "Vitreus.Image";
    const EXE: &str = "vitreus.exe";

    fn hive(machine: bool) -> &'static Key {
        if machine { LOCAL_MACHINE } else { CURRENT_USER }
    }

    pub fn register(exe: &Path, machine: bool) -> Result<()> {
        let hive = hive(machine);
        let exe = exe.display().to_string();
        let open = format!("\"{exe}\" \"%1\"");
        let app_icon = format!("\"{exe}\",0");
        // Photos get the picture icon (resource id 2), not the app logo.
        let file_icon = format!("\"{exe}\",-2");
        let set = |path: &str, name: &str, value: &str| -> Result<()> {
            hive.create(path)
                .and_then(|key| key.set_string(name, value))
                .with_context(|| format!("writing {path}"))
        };

        let prog = format!("Software\\Classes\\{PROG_ID}");
        set(&prog, "", &format!("{APP} image"))?;
        set(&format!("{prog}\\DefaultIcon"), "", &file_icon)?;
        set(&format!("{prog}\\shell\\open"), "FriendlyAppName", APP)?;
        set(&format!("{prog}\\shell\\open\\command"), "", &open)?;

        let app = format!("Software\\Classes\\Applications\\{EXE}");
        set(&app, "FriendlyAppName", APP)?;
        set(&format!("{app}\\DefaultIcon"), "", &file_icon)?;
        set(&format!("{app}\\shell\\open\\command"), "", &open)?;

        let caps = format!("Software\\{APP}\\Capabilities");
        set(&caps, "ApplicationName", APP)?;
        set(&caps, "ApplicationDescription", "Fast, no-nonsense photo viewer")?;
        set(&caps, "ApplicationIcon", &app_icon)?;
        set("Software\\RegisteredApplications", APP, &caps)?;

        for ext in EXTENSIONS {
            set(&format!("{app}\\SupportedTypes"), &format!(".{ext}"), "")?;
            set(&format!("{caps}\\FileAssociations"), &format!(".{ext}"), PROG_ID)?;
            hive.create(format!("Software\\Classes\\.{ext}\\OpenWithProgids"))
                .and_then(|key| key.set_bytes(PROG_ID, Type::Other(0), &[]))
                .with_context(|| format!("registering .{ext}"))?;
        }

        set(
            &format!("Software\\Microsoft\\Windows\\CurrentVersion\\App Paths\\{EXE}"),
            "",
            &exe,
        )?;
        notify_shell();
        Ok(())
    }

    pub fn unregister(machine: bool) -> Result<()> {
        let hive = hive(machine);
        // Missing keys are fine: unregistering twice must not fail.
        let _ = hive.remove_tree(format!("Software\\Classes\\{PROG_ID}"));
        let _ = hive.remove_tree(format!("Software\\Classes\\Applications\\{EXE}"));
        let _ = hive.remove_tree(format!("Software\\{APP}"));
        let _ = hive.remove_tree(format!(
            "Software\\Microsoft\\Windows\\CurrentVersion\\App Paths\\{EXE}"
        ));
        if let Ok(key) = hive.options().read().write().open("Software\\RegisteredApplications") {
            let _ = key.remove_value(APP);
        }
        for ext in EXTENSIONS {
            if let Ok(key) = hive
                .options()
                .read()
                .write()
                .open(format!("Software\\Classes\\.{ext}\\OpenWithProgids"))
            {
                let _ = key.remove_value(PROG_ID);
            }
        }
        notify_shell();
        Ok(())
    }

    /// Tells Explorer to pick up the new icons and "Open with" entries.
    fn notify_shell() {
        use windows::Win32::UI::Shell::{SHCNE_ASSOCCHANGED, SHCNF_IDLIST, SHChangeNotify};
        unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None) };
    }
}

/// Handles `--register [--machine] [--path <exe>]` and `--unregister [--machine]`.
/// Returns the process exit code.
pub fn run(args: &[String]) -> i32 {
    let machine = args.iter().any(|a| a == "--machine");
    let unregister = args.iter().any(|a| a == "--unregister");
    #[cfg(windows)]
    {
        let exe = match args.iter().position(|a| a == "--path") {
            Some(i) => match args.get(i + 1) {
                Some(path) => std::path::PathBuf::from(path),
                None => {
                    eprintln!("--path needs the vitreus.exe to register");
                    return 2;
                }
            },
            None => match std::env::current_exe() {
                Ok(exe) => exe,
                Err(e) => {
                    eprintln!("can't find vitreus.exe: {e}");
                    return 1;
                }
            },
        };
        let result = if unregister {
            imp::unregister(machine)
        } else {
            imp::register(&exe, machine)
        };
        match result {
            Ok(()) => {
                let what = if unregister { "Unregistered" } else { "Registered" };
                println!("{what} Vitreus for {} image types.", crate::catalog::EXTENSIONS.len());
                0
            }
            Err(e) => {
                eprintln!("{e:#}");
                1
            }
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (machine, unregister);
        eprintln!("--register is for Windows; on macOS and Linux install.sh registers Vitreus.");
        2
    }
}
