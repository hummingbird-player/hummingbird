use std::{cell::RefCell, collections::HashMap, rc::Rc, sync::Arc};

use alsa::pcm::{Access, HwParams};
use intx::I24;
use tracing::debug;

use crate::devices::{
    builtin::exclusive::AcquireError,
    errors::{InfoError, OpenError},
    format::{
        BufferSize, ChannelSpec, FormatInfo, SampleFormat, SupportedFormat, choose_sample_format,
    },
    traits::{Device, OutputStream},
};

use super::{
    PROVIDER_NAME, eld,
    format::{ALL_ENCODINGS, AlsaSample, Encoding, StreamFormat, UsbRates, parse_usb_stream},
    pipewire_info,
    reserve::{ReleaseSignal, Reservation},
    session::{PcmTarget, open_pcm},
    stream,
};

pub(super) const PROBE_RATES: [u32; 10] = [
    44_100, 48_000, 88_200, 96_000, 176_400, 192_000, 352_800, 384_000, 32_000, 22_050,
];

/// A format the hardware accepts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ProbedFormat {
    pub(super) format: StreamFormat,
    /// Significant bits, if fewer than the sample format holds (24-in-32).
    pub(super) valid_bits: Option<u32>,
}

/// Formats the hardware accepts. Keyed by device UID.
pub(super) type FormatCache = Rc<RefCell<HashMap<String, Vec<ProbedFormat>>>>;

pub(super) struct AlsaDevice {
    pub(super) card: i32,
    card_id: String,
    pcm_device: u32,
    name: String,
    format_cache: FormatCache,
}

impl AlsaDevice {
    pub(super) fn new(
        card: i32,
        card_id: String,
        pcm_device: u32,
        name: String,
        format_cache: FormatCache,
    ) -> Self {
        Self {
            card,
            card_id,
            pcm_device,
            name,
            format_cache,
        }
    }

    fn pcm_name(&self) -> String {
        format!("hw:CARD={},DEV={}", self.card_id, self.pcm_device)
    }

    pub(super) fn card_id(&self) -> &str {
        &self.card_id
    }

    pub(super) fn target(&self) -> PcmTarget {
        PcmTarget {
            card: self.card,
            card_id: self.card_id.clone(),
            pcm_name: self.pcm_name(),
        }
    }

    /// What the hardware accepts, narrowed to what the connected display accepts on HDMI and
    /// DisplayPort outputs.
    pub(super) fn formats(&self) -> Result<Vec<ProbedFormat>, InfoError> {
        let formats = self.hardware_formats()?;
        // read every time, since the display can change
        let Some(eld) = eld::read(self.card, self.pcm_device) else {
            return Ok(formats);
        };
        let restricted = eld.restrict(&formats);
        if restricted.is_empty() {
            debug!(
                "the display on {} accepts none of its formats",
                self.pcm_name()
            );
            return Ok(formats);
        }
        Ok(restricted)
    }

    fn hardware_formats(&self) -> Result<Vec<ProbedFormat>, InfoError> {
        let key = self.pcm_name();
        if let Some(formats) = self.format_cache.borrow().get(&key) {
            return Ok(formats.clone());
        }
        let formats = self.probe_formats()?;
        debug!(
            "ALSA formats for {key}: {:?}",
            formats
                .iter()
                .map(|p| (
                    p.format.rate,
                    p.format.encoding,
                    p.format.channels,
                    p.valid_bits
                ))
                .collect::<Vec<_>>()
        );
        self.format_cache.borrow_mut().insert(key, formats.clone());
        Ok(formats)
    }

    fn probe_formats(&self) -> Result<Vec<ProbedFormat>, InfoError> {
        // USB formats are listed in procfs, so the card doesn't need to be opened
        let stream_file = format!("/proc/asound/card{}/stream{}", self.card, self.pcm_device);
        if let Ok(contents) = std::fs::read_to_string(stream_file) {
            let formats = usb_formats(&contents);
            if !formats.is_empty() {
                return Ok(formats);
            }
        }

        match open_pcm(&self.pcm_name(), true) {
            Ok(pcm) => probe_pcm(&pcm),
            Err(AcquireError::Busy) => {
                if let Some(formats) = self.pipewire_formats() {
                    return Ok(formats);
                }
                // no PipeWire sink to ask, so take the card briefly to probe it
                debug!("taking {} to probe its formats", self.pcm_name());
                let reservation =
                    Reservation::acquire(self.card, Arc::new(ReleaseSignal::default()))
                        .map_err(|e| InfoError::Unknown(format!("{e:?}")))?;
                let pcm = open_pcm(&self.pcm_name(), true)
                    .map_err(|e| InfoError::Unknown(format!("{e:?}")))?;
                let formats = probe_pcm(&pcm);
                drop(pcm);
                drop(reservation);
                formats
            }
            Err(error) => Err(InfoError::Unknown(format!("{error:?}"))),
        }
    }

    /// The formats PipeWire's sink for this PCM reports. PipeWire doesn't report significant
    /// bits, so `valid_bits` is unknown.
    fn pipewire_formats(&self) -> Option<Vec<ProbedFormat>> {
        let mut formats: Vec<ProbedFormat> = Vec::new();
        for sink in pipewire_info::sink_formats(self.card, self.pcm_device)? {
            let channels = if sink.channels.contains(2) {
                2
            } else {
                sink.channels.min()?
            };
            for encoding in ALL_ENCODINGS
                .into_iter()
                .filter(|e| sink.encodings.contains(e))
            {
                for rate in PROBE_RATES.into_iter().filter(|&r| sink.rates.contains(r)) {
                    let format = StreamFormat {
                        encoding,
                        rate,
                        channels: u16::try_from(channels).ok()?,
                    };
                    if !formats.iter().any(|p| p.format == format) {
                        formats.push(ProbedFormat {
                            format,
                            valid_bits: None,
                        });
                    }
                }
            }
        }
        (!formats.is_empty()).then_some(formats)
    }

    fn open_stream<T: AlsaSample>(
        &self,
        probed: ProbedFormat,
    ) -> Result<Box<dyn OutputStream>, OpenError> {
        Ok(Box::new(stream::open::<T>(
            self.target(),
            probed.format,
            probed.valid_bits,
        )?))
    }
}

fn usb_formats(contents: &str) -> Vec<ProbedFormat> {
    let mut formats = Vec::new();
    for setting in parse_usb_stream(contents) {
        let rates = match &setting.rates {
            UsbRates::Discrete(rates) => rates.clone(),
            UsbRates::Continuous(..) => PROBE_RATES
                .into_iter()
                .filter(|&rate| setting.rates.contains(rate))
                .collect(),
        };
        for rate in rates {
            let format = StreamFormat {
                encoding: setting.encoding,
                rate,
                channels: setting.channels,
            };
            if !formats.iter().any(|p: &ProbedFormat| p.format == format) {
                formats.push(ProbedFormat {
                    format,
                    valid_bits: setting.encoding.valid_bits(setting.bits.map(u32::from)),
                });
            }
        }
    }
    formats
}

fn probe_pcm(pcm: &alsa::PCM) -> Result<Vec<ProbedFormat>, InfoError> {
    let mut formats: Vec<StreamFormat> = Vec::new();
    for encoding in ALL_ENCODINGS {
        let Ok(hwp) = HwParams::any(pcm) else {
            continue;
        };
        if hwp.set_access(Access::RWInterleaved).is_err()
            || hwp.set_format(encoding.alsa_format()).is_err()
        {
            continue;
        }
        let channels = if hwp.test_channels(2).is_ok() {
            2
        } else {
            match hwp.get_channels_min() {
                Ok(min) => min,
                Err(_) => continue,
            }
        };
        if hwp.set_channels(channels).is_err() {
            continue;
        }
        for rate in PROBE_RATES {
            if hwp.test_rate(rate).is_ok() {
                formats.push(StreamFormat {
                    encoding,
                    rate,
                    channels: channels as u16,
                });
            }
        }
    }
    if formats.is_empty() {
        return Err(InfoError::None);
    }

    // many 32-bit formats are really 24-bit (most HDA codecs), which only shows once installed
    let mut valid_bits: HashMap<Encoding, Option<u32>> = HashMap::new();
    for format in &formats {
        if format.encoding.sample_format().bits() > 24 && !valid_bits.contains_key(&format.encoding)
        {
            valid_bits.insert(
                format.encoding,
                format.encoding.valid_bits(significant_bits(pcm, *format)),
            );
        }
    }
    Ok(formats
        .into_iter()
        .map(|format| ProbedFormat {
            format,
            valid_bits: valid_bits.get(&format.encoding).copied().flatten(),
        })
        .collect())
}

/// The hardware's significant bits (`msbits`) once `format` is installed.
fn significant_bits(pcm: &alsa::PCM, format: StreamFormat) -> Option<u32> {
    let hwp = HwParams::any(pcm).ok()?;
    hwp.set_access(Access::RWInterleaved).ok()?;
    hwp.set_format(format.encoding.alsa_format()).ok()?;
    hwp.set_channels(u32::from(format.channels)).ok()?;
    hwp.set_rate(format.rate, alsa::ValueOr::Nearest).ok()?;
    pcm.hw_params(&hwp).ok()?;
    let mut output = alsa::Output::buffer_open().ok()?;
    pcm.dump_hw_setup(&mut output).ok()?;
    let bits = output.buffer_string(|setup| parse_msbits(&String::from_utf8_lossy(setup)));
    pcm.drop().ok();
    bits
}

/// Reads `msbits` from `snd_pcm_dump_hw_setup` output, which looks like `  msbits       : 24`.
fn parse_msbits(setup: &str) -> Option<u32> {
    setup.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        (key.trim() == "msbits")
            .then(|| value.trim().parse().ok())
            .flatten()
    })
}

impl Device for AlsaDevice {
    fn open_device(&mut self, format: FormatInfo) -> Result<Box<dyn OutputStream>, OpenError> {
        if format.originating_provider != PROVIDER_NAME {
            return Err(OpenError::InvalidConfigProvider);
        }
        let channels = format.channels.count();
        let supported = self
            .formats()
            .map_err(|e| OpenError::Unknown(format!("{e:?}")))?;
        let stream_format = Encoding::candidates(format.sample_type)
            .iter()
            .map(|&encoding| StreamFormat {
                encoding,
                rate: format.sample_rate,
                channels,
            })
            .find_map(|candidate| supported.iter().copied().find(|p| p.format == candidate))
            .ok_or(OpenError::InvalidSampleFormat)?;

        match stream_format.format.encoding {
            Encoding::U8 => self.open_stream::<u8>(stream_format),
            Encoding::S16 => self.open_stream::<i16>(stream_format),
            Encoding::S24Packed | Encoding::S24In32 => self.open_stream::<I24>(stream_format),
            Encoding::S32 => self.open_stream::<i32>(stream_format),
            Encoding::F32 => self.open_stream::<f32>(stream_format),
        }
    }

    fn get_supported_formats(&self) -> Result<Vec<SupportedFormat>, InfoError> {
        let mut supported: Vec<SupportedFormat> = Vec::new();
        for ProbedFormat { format, .. } in self.formats()? {
            let entry = SupportedFormat {
                originating_provider: PROVIDER_NAME,
                sample_type: format.encoding.sample_format(),
                sample_rates: (format.rate, format.rate),
                buffer_size: BufferSize::Unknown,
                channels: ChannelSpec::Count(format.channels),
            };
            if !supported.contains(&entry) {
                supported.push(entry);
            }
        }
        Ok(supported)
    }

    /// Without a sound server there's no configured format, so this prefers stereo at 48 kHz (or
    /// 44.1 kHz) in the widest sample format.
    fn get_default_format(&self) -> Result<FormatInfo, InfoError> {
        let formats: Vec<StreamFormat> = self.formats()?.into_iter().map(|p| p.format).collect();
        let channels = if formats.iter().any(|f| f.channels == 2) {
            2
        } else {
            formats.first().ok_or(InfoError::None)?.channels
        };
        let at_channels: Vec<_> = formats.iter().filter(|f| f.channels == channels).collect();
        let rate = [48_000, 44_100]
            .into_iter()
            .find(|rate| at_channels.iter().any(|f| f.rate == *rate))
            .unwrap_or(at_channels[0].rate);
        let sample_types: Vec<SampleFormat> = at_channels
            .iter()
            .filter(|f| f.rate == rate)
            .map(|f| f.encoding.sample_format())
            .collect();
        let sample_type = choose_sample_format(None, &sample_types).ok_or(InfoError::None)?;

        Ok(FormatInfo {
            originating_provider: PROVIDER_NAME,
            sample_type,
            sample_rate: rate,
            buffer_size: BufferSize::Unknown,
            channels: ChannelSpec::Count(channels),
        })
    }

    fn get_name(&self) -> Result<String, InfoError> {
        Ok(self.name.clone())
    }

    fn get_uid(&self) -> Result<String, InfoError> {
        Ok(self.pcm_name())
    }

    fn requires_matching_format(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_msbits_from_hw_setup() {
        let setup = "  stream       : PLAYBACK\n  format       : S32_LE\n  msbits       : 24\n  rate         : 48000\n";
        assert_eq!(parse_msbits(setup), Some(24));
        assert_eq!(parse_msbits("  format : S16_LE\n"), None);
    }

    #[test]
    fn expands_usb_formats() {
        let stream = "X\n\nPlayback:\n  Interface 1\n    Altset 1\n    Format: S24_3LE\n    \
                      Channels: 2\n    Rates: 32000 - 96000 (continuous)\n";
        let rates: Vec<u32> = usb_formats(stream).iter().map(|p| p.format.rate).collect();
        assert_eq!(rates, vec![44_100, 48_000, 88_200, 96_000, 32_000]);
    }
}
