use bitflags::bitflags;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelSpec {
    Bitmask(Channels),
    Count(u16),
}

impl ChannelSpec {
    pub fn count(self) -> u16 {
        match self {
            ChannelSpec::Bitmask(channels) => channels.count(),
            ChannelSpec::Count(count) => count,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BufferSize {
    /// Inclusive range of supported buffer sizes.
    Range(u32, u32),
    Fixed(u32),
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FormatInfo {
    pub originating_provider: &'static str,
    pub sample_type: SampleFormat,
    pub sample_rate: u32,
    pub buffer_size: BufferSize,
    pub channels: ChannelSpec,
}

/// TODO: this will be used in the future
#[allow(dead_code)]
pub struct SupportedFormat {
    pub originating_provider: &'static str,
    pub sample_type: SampleFormat,
    /// Lowest and highest supported sample rates.
    pub sample_rates: (u32, u32),
    pub buffer_size: BufferSize,
    pub channels: ChannelSpec,
}

bitflags! {
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct Channels: u32 {
        const FRONT_LEFT            = 0x1;
        const FRONT_RIGHT           = 0x2;
        const FRONT_CENTER          = 0x4;
        const LOW_FREQUENCY         = 0x8;
        const BACK_LEFT             = 0x10;
        const BACK_RIGHT            = 0x20;
        const FRONT_LEFT_OF_CENTER  = 0x40;
        const FRONT_RIGHT_OF_CENTER = 0x80;
        const BACK_CENTER           = 0x100;
        const SIDE_LEFT             = 0x200;
        const SIDE_RIGHT            = 0x400;
        const TOP_CENTER            = 0x800;
        const TOP_FRONT_LEFT        = 0x1000;
        const TOP_FRONT_CENTER      = 0x2000;
        const TOP_FRONT_RIGHT       = 0x4000;
        const TOP_BACK_LEFT         = 0x8000;
        const TOP_BACK_CENTER       = 0x10000;
        const TOP_BACK_RIGHT        = 0x20000;
    }
}

impl Channels {
    pub fn count(self) -> u16 {
        self.bits().count_ones().try_into().expect("infallible")
    }
}
