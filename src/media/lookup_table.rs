use std::{
    fs::File,
    path::Path,
    sync::{Arc, LazyLock},
};

// use tokio rwlock because it is write-preferring
use tokio::sync::RwLock;
use tracing::info;

use crate::media::traits::{MediaProvider, MediaProviderFeatures, MediaStream};

type LookupTableInnerType = Arc<RwLock<Vec<Box<dyn MediaProvider>>>>;

pub static LOOKUP_TABLE: LazyLock<LookupTableInnerType> =
    LazyLock::new(|| Arc::new(RwLock::new(Vec::new())));

pub fn add_provider(provider: Box<dyn MediaProvider>) {
    info!(
        "Attempting to register media provider \"{}\"",
        provider.name()
    );

    let mut write = LOOKUP_TABLE.blocking_write();
    write.push(provider);
}

#[allow(clippy::borrowed_box)]
fn provider_can_read(
    path: &Path,
    required_features: MediaProviderFeatures,
    provider: &Box<dyn MediaProvider>,
) -> anyhow::Result<bool> {
    // mime-types are more reliable but windows is too slow to use them
    // so now we only use extensions
    if let Some(ext) = path.extension().and_then(|v| v.to_str())
        && provider
            .supported_extensions()
            .iter()
            .any(|v| v.eq_ignore_ascii_case(ext))
    {
        return Ok(provider.supported_features() & required_features == required_features);
    }

    Ok(false)
}

pub fn can_be_read(path: &Path, required_features: MediaProviderFeatures) -> anyhow::Result<bool> {
    let read = LOOKUP_TABLE.blocking_read();
    for provider in read.iter() {
        if provider_can_read(path, required_features, provider)? {
            return Ok(true);
        }
    }

    Ok(false)
}

pub fn try_open_media(
    path: &Path,
    required_features: MediaProviderFeatures,
) -> anyhow::Result<Option<Box<dyn MediaStream>>> {
    let read = LOOKUP_TABLE.blocking_read();
    let mut last_error = None;

    for provider in read.iter() {
        if provider_can_read(path, required_features, provider)? {
            let file = File::open(path)?;
            match provider.open(file, path.extension()) {
                Ok(stream) => return Ok(Some(stream)),
                Err(e) => last_error = Some(e),
            }
        }
    }

    if let Some(e) = last_error {
        Err(e.into())
    } else {
        Ok(None)
    }
}
