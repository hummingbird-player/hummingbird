use crate::devices::channels::ChannelLayout;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SampleFormat {
    Float64,
    Float32,
    Signed32,
    Unsigned32,
    Signed24,
    Unsigned24,
    Signed16,
    Unsigned16,
    Signed8,
    Unsigned8,
}

/// Specification of the channels used by a stream.
///
/// `Count` is used when positional information is unavailable (e.g. for cpal devices
/// that only report a channel count, or for decoders that don't expose a layout). The
/// built-in channel mixer resolves `Count` to a canonical positional layout via
/// [`ChannelLayout::from_count_canonical`].
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ChannelSpec {
    /// A bare channel count with no positional information.
    Count(u16),
    /// An explicit channel layout.
    Layout(ChannelLayout),
}

impl ChannelSpec {
    /// Total number of channels.
    pub fn count(&self) -> u16 {
        match self {
            ChannelSpec::Count(count) => *count,
            ChannelSpec::Layout(layout) => layout.count() as u16,
        }
    }

    /// Resolve the spec to a positional [`ChannelLayout`], falling back to the canonical
    /// mapping for bare counts.
    pub fn to_layout(&self) -> ChannelLayout {
        match self {
            ChannelSpec::Count(count) => ChannelLayout::from_count_canonical(*count),
            ChannelSpec::Layout(layout) => layout.clone(),
        }
    }
}

impl From<u16> for ChannelSpec {
    fn from(count: u16) -> Self {
        ChannelSpec::Count(count)
    }
}

impl From<ChannelLayout> for ChannelSpec {
    fn from(layout: ChannelLayout) -> Self {
        ChannelSpec::Layout(layout)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BufferSize {
    /// Inclusive range of supported buffer sizes.
    Range(u32, u32),
    Fixed(u32),
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatInfo {
    pub originating_provider: &'static str,
    pub sample_type: SampleFormat,
    pub sample_rate: u32,
    pub buffer_size: BufferSize,
    pub channels: ChannelSpec,
}

/// TODO: this will be used in the future
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupportedFormat {
    pub originating_provider: &'static str,
    pub sample_type: SampleFormat,
    /// Lowest and highest supported sample rates.
    pub sample_rates: (u32, u32),
    pub buffer_size: BufferSize,
    pub channels: ChannelSpec,
}

impl SampleFormat {
    pub fn bits(self) -> u32 {
        match self {
            SampleFormat::Float64 => 64,
            SampleFormat::Float32 | SampleFormat::Signed32 | SampleFormat::Unsigned32 => 32,
            SampleFormat::Signed24 | SampleFormat::Unsigned24 => 24,
            SampleFormat::Signed16 | SampleFormat::Unsigned16 => 16,
            SampleFormat::Signed8 | SampleFormat::Unsigned8 => 8,
        }
    }

    pub fn is_float(self) -> bool {
        matches!(self, SampleFormat::Float32 | SampleFormat::Float64)
    }
}

/// Picks the device sample format that best preserves `source`.
///
/// For an integer source, this is the narrowest integer format with at least as many bits, so
/// the samples reach the device unchanged. For a float source, it's 32-bit float. If neither is
/// available, or the source format is unknown, the widest supported format is used.
pub fn choose_sample_format(
    source: Option<SampleFormat>,
    supported: &[SampleFormat],
) -> Option<SampleFormat> {
    let widest = || {
        supported.iter().copied().max_by_key(|format| {
            // widest first; at equal width, integer formats keep more mantissa bits than float
            (format.bits(), !format.is_float())
        })
    };

    let Some(source) = source else {
        return widest();
    };

    if source.is_float() {
        return supported
            .iter()
            .copied()
            .find(|format| *format == SampleFormat::Float32)
            .or_else(widest);
    }

    supported
        .iter()
        .copied()
        .filter(|format| !format.is_float() && format.bits() >= source.bits())
        .min_by_key(|format| format.bits())
        .or_else(widest)
}

#[cfg(test)]
mod tests {
    use super::{SampleFormat::*, choose_sample_format};

    #[test]
    fn integer_sources_pick_smallest_lossless_container() {
        let supported = [Signed32, Signed24, Signed16, Float32];
        assert_eq!(
            choose_sample_format(Some(Signed16), &supported),
            Some(Signed16)
        );
        assert_eq!(
            choose_sample_format(Some(Signed24), &supported),
            Some(Signed24)
        );
        assert_eq!(
            choose_sample_format(Some(Signed32), &supported),
            Some(Signed32)
        );
        assert_eq!(
            choose_sample_format(Some(Signed16), &[Signed32, Float32]),
            Some(Signed32)
        );
        assert_eq!(
            choose_sample_format(Some(Unsigned8), &[Signed16, Signed24]),
            Some(Signed16)
        );
    }

    #[test]
    fn falls_back_to_widest_format() {
        assert_eq!(
            choose_sample_format(Some(Signed32), &[Signed16, Float32]),
            Some(Float32)
        );
        assert_eq!(
            choose_sample_format(None, &[Signed16, Float32, Signed32]),
            Some(Signed32)
        );
        assert_eq!(choose_sample_format(Some(Signed16), &[]), None);
    }

    #[test]
    fn float_sources_prefer_float32() {
        assert_eq!(
            choose_sample_format(Some(Float32), &[Signed32, Float32]),
            Some(Float32)
        );
        assert_eq!(
            choose_sample_format(Some(Float64), &[Signed16, Signed24]),
            Some(Signed24)
        );
    }
}
