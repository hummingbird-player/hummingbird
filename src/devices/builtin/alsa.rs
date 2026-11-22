mod device;
mod eld;
mod format;
#[cfg(test)]
mod hardware_tests;
mod pipewire_info;
mod reserve;
mod session;
mod stream;

use std::{
    collections::HashMap,
    sync::{LazyLock, Mutex},
    time::Duration,
};

use alsa::{Direction, card, ctl::Ctl};
use tracing::{debug, info};

use crate::devices::{
    errors::{FindError, InitializationError, ListError},
    traits::{Device, DeviceProvider},
};

use self::device::{AlsaDevice, FormatCache};

pub const PROVIDER_NAME: &str = "alsa";

/// PipeWire device names (`alsa_card.*`) by ALSA card ID. Cached because PipeWire doesn't list
/// cards we hold.
static CARD_NAMES: LazyLock<Mutex<HashMap<String, String>>> = LazyLock::new(Mutex::default);
/// The last default sink PipeWire reported.
static LAST_SINK: Mutex<Option<String>> = Mutex::new(None);
const CARD_WAIT: Duration = Duration::from_millis(250);
const CARD_WAIT_ATTEMPTS: u32 = 8;

#[derive(Default)]
pub struct AlsaProvider {
    /// Probing non-USB cards may take them from the sound server, so results are cached.
    format_cache: FormatCache,
    /// The last card chosen as the default, used when the default sink can't be matched.
    last_card: Option<String>,
}

impl Drop for AlsaProvider {
    fn drop(&mut self) {
        // give the cards back before another provider opens a stream on them
        reserve::release_lingering();
    }
}

impl AlsaProvider {
    fn default_index(&self, devices: &[AlsaDevice]) -> Option<usize> {
        for attempt in 0..=CARD_WAIT_ATTEMPTS {
            let outputs = pipewire_info::query()?;
            let mut names = CARD_NAMES.lock().unwrap();
            for (name, card_id) in outputs.cards {
                names.insert(card_id, name);
            }
            // WirePlumber may not answer while setting up a card, so use the last sink we saw
            let sink = match outputs.sink {
                Some(sink) => {
                    *LAST_SINK.lock().unwrap() = Some(sink.clone());
                    sink
                }
                None => match LAST_SINK.lock().unwrap().clone() {
                    Some(sink) => sink,
                    None if attempt < CARD_WAIT_ATTEMPTS => {
                        std::thread::sleep(CARD_WAIT);
                        continue;
                    }
                    None => return None,
                },
            };
            let found = devices.iter().position(|device| {
                names
                    .get(device.card_id())
                    .is_some_and(|card_name| pipewire_info::sink_matches_card(&sink, card_name))
            });
            drop(names);
            if found.is_some() || !sink.starts_with("alsa_output.") {
                return found;
            }
            // we may still hold the card, or WirePlumber is still setting it up, so wait
            if attempt < CARD_WAIT_ATTEMPTS {
                debug!("card for {sink} not in PipeWire yet; waiting");
                std::thread::sleep(CARD_WAIT);
            }
        }
        None
    }

    fn devices(&self) -> Vec<AlsaDevice> {
        let mut devices = Vec::new();
        for card in card::Iter::new().filter_map(Result::ok) {
            let Ok(ctl) = Ctl::from_card(&card, false) else {
                continue;
            };
            let Ok(info) = ctl.card_info() else {
                continue;
            };
            let card_id = info.get_id().unwrap_or_default().to_string();
            let card_name = info.get_name().unwrap_or_default().to_string();

            let playback: Vec<(u32, String)> = alsa::ctl::DeviceIter::new(&ctl)
                .filter_map(|device| {
                    let pcm = ctl.pcm_info(device as u32, 0, Direction::Playback).ok()?;
                    Some((
                        device as u32,
                        pcm.get_name().unwrap_or_default().to_string(),
                    ))
                })
                .collect();
            let several = playback.len() > 1;
            for (pcm_device, pcm_name) in playback {
                let name = if several {
                    format!("{card_name} ({pcm_name})")
                } else {
                    card_name.clone()
                };
                devices.push(AlsaDevice::new(
                    card.get_index(),
                    card_id.clone(),
                    pcm_device,
                    name,
                    self.format_cache.clone(),
                ));
            }
        }
        devices
    }
}

impl DeviceProvider for AlsaProvider {
    fn initialize(&mut self) -> Result<(), InitializationError> {
        self.format_cache.borrow_mut().clear();
        info!("Using ALSA hw devices with device reservation");
        Ok(())
    }

    fn get_devices(&mut self) -> Result<Vec<Box<dyn Device>>, ListError> {
        Ok(self
            .devices()
            .into_iter()
            .map(|device| Box::new(device) as Box<dyn Device>)
            .collect())
    }

    /// The card for the default PipeWire sink, falling back to the last chosen card, then the
    /// first one.
    fn get_default_device(&mut self) -> Result<Box<dyn Device>, FindError> {
        let mut devices = self.devices();
        if devices.is_empty() {
            return Err(FindError::DeviceDoesNotExist);
        }

        let chosen = self.default_index(&devices).or_else(|| {
            let last = self.last_card.as_deref()?;
            debug!("default sink not matched to a card; staying on card {last}");
            devices.iter().position(|device| device.card_id() == last)
        });
        let index = chosen.unwrap_or_else(|| {
            debug!("default sink not matched to a card; using the first card");
            0
        });
        let device = devices.swap_remove(index);
        self.last_card = Some(device.card_id().to_string());
        Ok(Box::new(device))
    }

    fn get_device_by_uid(&mut self, id: &str) -> Result<Box<dyn Device>, FindError> {
        self.devices()
            .into_iter()
            .find(|device| device.get_uid().is_ok_and(|uid| uid == id))
            .map(|device| Box::new(device) as Box<dyn Device>)
            .ok_or(FindError::DeviceDoesNotExist)
    }
}
