mod com;
mod device;
mod format;
#[cfg(test)]
mod hardware_tests;
mod session;
mod stream;

use tracing::info;
use windows::{
    Win32::Media::Audio::{DEVICE_STATE_ACTIVE, IMMDevice, IMMDeviceEnumerator, eConsole, eRender},
    core::PCWSTR,
};

use crate::devices::{
    errors::{FindError, InitializationError, ListError},
    traits::{Device, DeviceProvider},
};

use self::{
    com::{enumerator, wide},
    device::{FormatCache, WasapiDevice},
};

pub const PROVIDER_NAME: &str = "wasapi";

#[derive(Default)]
pub struct WasapiProvider {
    enumerator: Option<IMMDeviceEnumerator>,
    /// Exclusive-mode format probes are slow, and the controller asks for them every time it
    /// opens a stream. Cleared whenever the provider is initialized.
    format_cache: FormatCache,
}

impl WasapiProvider {
    fn enumerator(&mut self) -> windows::core::Result<&IMMDeviceEnumerator> {
        if self.enumerator.is_none() {
            self.enumerator = Some(enumerator()?);
        }
        Ok(self.enumerator.as_ref().unwrap())
    }

    fn wrap(&self, device: IMMDevice) -> Box<dyn Device> {
        Box::new(WasapiDevice::new(device, self.format_cache.clone()))
    }
}

impl DeviceProvider for WasapiProvider {
    fn initialize(&mut self) -> Result<(), InitializationError> {
        self.format_cache.borrow_mut().clear();
        self.enumerator = None;
        self.enumerator()
            .map_err(|e| InitializationError::Unknown(e.to_string()))?;
        info!("Using WASAPI exclusive mode");
        Ok(())
    }

    fn get_devices(&mut self) -> Result<Vec<Box<dyn Device>>, ListError> {
        let collection = unsafe {
            self.enumerator()
                .and_then(|e| e.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE))
        }
        .map_err(|e| ListError::Unknown(e.to_string()))?;
        let count =
            unsafe { collection.GetCount() }.map_err(|e| ListError::Unknown(e.to_string()))?;

        Ok((0..count)
            .filter_map(|index| unsafe { collection.Item(index) }.ok())
            .map(|device| self.wrap(device))
            .collect())
    }

    fn get_default_device(&mut self) -> Result<Box<dyn Device>, FindError> {
        let device = unsafe {
            self.enumerator()
                .and_then(|e| e.GetDefaultAudioEndpoint(eRender, eConsole))
        }
        .map_err(|_| FindError::DeviceDoesNotExist)?;
        Ok(self.wrap(device))
    }

    fn get_device_by_uid(&mut self, id: &str) -> Result<Box<dyn Device>, FindError> {
        let id = wide(id);
        let device = unsafe {
            self.enumerator()
                .and_then(|e| e.GetDevice(PCWSTR(id.as_ptr())))
        }
        .map_err(|_| FindError::DeviceDoesNotExist)?;
        Ok(self.wrap(device))
    }
}
