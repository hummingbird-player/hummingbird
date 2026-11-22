use alsa::pcm::Format;
use intx::I24;

use crate::devices::{
    format::SampleFormat,
    resample::{SampleFrom, SampleInto},
    util::Scale,
};

/// A sample encoding the `hw:` device accepts. `S24_LE` is 24 bits right-justified in 32, so
/// it's written from 24-bit samples.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Encoding {
    U8,
    S16,
    S24Packed,
    S24In32,
    S32,
    F32,
}

pub(super) const ALL_ENCODINGS: [Encoding; 6] = [
    Encoding::S32,
    Encoding::S24In32,
    Encoding::S24Packed,
    Encoding::F32,
    Encoding::S16,
    Encoding::U8,
];

impl Encoding {
    pub(super) fn alsa_format(self) -> Format {
        match self {
            Encoding::U8 => Format::U8,
            Encoding::S16 => Format::S16LE,
            Encoding::S24Packed => Format::S243LE,
            Encoding::S24In32 => Format::S24LE,
            Encoding::S32 => Format::S32LE,
            Encoding::F32 => Format::FloatLE,
        }
    }

    pub(super) fn bytes(self) -> usize {
        match self {
            Encoding::U8 => 1,
            Encoding::S16 => 2,
            Encoding::S24Packed => 3,
            Encoding::S24In32 | Encoding::S32 | Encoding::F32 => 4,
        }
    }

    /// Significant bits, if the reported depth is less than this encoding holds.
    pub(super) fn valid_bits(self, reported: Option<u32>) -> Option<u32> {
        reported.filter(|&bits| bits > 0 && bits < self.sample_format().bits())
    }

    /// The pipeline sample type that fills this encoding.
    pub(super) fn sample_format(self) -> SampleFormat {
        match self {
            Encoding::U8 => SampleFormat::Unsigned8,
            Encoding::S16 => SampleFormat::Signed16,
            Encoding::S24Packed | Encoding::S24In32 => SampleFormat::Signed24,
            Encoding::S32 => SampleFormat::Signed32,
            Encoding::F32 => SampleFormat::Float32,
        }
    }

    /// Ordered best first.
    pub(super) fn candidates(format: SampleFormat) -> &'static [Encoding] {
        match format {
            SampleFormat::Unsigned8 => &[Encoding::U8],
            SampleFormat::Signed16 => &[Encoding::S16],
            SampleFormat::Signed24 => &[Encoding::S24Packed, Encoding::S24In32],
            SampleFormat::Signed32 => &[Encoding::S32],
            SampleFormat::Float32 => &[Encoding::F32],
            _ => &[],
        }
    }

    /// The name `/proc/asound/cardN/streamM` uses for this encoding.
    fn proc_name(self) -> &'static str {
        match self {
            Encoding::U8 => "U8",
            Encoding::S16 => "S16_LE",
            Encoding::S24Packed => "S24_3LE",
            Encoding::S24In32 => "S24_LE",
            Encoding::S32 => "S32_LE",
            Encoding::F32 => "FLOAT_LE",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct StreamFormat {
    pub(super) encoding: Encoding,
    pub(super) rate: u32,
    pub(super) channels: u16,
}

/// One playback format a USB audio interface advertises in `/proc/asound/cardN/streamM`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct UsbAltSetting {
    pub(super) encoding: Encoding,
    pub(super) channels: u16,
    pub(super) rates: UsbRates,
    /// The reported bit depth. `S32_LE` with 24 bits is 24-bit audio in a 32-bit container.
    pub(super) bits: Option<u16>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum UsbRates {
    Discrete(Vec<u32>),
    Continuous(u32, u32),
}

impl UsbRates {
    pub(super) fn contains(&self, rate: u32) -> bool {
        match self {
            UsbRates::Discrete(rates) => rates.contains(&rate),
            UsbRates::Continuous(low, high) => (*low..=*high).contains(&rate),
        }
    }
}

/// Parses the playback section of a USB audio `stream` file, which can be read without
/// opening the device.
pub(super) fn parse_usb_stream(contents: &str) -> Vec<UsbAltSetting> {
    let Some(playback) = contents.split("\nPlayback:").nth(1) else {
        return Vec::new();
    };
    let playback = playback.split("\nCapture:").next().unwrap_or(playback);

    let mut settings = Vec::new();
    // each altset block starts with an "Interface" line
    for block in playback.split("\n  Interface").skip(1) {
        let field = |name: &str| {
            block.lines().find_map(|line| {
                line.trim()
                    .strip_prefix(name)
                    .and_then(|rest| rest.strip_prefix(':'))
                    .map(str::trim)
            })
        };
        let Some(encoding) = field("Format")
            .and_then(|name| ALL_ENCODINGS.into_iter().find(|e| e.proc_name() == name))
        else {
            continue;
        };
        let Some(channels) = field("Channels").and_then(|c| c.parse().ok()) else {
            continue;
        };
        let Some(rates) = field("Rates").and_then(parse_rates) else {
            continue;
        };
        settings.push(UsbAltSetting {
            encoding,
            channels,
            rates,
            bits: field("Bits").and_then(|bits| bits.parse().ok()),
        });
    }
    settings
}

fn parse_rates(rates: &str) -> Option<UsbRates> {
    if let Some(range) = rates.strip_suffix("(continuous)") {
        let (low, high) = range.trim().split_once('-')?;
        return Some(UsbRates::Continuous(
            low.trim().parse().ok()?,
            high.trim().parse().ok()?,
        ));
    }
    let rates: Vec<u32> = rates
        .split(',')
        .filter_map(|rate| rate.trim().parse().ok())
        .collect();
    (!rates.is_empty()).then_some(UsbRates::Discrete(rates))
}

pub(super) trait AlsaSample:
    Copy + Scale + SampleFrom<f64> + SampleInto<f64> + Send + 'static
{
    /// Writes the sample little-endian into `out`, which is `Encoding::bytes` long.
    fn write_le(self, out: &mut [u8]);

    fn silence() -> Self {
        Self::sample_from(0.0)
    }
}

macro_rules! alsa_sample {
    ($t:ty) => {
        impl AlsaSample for $t {
            fn write_le(self, out: &mut [u8]) {
                out.copy_from_slice(&self.to_le_bytes());
            }
        }
    };
}

alsa_sample!(u8);
alsa_sample!(i16);
alsa_sample!(i32);
alsa_sample!(f32);

impl AlsaSample for I24 {
    fn write_le(self, out: &mut [u8]) {
        // packed S24_3LE uses the low three bytes, and S24_LE sign-extends them into a fourth
        let bytes = i32::from(self).to_le_bytes();
        out.copy_from_slice(&bytes[..out.len()]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const K7: &str =
        "GuangZhou FiiO Electronics Co.,Ltd FiiO K7 at usb-0000:12:00.3-2.2, high speed : USB Audio

Playback:
  Status: Stop
  Interface 1
    Altset 1
    Format: S32_LE
    Channels: 2
    Endpoint: 0x01 (1 OUT) (ASYNC)
    Rates: 44100, 48000, 88200, 96000, 176400, 192000, 352800, 384000
    Bits: 32
  Interface 1
    Altset 2
    Format: S16_LE
    Channels: 2
    Rates: 44100, 48000
    Bits: 16

Capture:
  Interface 2
    Altset 1
    Format: S24_3LE
    Channels: 1
    Rates: 48000
";

    #[test]
    fn parses_usb_playback_formats() {
        let settings = parse_usb_stream(K7);
        assert_eq!(
            settings,
            vec![
                UsbAltSetting {
                    encoding: Encoding::S32,
                    channels: 2,
                    rates: UsbRates::Discrete(vec![
                        44_100, 48_000, 88_200, 96_000, 176_400, 192_000, 352_800, 384_000
                    ]),
                    bits: Some(32),
                },
                UsbAltSetting {
                    encoding: Encoding::S16,
                    channels: 2,
                    rates: UsbRates::Discrete(vec![44_100, 48_000]),
                    bits: Some(16),
                },
            ]
        );
    }

    #[test]
    fn parses_continuous_rates_and_ignores_capture_only() {
        let stream = "X\n\nPlayback:\n  Interface 1\n    Altset 1\n    Format: S24_3LE\n    \
                      Channels: 2\n    Rates: 8000 - 96000 (continuous)\n";
        let settings = parse_usb_stream(stream);
        assert_eq!(settings.len(), 1);
        assert_eq!(settings[0].encoding, Encoding::S24Packed);
        assert!(settings[0].rates.contains(44_100));
        assert!(!settings[0].rates.contains(192_000));

        assert!(parse_usb_stream("X\n\nCapture:\n  Interface 1\n").is_empty());
    }

    #[test]
    fn reports_24_bits_in_a_32_bit_container() {
        let stream = "X\n\nPlayback:\n  Interface 1\n    Altset 1\n    Format: S32_LE\n    \
                      Channels: 2\n    Rates: 48000\n    Bits: 24\n";
        let settings = parse_usb_stream(stream);
        assert_eq!(settings[0].bits, Some(24));
        assert_eq!(Encoding::S32.valid_bits(Some(24)), Some(24));
        assert_eq!(Encoding::S32.valid_bits(Some(32)), None);
        // S24_LE is already written from 24-bit samples
        assert_eq!(Encoding::S24In32.valid_bits(Some(24)), None);
    }

    #[test]
    fn s24_serializes_packed_and_in_32() {
        let sample = I24::sample_from(-1.0 / 8_388_608.0);
        let mut packed = [0u8; 3];
        sample.write_le(&mut packed);
        assert_eq!(packed, [0xFF, 0xFF, 0xFF]);
        let mut wide = [0u8; 4];
        sample.write_le(&mut wide);
        assert_eq!(wide, [0xFF, 0xFF, 0xFF, 0xFF]);

        let mut wide = [0u8; 4];
        I24::sample_from(0.5).write_le(&mut wide);
        assert_eq!(i32::from_le_bytes(wide), 0x40_0000);
    }
}
