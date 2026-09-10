use std::{
    ffi::OsStr,
    path::Path,
    sync::{Arc, LazyLock},
};

// use tokio rwlock because it is write-preferring
use tokio::sync::RwLock;
use tracing::info;

use crate::media::traits::{MediaInput, MediaProvider, MediaProviderFeatures, MediaStream};

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
    extension: Option<&OsStr>,
    required_features: MediaProviderFeatures,
    provider: &Box<dyn MediaProvider>,
) -> anyhow::Result<bool> {
    // mime-types are more reliable but windows is too slow to use them
    // so now we only use extensions
    if let Some(ext) = extension {
        let Some(ext) = ext.to_str() else {
            return Ok(false);
        };
        if !provider
            .supported_extensions()
            .iter()
            .any(|v| v.eq_ignore_ascii_case(ext))
        {
            return Ok(false);
        }
    }

    Ok(provider.supported_features() & required_features == required_features)
}

pub fn can_be_read(path: &Path, required_features: MediaProviderFeatures) -> anyhow::Result<bool> {
    let read = LOOKUP_TABLE.blocking_read();
    for provider in read.iter() {
        if provider_can_read(path.extension(), required_features, provider)? {
            return Ok(true);
        }
    }

    Ok(false)
}

pub fn try_open_media(
    path: &Path,
    required_features: MediaProviderFeatures,
) -> anyhow::Result<Option<Box<dyn MediaStream>>> {
    let input = MediaInput::file(path)?;
    try_open_input(input, required_features)
}

pub fn try_open_input(
    input: MediaInput,
    required_features: MediaProviderFeatures,
) -> anyhow::Result<Option<Box<dyn MediaStream>>> {
    let read = LOOKUP_TABLE.blocking_read();
    let Some(provider) = read.iter().find(|provider| {
        provider_can_read(input.extension.as_deref(), required_features, provider).unwrap_or(false)
    }) else {
        return Ok(None);
    };
    provider
        .open(input.source, input.extension.as_deref())
        .map(Some)
        .map_err(Into::into)
}
