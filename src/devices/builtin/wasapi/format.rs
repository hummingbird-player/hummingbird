use std::time::Duration;

use intx::I24;
use windows::{
    Win32::Media::{
        Audio::{
            AUDCLNT_SHAREMODE_EXCLUSIVE, IAudioClient, WAVEFORMATEX, WAVEFORMATEXTENSIBLE,
            WAVEFORMATEXTENSIBLE_0,
        },
        KernelStreaming::{KSDATAFORMAT_SUBTYPE_PCM, WAVE_FORMAT_EXTENSIBLE},
        Multimedia::KSDATAFORMAT_SUBTYPE_IEEE_FLOAT,
    },
    core::HRESULT,
};

use crate::devices::{
    channels::ChannelLayout,
    format::SampleFormat,
    resample::{SampleFrom, SampleInto},
    util::Scale,
};

const WAVE_FORMAT_PCM: u16 = 1;
const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;

/// A concrete exclusive-mode sample encoding. `S24In32` is 24 valid bits left-justified in a
/// 32-bit container, which many drivers expose instead of packed 24-bit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Encoding {
    U8,
    S16,
    S24,
    S24In32,
    S32,
    F32,
}

pub(super) const ALL_ENCODINGS: [Encoding; 6] = [
    Encoding::S32,
    Encoding::S24In32,
    Encoding::S24,
    Encoding::F32,
    Encoding::S16,
    Encoding::U8,
];

impl Encoding {
    fn container_bits(self) -> u16 {
        match self {
            Encoding::U8 => 8,
            Encoding::S16 => 16,
            Encoding::S24 => 24,
            Encoding::S24In32 | Encoding::S32 | Encoding::F32 => 32,
        }
    }

    fn valid_bits(self) -> u16 {
        match self {
            Encoding::S24In32 => 24,
            other => other.container_bits(),
        }
    }

    /// The pipeline sample type that fills this encoding. 24-in-32 is written as a full `i32`;
    /// the hardware ignores the low byte, which is zero for any source of 24 bits or fewer.
    pub(super) fn sample_format(self) -> SampleFormat {
        match self {
            Encoding::U8 => SampleFormat::Unsigned8,
            Encoding::S16 => SampleFormat::Signed16,
            Encoding::S24 => SampleFormat::Signed24,
            Encoding::S24In32 | Encoding::S32 => SampleFormat::Signed32,
            Encoding::F32 => SampleFormat::Float32,
        }
    }

    /// Ordered best first.
    pub(super) fn candidates(format: SampleFormat) -> &'static [Encoding] {
        match format {
            SampleFormat::Unsigned8 => &[Encoding::U8],
            SampleFormat::Signed16 => &[Encoding::S16],
            SampleFormat::Signed24 => &[Encoding::S24],
            SampleFormat::Signed32 => &[Encoding::S32, Encoding::S24In32],
            SampleFormat::Float32 => &[Encoding::F32],
            _ => &[],
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct StreamFormat {
    pub(super) encoding: Encoding,
    pub(super) rate: u32,
    pub(super) channels: u16,
    pub(super) channel_mask: u32,
}

impl StreamFormat {
    fn block_align(&self) -> usize {
        usize::from(self.channels) * usize::from(self.encoding.container_bits() / 8)
    }

    pub(super) fn to_wave_format(self) -> WAVEFORMATEXTENSIBLE {
        let block_align = self.block_align() as u16;
        WAVEFORMATEXTENSIBLE {
            Format: WAVEFORMATEX {
                wFormatTag: WAVE_FORMAT_EXTENSIBLE as u16,
                nChannels: self.channels,
                nSamplesPerSec: self.rate,
                nAvgBytesPerSec: self.rate * u32::from(block_align),
                nBlockAlign: block_align,
                wBitsPerSample: self.encoding.container_bits(),
                cbSize: (size_of::<WAVEFORMATEXTENSIBLE>() - size_of::<WAVEFORMATEX>()) as u16,
            },
            Samples: WAVEFORMATEXTENSIBLE_0 {
                wValidBitsPerSample: self.encoding.valid_bits(),
            },
            dwChannelMask: self.channel_mask,
            SubFormat: if self.encoding == Encoding::F32 {
                KSDATAFORMAT_SUBTYPE_IEEE_FLOAT
            } else {
                KSDATAFORMAT_SUBTYPE_PCM
            },
        }
    }
}

pub(super) fn default_channel_mask(channels: u16) -> u32 {
    // the low 18 bits of `ChannelPosition` match the WAVE channel mask
    match ChannelLayout::from_count_canonical(channels) {
        ChannelLayout::Positioned(positions) => positions.bits() as u32,
        _ => 0,
    }
}

/// # Safety
/// `ptr` must point to a valid `WAVEFORMATEX` followed by `cbSize` bytes of extension data.
pub(super) unsafe fn parse_wave_format(ptr: *const WAVEFORMATEX) -> Option<StreamFormat> {
    let format = unsafe { ptr.read_unaligned() };
    let tag = format.wFormatTag;
    let bits = format.wBitsPerSample;
    let (float, valid_bits, channel_mask) = if u32::from(tag) == WAVE_FORMAT_EXTENSIBLE
        && usize::from(format.cbSize)
            >= size_of::<WAVEFORMATEXTENSIBLE>() - size_of::<WAVEFORMATEX>()
    {
        let ext = unsafe { (ptr as *const WAVEFORMATEXTENSIBLE).read_unaligned() };
        let valid = unsafe { ext.Samples.wValidBitsPerSample };
        let sub_format = ext.SubFormat;
        let float = if sub_format == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT {
            true
        } else if sub_format == KSDATAFORMAT_SUBTYPE_PCM {
            false
        } else {
            return None;
        };
        (
            float,
            if valid == 0 { bits } else { valid },
            ext.dwChannelMask,
        )
    } else if tag == WAVE_FORMAT_PCM {
        (false, bits, 0)
    } else if tag == WAVE_FORMAT_IEEE_FLOAT {
        (true, bits, 0)
    } else {
        return None;
    };

    let encoding = match (float, bits, valid_bits) {
        (true, 32, _) => Encoding::F32,
        (false, 8, _) => Encoding::U8,
        (false, 16, _) => Encoding::S16,
        (false, 24, _) => Encoding::S24,
        (false, 32, 24) => Encoding::S24In32,
        (false, 32, _) => Encoding::S32,
        _ => return None,
    };

    let channels = format.nChannels;
    Some(StreamFormat {
        encoding,
        rate: format.nSamplesPerSec,
        channels,
        channel_mask: if channel_mask == 0 {
            default_channel_mask(channels)
        } else {
            channel_mask
        },
    })
}

pub(super) fn is_format_supported(client: &IAudioClient, format: StreamFormat) -> bool {
    let wave_format = format.to_wave_format();
    let hr = unsafe {
        client.IsFormatSupported(
            AUDCLNT_SHAREMODE_EXCLUSIVE,
            &wave_format as *const _ as *const WAVEFORMATEX,
            None,
        )
    };
    hr == HRESULT(0)
}

pub(super) fn frames_for_duration(sample_rate: u32, duration: Duration) -> u32 {
    ((u64::from(sample_rate) * duration.as_micros() as u64) / 1_000_000).max(1) as u32
}

pub(super) trait WasapiSample:
    Copy + Scale + SampleFrom<f64> + SampleInto<f64> + Send + 'static
{
    const BYTES: usize;

    fn write_le(self, out: &mut [u8]);

    fn silence() -> Self {
        Self::sample_from(0.0)
    }
}

macro_rules! wasapi_sample {
    ($t:ty, $bytes:expr) => {
        impl WasapiSample for $t {
            const BYTES: usize = $bytes;

            fn write_le(self, out: &mut [u8]) {
                out.copy_from_slice(&self.to_le_bytes());
            }
        }
    };
}

wasapi_sample!(u8, 1);
wasapi_sample!(i16, 2);
wasapi_sample!(I24, 3);
wasapi_sample!(i32, 4);
wasapi_sample!(f32, 4);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wave_format_round_trips() {
        for encoding in ALL_ENCODINGS {
            let format = StreamFormat {
                encoding,
                rate: 96_000,
                channels: 2,
                channel_mask: 0x3,
            };
            let wave_format = format.to_wave_format();
            let parsed =
                unsafe { parse_wave_format(&wave_format as *const _ as *const WAVEFORMATEX) }
                    .unwrap();
            assert_eq!(parsed.encoding, encoding);
            assert_eq!(parsed.rate, 96_000);
            assert_eq!(parsed.channels, 2);
            assert_eq!(parsed.channel_mask, 0x3);
        }
    }

    fn converts_exactly<T: WasapiSample>(value: f64) -> bool {
        T::sample_from(value).sample_into() == value
    }

    #[test]
    fn exact_conversion_detects_lost_bits() {
        let s16 = f64::from(-12_345i16) / 32_768.0;
        let s24 = f64::from(1_234_567i32) / 8_388_608.0;
        assert!(converts_exactly::<i16>(s16));
        assert!(converts_exactly::<I24>(s16));
        assert!(converts_exactly::<i32>(s24));
        assert!(converts_exactly::<I24>(s24));
        assert!(!converts_exactly::<i16>(s24));
        assert!(converts_exactly::<f32>(s24));
        // values produced by resampling or gain
        assert!(!converts_exactly::<i32>(0.1));
        assert!(!converts_exactly::<i16>(1.0));
    }

    #[test]
    fn samples_serialize_little_endian() {
        let mut out = [0u8; 3];
        I24::sample_from(-1.0 / 8_388_608.0).write_le(&mut out);
        assert_eq!(out, [0xFF, 0xFF, 0xFF]);
        let mut out = [0u8; 1];
        u8::silence().write_le(&mut out);
        assert_eq!(out, [0x80]);
    }
}
