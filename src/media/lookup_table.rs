use std::{
    fs::File,
    path::Path,
    sync::{Arc, LazyLock},
};

// use tokio rwlock because it is write-preferring
use tokio::sync::RwLock;
use tracing::info;

use crate::media::traits::{MediaProvider, MediaProviderFeatures, MediaStream};

type LookupTableInnerType = Arc<RwLock<Vec<Arc<dyn MediaProvider>>>>;

pub static LOOKUP_TABLE: LazyLock<LookupTableInnerType> =
    LazyLock::new(|| Arc::new(RwLock::new(Vec::new())));

pub fn add_provider(provider: Box<dyn MediaProvider>) {
    info!(
        "Attempting to register media provider \"{}\"",
        provider.name()
    );

    let mut write = LOOKUP_TABLE.blocking_write();
    write.push(Arc::from(provider));
}

fn provider_can_read(
    path: &Path,
    required_features: MediaProviderFeatures,
    provider: &dyn MediaProvider,
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
        if provider_can_read(path, required_features, provider.as_ref())? {
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
        if provider_can_read(path, required_features, provider.as_ref())? {
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

/// Completed host caches can use file-only codecs too. Each candidate gets an
/// independent file cursor; provider probing never holds the registry lock.
pub fn try_open_file(
    extension: Option<&std::ffi::OsStr>,
    required_features: MediaProviderFeatures,
    mut open: impl FnMut() -> std::io::Result<File>,
) -> anyhow::Result<Option<Box<dyn MediaStream>>> {
    let mut providers: smallvec::SmallVec<[Arc<dyn MediaProvider>; 4]> = LOOKUP_TABLE
        .blocking_read()
        .iter()
        .filter(|provider| provider.supported_features().contains(required_features))
        .cloned()
        .collect();
    if let Some(extension) = extension.and_then(|extension| extension.to_str()) {
        providers.sort_by_key(|provider| {
            !provider
                .supported_extensions()
                .iter()
                .any(|candidate| candidate.eq_ignore_ascii_case(extension))
        });
    }
    let mut error = None;
    for provider in providers {
        match provider.open(open()?, extension) {
            Ok(stream) => return Ok(Some(stream)),
            Err(failure) => {
                if matches!(failure, crate::media::errors::OpenError::Io(_)) || error.is_none() {
                    error = Some(failure);
                }
            }
        }
    }
    match error {
        Some(error) => Err(error.into()),
        None => Ok(None),
    }
}

/// A fresh input is opened for each candidate provider. The provider list is
/// copied under its lock and probing happens after releasing that lock, since a
/// buffered remote input may wait for bytes. This path runs on decoder workers.
pub fn try_open_input(
    extension: Option<&std::ffi::OsStr>,
    required_features: MediaProviderFeatures,
    mut open: impl FnMut() -> std::io::Result<Box<dyn super::input::MediaInput>>,
) -> anyhow::Result<Option<Box<dyn MediaStream>>> {
    let required = required_features | MediaProviderFeatures::ACCEPTS_INPUT;
    let mut providers: smallvec::SmallVec<[Arc<dyn MediaProvider>; 4]> = LOOKUP_TABLE
        .blocking_read()
        .iter()
        .filter(|provider| provider.supported_features().contains(required))
        .cloned()
        .collect();
    if let Some(extension) = extension.and_then(|extension| extension.to_str()) {
        providers.sort_by_key(|provider| {
            !provider
                .supported_extensions()
                .iter()
                .any(|candidate| candidate.eq_ignore_ascii_case(extension))
        });
    }
    let mut error = None;
    for provider in providers {
        match provider.open_input(open()?, extension) {
            Ok(stream) => return Ok(Some(stream)),
            Err(failure) => {
                if matches!(failure, crate::media::errors::OpenError::Io(_)) || error.is_none() {
                    error = Some(failure);
                }
            }
        }
    }
    match error {
        Some(error) => Err(error.into()),
        None => Ok(None),
    }
}
