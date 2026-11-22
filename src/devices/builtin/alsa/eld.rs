use alsa::ctl::{Ctl, ElemId, ElemIface, ElemType, ElemValue};

use super::{device::ProbedFormat, format::Encoding};

/// CEA-861 audio format code for linear PCM.
const LPCM: u8 = 1;
/// The sample rates a short audio descriptor's rate bits stand for, lowest bit first.
const SAD_RATES: [u32; 7] = [32_000, 44_100, 48_000, 88_200, 96_000, 176_400, 192_000];
/// The bit depths an LPCM short audio descriptor's third byte stands for, lowest bit first.
const SAD_BITS: [u32; 3] = [16, 20, 24];

/// One LPCM short audio descriptor.
#[derive(Debug, PartialEq)]
struct Lpcm {
    max_channels: u16,
    rates: Vec<u32>,
    max_bits: u32,
}

/// The LPCM formats the connected display accepts.
#[derive(Debug, PartialEq)]
pub(super) struct Eld(Vec<Lpcm>);

/// The ELD (EDID-Like Data) of `hw:<card>,<pcm_device>`, which describes what the connected
/// display accepts. `None` if it isn't an HDMI or DisplayPort output, or no display is connected.
pub(super) fn read(card: i32, pcm_device: u32) -> Option<Eld> {
    let ctl = Ctl::new(&format!("hw:{card}"), false).ok()?;
    let mut id = ElemId::new(ElemIface::PCM);
    id.set_device(pcm_device);
    id.set_name(c"ELD");
    let mut value = ElemValue::new(ElemType::Bytes).ok()?;
    value.set_id(&id);
    ctl.elem_read(&mut value).ok()?;
    parse(value.get_bytes()?)
}

fn parse(eld: &[u8]) -> Option<Eld> {
    // version 2 (CEA-861-D) in the top five bits; disconnected outputs read as zeros
    if eld.len() < 20 || eld[0] >> 3 != 2 {
        return None;
    }
    let name_len = usize::from(eld[4] & 0x1f);
    let sad_count = usize::from(eld[5] >> 4);
    let start = 20 + name_len;
    let sads = eld.get(start..start + 3 * sad_count)?;

    let bits_set = |byte: u8, values: &[u32]| -> Vec<u32> {
        values
            .iter()
            .enumerate()
            .filter(|(bit, _)| byte & (1 << bit) != 0)
            .map(|(_, value)| *value)
            .collect()
    };
    let lpcm: Vec<Lpcm> = sads
        .as_chunks::<3>()
        .0
        .iter()
        .filter(|sad| (sad[0] >> 3) & 0xf == LPCM)
        .map(|sad| Lpcm {
            max_channels: u16::from(sad[0] & 0x7) + 1,
            rates: bits_set(sad[1], &SAD_RATES),
            max_bits: bits_set(sad[2], &SAD_BITS).into_iter().max().unwrap_or(16),
        })
        .collect();
    (!lpcm.is_empty()).then_some(Eld(lpcm))
}

impl Eld {
    /// Drops the formats the display doesn't accept, and sets the significant bits of the rest to
    /// what the display uses (24-bit audio in a 32-bit container, for example).
    pub(super) fn restrict(&self, formats: &[ProbedFormat]) -> Vec<ProbedFormat> {
        formats
            .iter()
            .filter_map(|probed| {
                let format = probed.format;
                let max_bits = self
                    .0
                    .iter()
                    .filter(|sad| {
                        sad.max_channels >= format.channels && sad.rates.contains(&format.rate)
                    })
                    .map(|sad| sad.max_bits)
                    .max()?;
                if format.encoding == Encoding::F32 {
                    return Some(*probed);
                }
                let bits = probed
                    .valid_bits
                    .unwrap_or(format.encoding.sample_format().bits())
                    .min(max_bits);
                Some(ProbedFormat {
                    format,
                    valid_bits: format.encoding.valid_bits(Some(bits)),
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devices::builtin::alsa::format::StreamFormat;

    /// An ELD for a display named "LG" with one stereo LPCM descriptor: 32, 44.1 and 48 kHz at
    /// 16, 20 and 24 bits.
    fn stereo_display() -> Vec<u8> {
        let mut eld = vec![0u8; 20];
        eld[0] = 2 << 3;
        eld[4] = 2; // name length
        eld[5] = 1 << 4; // one descriptor
        eld.extend_from_slice(b"LG");
        eld.extend_from_slice(&[(LPCM << 3) | 1, 0b111, 0b111]);
        eld
    }

    fn probed(encoding: Encoding, rate: u32, channels: u16) -> ProbedFormat {
        ProbedFormat {
            format: StreamFormat {
                encoding,
                rate,
                channels,
            },
            valid_bits: None,
        }
    }

    #[test]
    fn parses_lpcm_descriptors() {
        assert_eq!(
            parse(&stereo_display()),
            Some(Eld(vec![Lpcm {
                max_channels: 2,
                rates: vec![32_000, 44_100, 48_000],
                max_bits: 24,
            }]))
        );
        // nothing connected
        assert_eq!(parse(&[0; 64]), None);
    }

    #[test]
    fn restricts_to_what_the_display_accepts() {
        let eld = parse(&stereo_display()).unwrap();
        let restricted = eld.restrict(&[
            probed(Encoding::S32, 48_000, 2),
            probed(Encoding::S16, 48_000, 2),
            probed(Encoding::S32, 192_000, 2),
            probed(Encoding::S32, 48_000, 8),
        ]);
        assert_eq!(
            restricted,
            vec![
                ProbedFormat {
                    valid_bits: Some(24),
                    ..probed(Encoding::S32, 48_000, 2)
                },
                probed(Encoding::S16, 48_000, 2),
            ]
        );
    }
}
