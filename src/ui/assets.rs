pub mod bundled;

use std::borrow::Cow;

use gpui::AssetSource;
use url::Url;

use crate::ui::assets::bundled::BundledAssets;

pub struct HummingbirdAssetSource;

impl AssetSource for HummingbirdAssetSource {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        let url = Url::parse(&path[1..])?;

        match url.scheme() {
            "bundled" => BundledAssets::load(url),
            _ => panic!("invalid url scheme for resource"),
        }
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<gpui::SharedString>> {
        BundledAssets.list(path)
    }
}
