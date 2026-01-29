#![allow(dead_code)]

use intx::{I24, U24};

pub trait Mute {
    fn muted() -> Self;
}

macro_rules! mute_impl {
    ($t:ty, $val:expr) => {
        impl Mute for $t {
            fn muted() -> Self {
                $val
            }
        }
    };
}

mute_impl!(f64, 0.0);
mute_impl!(f32, 0.0);
mute_impl!(u32, 2147483647);
mute_impl!(U24, U24::try_from(8388607).unwrap());
mute_impl!(u16, 32767);
mute_impl!(u8, 127);
mute_impl!(i32, 0);
mute_impl!(I24, I24::from(0_u8));
mute_impl!(i16, 0);
mute_impl!(i8, 0);
