use gpui::SharedString;
use rust_embed::RustEmbed;
use url::Url;

#[derive(RustEmbed)]
#[folder = "./assets"]
#[include = "fonts/*"]
#[include = "icons/*"]
#[include = "images/*"]
#[exclude = "*.DS_Store"]
#[exclude = "icons/LICENSE"]
#[exclude = "icons/LICENSE-hummingbird-icons"]
pub struct BundledAssets;

impl BundledAssets {
    pub fn load(url: Url) -> gpui::Result<Option<std::borrow::Cow<'static, [u8]>>> {
        let path = url.path().trim_start_matches('/');
        Ok(Self::get(path).map(|f| Some(f.data)).unwrap_or(None))
    }

    pub fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
        Ok(Self::iter()
            .map(|p| format!("!bundled:{p}"))
            .filter_map(|p| {
                if p.starts_with(path) {
                    Some(p.into())
                } else {
                    None
                }
            })
            .collect())
    }
}
