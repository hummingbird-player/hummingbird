use intx::{I24, U24};
use std::sync::atomic::AtomicU64;

use super::resample::{SampleFrom, SampleInto};

pub fn interleave<T>(samples: Vec<Vec<T>>) -> Vec<T>
where
    T: Copy + PartialEq,
{
    if samples.is_empty() {
        return vec![];
    }

    let length = samples.len();
    let mut result = vec![];

    for i in 0..(samples.len() * samples[0].len()) {
        result.push(samples[i % length][i / length]);
    }

    result
}

// Code is dead on non-Linux platforms only
#[allow(dead_code)]
pub trait Packed {
    fn pack(&self) -> Vec<u8>;
}

macro_rules! impl_packed {
    ($t:ty) => {
        impl Packed for [$t] {
            fn pack(&self) -> Vec<u8> {
                self.iter()
                    .flat_map(|&x| x.to_ne_bytes().to_vec())
                    .collect()
            }
        }
    };
}

impl_packed!(u16);
impl_packed!(U24);
impl_packed!(u32);
impl_packed!(i16);
impl_packed!(I24);
impl_packed!(i32);
impl_packed!(i8);
impl_packed!(f32);
impl_packed!(f64);

// special cases
impl Packed for [u8] {
    fn pack(&self) -> Vec<u8> {
        self.to_vec()
    }
}

pub trait Scale: Sized {
    fn scale(self, factor: f64) -> Self;
}

impl<T> Scale for T
where
    T: SampleInto<f64> + SampleFrom<f64> + Copy,
{
    fn scale(self, factor: f64) -> T {
        // anything over 1.0 or under -1.0 will be clamped since it's out of bounds
        T::sample_from(self.sample_into().scale(factor))
    }
}

impl Scale for f64 {
    fn scale(self, factor: f64) -> f64 {
        f64::clamp(self * factor, -1.0, 1.0)
    }
}

pub struct AtomicF64 {
    inner: AtomicU64,
}

impl AtomicF64 {
    pub fn new(value: f64) -> Self {
        let as_u64 = value.to_bits();
        Self {
            inner: AtomicU64::new(as_u64),
        }
    }

    pub fn store(&self, value: f64, ordering: std::sync::atomic::Ordering) {
        let as_u64 = value.to_bits();
        self.inner.store(as_u64, ordering)
    }

    pub fn load(&self, ordering: std::sync::atomic::Ordering) -> f64 {
        let as_u64 = self.inner.load(ordering);
        f64::from_bits(as_u64)
    }
}
