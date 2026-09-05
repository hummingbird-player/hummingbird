//! Decoder input is separate from library backends and file discovery. Remote
//! implementations may block waiting for buffered bytes, so they are consumed on
//! a dedicated decoder worker, never on the UI, control thread, or audio callback.
use std::{
    fs::File,
    io::{self, Read, Seek},
};

pub trait MediaInput: Read + Seek + Send + Sync {
    /// Describe actual seek support. A partially buffered input must not pretend
    /// that arbitrary offsets are available merely because it has a known length.
    fn is_seekable(&self) -> bool;
    /// Exact byte length only; estimates must remain None.
    fn byte_len(&self) -> Option<u64>;
}
impl MediaInput for File {
    fn is_seekable(&self) -> bool {
        self.metadata().is_ok_and(|metadata| metadata.is_file())
    }
    fn byte_len(&self) -> Option<u64> {
        self.metadata().ok().map(|metadata| metadata.len())
    }
}
impl MediaInput for io::Cursor<Vec<u8>> {
    fn is_seekable(&self) -> bool {
        true
    }
    fn byte_len(&self) -> Option<u64> {
        Some(self.get_ref().len() as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::{
        lookup_table::try_open_input,
        pipeline::{ChannelBuffers, DecodeResult},
        traits::MediaProviderFeatures,
    };
    #[test]
    fn memory_input_uses_the_existing_decoder_and_recovers_exact_samples() {
        crate::test_support::register_test_media_providers();
        let samples = [-32768_i16, -12345, 0, 12345, 32767];
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36_u32 + samples.len() as u32 * 2).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16_u32.to_le_bytes());
        wav.extend_from_slice(&1_u16.to_le_bytes());
        wav.extend_from_slice(&1_u16.to_le_bytes());
        wav.extend_from_slice(&48000_u32.to_le_bytes());
        wav.extend_from_slice(&96000_u32.to_le_bytes());
        wav.extend_from_slice(&2_u16.to_le_bytes());
        wav.extend_from_slice(&16_u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(samples.len() as u32 * 2).to_le_bytes());
        for sample in samples {
            wav.extend_from_slice(&sample.to_le_bytes());
        }
        // No path or extension is needed: the actual bytes identify the format.
        let mut stream = try_open_input(None, MediaProviderFeatures::PROVIDES_DECODER, || {
            Ok(Box::new(io::Cursor::new(wav.clone())))
        })
        .unwrap()
        .unwrap();
        stream.start_playback().unwrap();
        assert_eq!(stream.sample_rate().unwrap(), 48000);
        let (mut output, mut input) = ChannelBuffers::<f64>::new(1, 8192).split();
        assert!(matches!(
            stream.decode_into(&mut output).unwrap(),
            DecodeResult::Decoded {
                frames: 5,
                rate: 48000
            }
        ));
        assert_eq!(input.try_read_to_staging(5), 5);
        let frames = input.staging();
        assert_eq!(frames[0], samples.map(|sample| f64::from(sample) / 32768.0));
    }
}
