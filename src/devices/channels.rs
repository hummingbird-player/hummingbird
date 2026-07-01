use bitflags::bitflags;

bitflags! {
    /// A bitmask representing positional audio channels.
    /// The first 18 bits match Microsoft's `WAVEFORMATEXTENSIBLE` mask.
    #[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
    pub struct ChannelPosition: u64 {
        const FRONT_LEFT          = 1 << 0;
        const FRONT_RIGHT         = 1 << 1;
        const FRONT_CENTER        = 1 << 2;
        const LFE1                = 1 << 3;
        const REAR_LEFT           = 1 << 4;
        const REAR_RIGHT          = 1 << 5;
        const FRONT_LEFT_CENTER   = 1 << 6;
        const FRONT_RIGHT_CENTER  = 1 << 7;
        const REAR_CENTER         = 1 << 8;
        const SIDE_LEFT           = 1 << 9;
        const SIDE_RIGHT          = 1 << 10;
        const TOP_CENTER          = 1 << 11;
        const TOP_FRONT_LEFT      = 1 << 12;
        const TOP_FRONT_CENTER    = 1 << 13;
        const TOP_FRONT_RIGHT     = 1 << 14;
        const TOP_REAR_LEFT       = 1 << 15;
        const TOP_REAR_CENTER     = 1 << 16;
        const TOP_REAR_RIGHT      = 1 << 17;

        // Extended positions beyond the WAVE mask.
        const LFE2                = 1 << 18;
        const TOP_SIDE_LEFT       = 1 << 19;
        const TOP_SIDE_RIGHT      = 1 << 20;
        const BOTTOM_FRONT_CENTER = 1 << 21;
        const BOTTOM_FRONT_LEFT   = 1 << 22;
        const BOTTOM_FRONT_RIGHT  = 1 << 23;
        const FRONT_LEFT_WIDE     = 1 << 24;
        const FRONT_RIGHT_WIDE    = 1 << 25;
    }
}

impl ChannelPosition {
    /// Iterate over each channel position set in this mask, in ascending bit order.
    pub fn positions(self) -> impl Iterator<Item = ChannelPosition> {
        (0..u64::BITS).filter_map(move |bit| {
            let bit_val = 1u64 << bit;
            if self.bits() & bit_val == 0 {
                return None;
            }
            let single = ChannelPosition::from_bits(bit_val)?;
            Some(single)
        })
    }
}

/// A label for a single channel, independent of position.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum ChannelLabel {
    /// A positioned channel. Exactly one bit of the mask must be set.
    Positioned(ChannelPosition),
    /// A discrete, independent channel identified by index.
    Discrete(u16),
}

impl From<ChannelPosition> for ChannelLabel {
    fn from(value: ChannelPosition) -> Self {
        ChannelLabel::Positioned(value)
    }
}

/// A set of channels describing a layout.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub enum ChannelLayout {
    /// Channels are assigned explicit speaker positions.
    Positioned(ChannelPosition),
    /// Channels are 0..count, lacking positional information.
    Discrete(u16),
    /// Channels are identified by an explicit list of labels.
    Custom(Box<[ChannelLabel]>),
    /// No channels.
    #[default]
    None,
}

impl ChannelLayout {
    /// Total number of channels in the layout.
    pub fn count(&self) -> usize {
        match self {
            ChannelLayout::Positioned(pos) => pos.bits().count_ones() as usize,
            ChannelLayout::Discrete(count) => usize::from(*count),
            ChannelLayout::Custom(labels) => labels.len(),
            ChannelLayout::None => 0,
        }
    }

    /// Resolve a bare channel count to a conventional layout when one is obvious (common surround
    /// sound configurations).
    pub fn from_count_canonical(count: u16) -> ChannelLayout {
        let pos = match count {
            0 => ChannelPosition::empty(),
            1 => ChannelPosition::FRONT_CENTER,
            2 => ChannelPosition::FRONT_LEFT.union(ChannelPosition::FRONT_RIGHT),
            3 => ChannelPosition::FRONT_LEFT
                .union(ChannelPosition::FRONT_RIGHT)
                .union(ChannelPosition::FRONT_CENTER),
            4 => ChannelPosition::FRONT_LEFT
                .union(ChannelPosition::FRONT_RIGHT)
                .union(ChannelPosition::REAR_LEFT)
                .union(ChannelPosition::REAR_RIGHT),
            // NB: this could also be 4.1 but 5.0 is more common
            5 => ChannelPosition::FRONT_LEFT
                .union(ChannelPosition::FRONT_RIGHT)
                .union(ChannelPosition::FRONT_CENTER)
                .union(ChannelPosition::REAR_LEFT)
                .union(ChannelPosition::REAR_RIGHT),
            6 => ChannelPosition::FRONT_LEFT
                .union(ChannelPosition::FRONT_RIGHT)
                .union(ChannelPosition::FRONT_CENTER)
                .union(ChannelPosition::LFE1)
                .union(ChannelPosition::REAR_LEFT)
                .union(ChannelPosition::REAR_RIGHT),
            7 => ChannelPosition::FRONT_LEFT
                .union(ChannelPosition::FRONT_RIGHT)
                .union(ChannelPosition::FRONT_CENTER)
                .union(ChannelPosition::LFE1)
                .union(ChannelPosition::REAR_CENTER)
                .union(ChannelPosition::REAR_LEFT)
                .union(ChannelPosition::REAR_RIGHT),
            8 => ChannelPosition::FRONT_LEFT
                .union(ChannelPosition::FRONT_RIGHT)
                .union(ChannelPosition::FRONT_CENTER)
                .union(ChannelPosition::LFE1)
                .union(ChannelPosition::REAR_LEFT)
                .union(ChannelPosition::REAR_RIGHT)
                .union(ChannelPosition::SIDE_LEFT)
                .union(ChannelPosition::SIDE_RIGHT),
            _ => return ChannelLayout::Discrete(count),
        };
        ChannelLayout::Positioned(pos)
    }
}

impl From<ChannelPosition> for ChannelLayout {
    fn from(value: ChannelPosition) -> Self {
        ChannelLayout::Positioned(value)
    }
}
