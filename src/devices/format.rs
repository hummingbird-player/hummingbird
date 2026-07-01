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
