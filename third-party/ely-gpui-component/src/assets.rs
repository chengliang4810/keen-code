use std::borrow::Cow;

use anyhow::Context as _;
use gpui::{App, AssetSource, Result, SharedString};
use rust_embed::RustEmbed;

/// Icons and fonts. Pass to `Application::with_assets`.
#[derive(RustEmbed)]
#[folder = "assets"]
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(Self::get(path).map(|file| file.data))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(Self::iter()
            .filter(|name| name.starts_with(path))
            .map(SharedString::from)
            .collect())
    }
}

pub(crate) fn load_fonts(cx: &App) -> Result<()> {
    let fonts = Assets::iter()
        .filter(|name| name.ends_with(".ttf"))
        .map(|name| {
            Assets::get(&name)
                .map(|file| file.data)
                .with_context(|| format!("font {name} vanished from the bundle"))
        })
        .collect::<Result<Vec<_>>>()?;
    log::info!("assets: registering {} fonts", fonts.len());
    cx.text_system().add_fonts(fonts)
}
