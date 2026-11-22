use std::{cell::RefCell, collections::HashMap, rc::Rc};

use intx::I24;
use tracing::debug;
use windows::Win32::{
    Media::Audio::{IAudioClient, IMMDevice, PKEY_AudioEngine_DeviceFormat, WAVEFORMATEX},
    System::{
        Com::{CLSCTX_ALL, CoTaskMemFree, STGM_READ},
        Variant::VT_BLOB,
    },
};

use crate::devices::{
    errors::{InfoError, OpenError},
    format::{BufferSize, ChannelSpec, FormatInfo, SupportedFormat},
    traits::{Device, OutputStream},
};

use super::{
    PROVIDER_NAME,
    com::{device_id, friendly_name, read_property},
    format::{
        ALL_ENCODINGS, Encoding, StreamFormat, WasapiSample, default_channel_mask,
        is_format_supported, parse_wave_format,
    },
    stream,
};

pub(super) const PROBE_RATES: [u32; 10] = [
    44_100, 48_000, 88_200, 96_000, 176_400, 192_000, 352_800, 384_000, 32_000, 22_050,
];

/// Keyed by device ID.
pub(super) type FormatCache = Rc<RefCell<HashMap<String, Vec<SupportedFormat>>>>;

pub(super) struct WasapiDevice {
    device: IMMDevice,
    id: Option<String>,
    format_cache: FormatCache,
}

impl WasapiDevice {
    pub(super) fn new(device: IMMDevice, format_cache: FormatCache) -> Self {
        let id = device_id(&device);
        Self {
            device,
            id,
            format_cache,
        }
    }

    fn client(&self) -> Result<IAudioClient, InfoError> {
        unsafe { self.device.Activate::<IAudioClient>(CLSCTX_ALL, None) }
            .map_err(|e| InfoError::Unknown(e.to_string()))
    }

    /// The format configured in the device's advanced sound settings. The driver runs at this
    /// format, so it always works in exclusive mode. Falls back to the shared-mode mix format,
    /// which may need a different sample encoding.
    pub(super) fn device_format(&self) -> Result<StreamFormat, InfoError> {
        if let Ok(store) = unsafe { self.device.OpenPropertyStore(STGM_READ) }
            && let Some(format) =
                read_property(&store, &PKEY_AudioEngine_DeviceFormat, |value| unsafe {
                    let inner = &value.Anonymous.Anonymous;
                    let blob = inner.Anonymous.blob;
                    if inner.vt != VT_BLOB
                        || blob.pBlobData.is_null()
                        || (blob.cbSize as usize) < size_of::<WAVEFORMATEX>()
                    {
                        return None;
                    }
                    parse_wave_format(blob.pBlobData as *const WAVEFORMATEX)
                })
        {
            return Ok(format);
        }

        let client = self.client()?;
        let mix =
            unsafe { client.GetMixFormat() }.map_err(|e| InfoError::Unknown(e.to_string()))?;
        let parsed = unsafe { parse_wave_format(mix) };
        unsafe { CoTaskMemFree(Some(mix as *const _)) };
        let mut format = parsed.ok_or(InfoError::None)?;

        // the mix format is usually 32-bit float, which few drivers accept exclusively
        if !is_format_supported(&client, format)
            && let Some(encoding) = ALL_ENCODINGS
                .into_iter()
                .find(|&encoding| is_format_supported(&client, StreamFormat { encoding, ..format }))
        {
            format.encoding = encoding;
        }
        Ok(format)
    }

    fn channel_mask_for(&self, channels: u16) -> u32 {
        match self.device_format() {
            Ok(format) if format.channels == channels => format.channel_mask,
            _ => default_channel_mask(channels),
        }
    }

    fn probe_formats(&self) -> Result<Vec<SupportedFormat>, InfoError> {
        let client = self.client()?;
        let base = self.device_format()?;
        let mut formats: Vec<SupportedFormat> = Vec::new();

        let mut rates = PROBE_RATES.to_vec();
        if !rates.contains(&base.rate) {
            rates.push(base.rate);
        }

        for rate in rates {
            for encoding in ALL_ENCODINGS {
                let sample_type = encoding.sample_format();
                let already_listed = formats
                    .iter()
                    .any(|f| f.sample_rates.0 == rate && f.sample_type == sample_type);
                if already_listed
                    || !is_format_supported(
                        &client,
                        StreamFormat {
                            encoding,
                            rate,
                            ..base
                        },
                    )
                {
                    continue;
                }
                formats.push(SupportedFormat {
                    originating_provider: PROVIDER_NAME,
                    sample_type,
                    sample_rates: (rate, rate),
                    buffer_size: BufferSize::Unknown,
                    channels: ChannelSpec::Count(base.channels),
                });
            }
        }

        debug!(
            "WASAPI exclusive formats for {:?}: {:?}",
            self.id,
            formats
                .iter()
                .map(|f| (f.sample_rates.0, f.sample_type))
                .collect::<Vec<_>>()
        );
        Ok(formats)
    }

    fn open_stream<T: WasapiSample>(
        &self,
        format: StreamFormat,
    ) -> Result<Box<dyn OutputStream>, OpenError> {
        let device_id = self
            .id
            .clone()
            .ok_or_else(|| OpenError::Unknown("device has no ID".into()))?;
        Ok(Box::new(stream::open::<T>(device_id, format)?))
    }
}

impl Device for WasapiDevice {
    fn open_device(&mut self, format: FormatInfo) -> Result<Box<dyn OutputStream>, OpenError> {
        if format.originating_provider != PROVIDER_NAME {
            return Err(OpenError::InvalidConfigProvider);
        }

        let channels = format.channels.count();
        let channel_mask = self.channel_mask_for(channels);
        let client = self
            .client()
            .map_err(|e| OpenError::Unknown(e.to_string()))?;
        let stream_format = Encoding::candidates(format.sample_type)
            .iter()
            .map(|&encoding| StreamFormat {
                encoding,
                rate: format.sample_rate,
                channels,
                channel_mask,
            })
            .find(|&candidate| is_format_supported(&client, candidate))
            .ok_or(OpenError::InvalidSampleFormat)?;

        match stream_format.encoding {
            Encoding::U8 => self.open_stream::<u8>(stream_format),
            Encoding::S16 => self.open_stream::<i16>(stream_format),
            Encoding::S24 => self.open_stream::<I24>(stream_format),
            Encoding::S24In32 | Encoding::S32 => self.open_stream::<i32>(stream_format),
            Encoding::F32 => self.open_stream::<f32>(stream_format),
        }
    }

    fn get_supported_formats(&self) -> Result<Vec<SupportedFormat>, InfoError> {
        let key = self.id.clone().unwrap_or_default();
        if let Some(formats) = self.format_cache.borrow().get(&key) {
            return Ok(formats.clone());
        }
        let formats = self.probe_formats()?;
        self.format_cache.borrow_mut().insert(key, formats.clone());
        Ok(formats)
    }

    fn get_default_format(&self) -> Result<FormatInfo, InfoError> {
        let format = self.device_format()?;
        Ok(FormatInfo {
            originating_provider: PROVIDER_NAME,
            sample_type: format.encoding.sample_format(),
            sample_rate: format.rate,
            buffer_size: BufferSize::Unknown,
            channels: ChannelSpec::Count(format.channels),
        })
    }

    fn get_name(&self) -> Result<String, InfoError> {
        friendly_name(&self.device).ok_or(InfoError::None)
    }

    fn get_uid(&self) -> Result<String, InfoError> {
        self.id.clone().ok_or(InfoError::None)
    }

    fn requires_matching_format(&self) -> bool {
        true
    }
}
