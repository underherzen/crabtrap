//! Иконки приложения: штатные иконки компонентов gpui-kit + несколько
//! Lucide-иконок, которых нет в штатном наборе (встраиваются только они).

use std::borrow::Cow;

use gpui_kit::assets::{self, icon_assets};
use gpui_kit::{AssetSource, Result, SharedString};

pub use gpui_kit::assets::IconName as AppIcon;

icon_assets!(pub ExtraIcons, [
    ArrowDownUp,
    Ban,
    Circle,
    CirclePause,
    CircleDot,
    CircleStop,
    FileCode,
    FolderOpen,
    Funnel,
    Layers,
    Lock,
    LockOpen,
    Pin,
    PinOff,
    Replace,
    Server,
    ShieldCheck,
    SlidersHorizontal,
    Trash,
    Wrench,
]);

/// Источник ассетов окна: дополнительные иконки, затем штатные.
#[derive(Clone, Copy, Debug, Default)]
pub struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        match ExtraIcons.load(path)? {
            Some(bytes) => Ok(Some(bytes)),
            None => assets::Assets.load(path),
        }
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut names = ExtraIcons.list(path)?;
        names.extend(assets::Assets.list(path)?);
        names.sort();
        names.dedup();
        Ok(names)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extra_and_default_icons_both_load() {
        let extra = AppAssets.load("icons/trash.svg").unwrap();
        assert!(extra.is_some_and(|svg| svg.starts_with(b"<svg")));
        let default = AppAssets.load("icons/search.svg").unwrap();
        assert!(default.is_some());
    }

    #[test]
    fn listing_merges_without_duplicates() {
        let names = AppAssets.list("icons/").unwrap();
        assert!(names.iter().any(|n| n.as_ref() == "icons/wrench.svg"));
        assert!(names.iter().any(|n| n.as_ref() == "icons/play.svg"));
        let mut deduped = names.clone();
        deduped.dedup();
        assert_eq!(names.len(), deduped.len());
    }
}
