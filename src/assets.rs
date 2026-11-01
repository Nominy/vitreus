//! UI icons and the logo compiled into the binary, so the viewer is a single file.

use std::borrow::Cow;

use anyhow::Result;
use gpui::{AssetSource, SharedString};

macro_rules! icons {
    ($($name:literal),* $(,)?) => {
        &[$((concat!("icons/", $name, ".svg"), include_bytes!(concat!("../assets/icons/", $name, ".svg")))),*]
    };
}

const ICONS: &[(&str, &[u8])] = icons![
    "actual",
    "close",
    "fit",
    "folder",
    "fullscreen",
    "info",
    "maximize",
    "minimize",
    "next",
    "prev",
    "rotate-left",
    "rotate-right",
    "settings",
    "trash",
    "zoom-in",
    "zoom-out",
];

const BRAND: &[(&str, &[u8])] = &[(
    "brand/mark.png",
    include_bytes!("../assets/app/vitreus-mark.png"),
)];

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(ICONS
            .iter()
            .chain(BRAND)
            .find(|(name, _)| *name == path)
            .map(|(_, bytes)| Cow::Borrowed(*bytes)))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(ICONS
            .iter()
            .chain(BRAND)
            .filter(|(name, _)| name.starts_with(path))
            .map(|(name, _)| SharedString::from(*name))
            .collect())
    }
}
