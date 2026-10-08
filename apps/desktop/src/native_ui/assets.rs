//! 应用自有的 GPUI 资源覆盖层。
//!
//! Ely 的图标集合保持为回退来源；这里仅覆盖产品需要但 Ely 当前 revision 尚未提供的
//! Lucide 图标，避免修改第三方 crate 或引入第二套图标运行时。

use std::borrow::Cow;

use ely_gpui_component::Assets;
use gpui::{AssetSource, Result, SharedString};

const MESSAGE_CIRCLE_PLUS: &str = "native/icons/message-circle-plus.svg";
const CALENDAR_CLOCK: &str = "native/icons/calendar-clock.svg";
const BLOCKS: &str = "native/icons/blocks.svg";
const SETTINGS_2: &str = "native/icons/settings-2.svg";
const BRAIN: &str = "native/icons/brain.svg";
const CABLE: &str = "native/icons/cable.svg";
const ANCHOR: &str = "native/icons/anchor.svg";
const LIST_FILTER: &str = "native/icons/list-filter.svg";
const CHEVRON_DOWN: &str = "native/icons/chevron-down.svg";
const BRAND_ICON: &str = "native/brand/icon.png";
const SQUARE_TERMINAL: &str = "native/icons/square-terminal.svg";
const PANEL_RIGHT_OPEN: &str = "native/icons/panel-right-open.svg";
const WINDOW_MAXIMIZE: &str = "native/icons/window-maximize.svg";
const WINDOW_RESTORE: &str = "native/icons/window-restore.svg";
const EMPTY_WATERMARK: &str = "native/brand/empty-watermark.svg";

const APP_ASSET_PATHS: &[&str] = &[
    MESSAGE_CIRCLE_PLUS,
    CALENDAR_CLOCK,
    BLOCKS,
    SETTINGS_2,
    BRAIN,
    CABLE,
    ANCHOR,
    LIST_FILTER,
    CHEVRON_DOWN,
    BRAND_ICON,
    SQUARE_TERMINAL,
    PANEL_RIGHT_OPEN,
    WINDOW_MAXIMIZE,
    WINDOW_RESTORE,
    EMPTY_WATERMARK,
];

/// 提供应用覆盖图标，并把其余资源交给固定 revision 的 Ely 资源源。
pub struct NativeAssets;

impl AssetSource for NativeAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        match path {
            MESSAGE_CIRCLE_PLUS => Ok(Some(Cow::Borrowed(include_bytes!(
                "../../assets/icons/message-circle-plus.svg"
            )))),
            CALENDAR_CLOCK => Ok(Some(Cow::Borrowed(include_bytes!(
                "../../assets/icons/calendar-clock.svg"
            )))),
            BLOCKS => Ok(Some(Cow::Borrowed(include_bytes!(
                "../../assets/icons/blocks.svg"
            )))),
            SETTINGS_2 => Ok(Some(Cow::Borrowed(include_bytes!(
                "../../assets/icons/settings-2.svg"
            )))),
            BRAIN => Ok(Some(Cow::Borrowed(include_bytes!(
                "../../assets/icons/brain.svg"
            )))),
            CABLE => Ok(Some(Cow::Borrowed(include_bytes!(
                "../../assets/icons/cable.svg"
            )))),
            ANCHOR => Ok(Some(Cow::Borrowed(include_bytes!(
                "../../assets/icons/anchor.svg"
            )))),
            LIST_FILTER => Ok(Some(Cow::Borrowed(include_bytes!(
                "../../assets/icons/list-filter.svg"
            )))),
            CHEVRON_DOWN => Ok(Some(Cow::Borrowed(include_bytes!(
                "../../assets/icons/chevron-down.svg"
            )))),
            BRAND_ICON => Ok(Some(Cow::Borrowed(include_bytes!(
                "../../icons/128x128.png"
            )))),
            SQUARE_TERMINAL => Ok(Some(Cow::Borrowed(include_bytes!(
                "../../assets/icons/square-terminal.svg"
            )))),
            PANEL_RIGHT_OPEN => Ok(Some(Cow::Borrowed(include_bytes!(
                "../../assets/icons/panel-right-open.svg"
            )))),
            WINDOW_MAXIMIZE => Ok(Some(Cow::Borrowed(include_bytes!(
                "../../assets/icons/window-maximize.svg"
            )))),
            WINDOW_RESTORE => Ok(Some(Cow::Borrowed(include_bytes!(
                "../../assets/icons/window-restore.svg"
            )))),
            EMPTY_WATERMARK => Ok(Some(Cow::Borrowed(include_bytes!(
                "../../assets/brand/empty-watermark.svg"
            )))),
            _ => Assets.load(path),
        }
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut assets = Assets.list(path)?;
        assets.extend(
            APP_ASSET_PATHS
                .iter()
                .filter(|asset_path| asset_path.starts_with(path))
                .map(|asset_path| SharedString::from(*asset_path)),
        );
        Ok(assets)
    }
}
