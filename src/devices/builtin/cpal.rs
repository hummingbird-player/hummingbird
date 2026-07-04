use crate::{
    devices::{
        errors::{
            CloseError, FindError, InfoError, InitializationError, ListError, OpenError,
            ResetError, StateError, SubmissionError,
        },
        format::{BufferSize, ChannelSpec, FormatInfo, SampleFormat, SupportedFormat},
        resample::SampleFrom,
        traits::{Device, DeviceProvider, OutputStream},
        util::{AtomicF64, GainRamp, Scale, read_available, write_bounded},
    },
    media::{
        pipeline::{ChannelConsumers, DEFAULT_BUFFER_FRAMES},
        playback::Mute,
    },
    util::make_unknown_error,
};
use cpal::{
    Host, SizedSample,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use rtrb::{Producer, RingBuffer};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

/// Delay between requesting a fade-out and pausing the stream. Must exceed
/// the gain ramp length (15 ms) plus a few callback periods to ensure the
/// audio thread drains a buffer at zero gain.
const PAUSE_FADE_WAIT: Duration = Duration::from_millis(50);
/// Target callback buffer size.
const DEVICE_BUFFER_TARGET: Duration = Duration::from_millis(20);
/// Internal ring buffer target. This stays larger than the device buffer so the
/// decoder can absorb scheduling jitter without underrunning the stream.
const RING_BUFFER_TARGET: Duration = Duration::from_millis(100);

pub struct CpalProvider {
    host: Host,
}

impl Default for CpalProvider {
    fn default() -> Self {
        Self {
            host: cpal::default_host(),
        }
    }
}

impl DeviceProvider for CpalProvider {
    fn initialize(&mut self) -> Result<(), InitializationError> {
        self.host = cpal::default_host();

        info!("Using cpal host {}", self.host.id().name());

        Ok(())
    }

    fn get_devices(&mut self) -> Result<Vec<Box<dyn Device>>, ListError> {
        Ok(self
            .host
            .devices()?
            .map(|dev| Box::new(CpalDevice::from(dev)) as Box<dyn Device>)
            .collect())
    }

    fn get_default_device(&mut self) -> Result<Box<dyn Device>, FindError> {
        self.host
            .default_output_device()
            .ok_or(FindError::DeviceDoesNotExist)
            .map(|dev| Box::new(CpalDevice::from(dev)) as Box<dyn Device>)
    }

    fn get_device_by_uid(&mut self, id: &str) -> Result<Box<dyn Device>, FindError> {
        self.host
            .devices()?
            .find(|dev| {
                id == dev
                    .description()
                    .map(|v| v.name().to_string())
                    .as_deref()
                    .unwrap_or("NULL")
            })
            .ok_or(FindError::DeviceDoesNotExist)
            .map(|dev| Box::new(CpalDevice::from(dev)) as Box<dyn Device>)
    }
}

struct CpalDevice {
    device: cpal::Device,
}

impl From<cpal::Device> for CpalDevice {
    fn from(value: cpal::Device) -> Self {
        CpalDevice { device: value }
    }
}

impl TryFrom<cpal::SampleFormat> for SampleFormat {
    type Error = InfoError;

    fn try_from(value: cpal::SampleFormat) -> Result<Self, Self::Error> {
        match value {
            cpal::SampleFormat::I8 => Ok(SampleFormat::Signed8),
            cpal::SampleFormat::I16 => Ok(SampleFormat::Signed16),
            cpal::SampleFormat::I32 => Ok(SampleFormat::Signed32),
            cpal::SampleFormat::U8 => Ok(SampleFormat::Unsigned8),
            cpal::SampleFormat::U16 => Ok(SampleFormat::Unsigned16),
            cpal::SampleFormat::U32 => Ok(SampleFormat::Unsigned32),
            cpal::SampleFormat::F32 => Ok(SampleFormat::Float32),
            cpal::SampleFormat::F64 => Ok(SampleFormat::Float64),
            unsupported => Err(InfoError::SampleFmt(unsupported.to_string())),
        }
    }
}

fn cpal_config_from_info(format: &FormatInfo) -> Result<cpal::StreamConfig, ()> {
    if format.originating_provider != "cpal" {
        Err(())
    } else {
        let target_frames = frames_for_duration(format.sample_rate, DEVICE_BUFFER_TARGET);
        let buffer_size = match format.buffer_size {
            BufferSize::Range(min, max) => cpal::BufferSize::Fixed(target_frames.clamp(min, max)),
            BufferSize::Fixed(size) => cpal::BufferSize::Fixed(size),
            BufferSize::Unknown => cpal::BufferSize::Default,
        };

        Ok(cpal::StreamConfig {
            channels: format.channels.count(),
            sample_rate: format.sample_rate,
            buffer_size,
        })
    }
}

fn frames_for_duration(sample_rate: u32, duration: Duration) -> u32 {
    ((u64::from(sample_rate) * duration.as_micros() as u64) / 1_000_000).max(1) as u32
}

trait CpalSample: SizedSample + Default + Send + Sized + 'static + Mute + Scale {}

impl<T> CpalSample for T where T: SizedSample + Default + Send + Sized + 'static + Mute + Scale {}

fn create_stream_internal<T: CpalSample>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    buffer_size: usize,
    target_gain: Arc<AtomicF64>,
    underruns: Arc<AtomicU64>,
) -> Result<(cpal::Stream, Producer<T>, Arc<AtomicBool>), OpenError> {
    let (prod, mut cons) = RingBuffer::<T>::new(buffer_size);
    let channels = config.channels as usize;
    let mut ramp = GainRamp::new(config.sample_rate);

    let device_errored = Arc::new(AtomicBool::new(false));
    let error_flag = device_errored.clone();

    let stream = device.build_output_stream(
        config,
        move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
            data.iter_mut().for_each(|v| *v = T::muted());
            let read = read_available(&mut cons, data);
            if read < data.len() {
                underruns.fetch_add(1, Ordering::Relaxed);
            }

            let target = target_gain.load(Ordering::Relaxed);
            ramp.apply(data, channels, target);
        },
        move |err| {
            warn!("cpal stream error: {err}");
            error_flag.store(true, Ordering::Relaxed);
        },
        None,
    )?;

    Ok((stream, prod, device_errored))
}

impl CpalDevice {
    fn create_stream<T>(&mut self, format: FormatInfo) -> Result<Box<dyn OutputStream>, OpenError>
    where
        T: CpalSample + SampleFrom<f64> + SampleFrom<f32>,
    {
        let config =
            cpal_config_from_info(&format).map_err(|_| OpenError::InvalidConfigProvider)?;
        let channels = format.channels.count();
        let ring_buffer_frames = frames_for_duration(config.sample_rate, RING_BUFFER_TARGET);
        let buffer_size = ring_buffer_frames as usize * channels as usize;
        debug!(
            "CPAL buffer size {buffer_size}, ring buffer {ring_buffer_frames} frames ({buffer_size} samples)",
        );
        info!("Requesting buffer size: {buffer_size}");
        let target_gain = Arc::new(AtomicF64::new(1.0));
        let underruns = Arc::new(AtomicU64::new(0));
        let (stream, prod, device_errored) = create_stream_internal::<T>(
            &self.device,
            config,
            buffer_size,
            target_gain.clone(),
            underruns.clone(),
        )?;

        Ok(Box::new(CpalStream {
            ring_buf: prod,
            stream,
            format,
            config,
            buffer_size,
            device: self.device.clone(),
            target_gain,
            last_user_volume: 1.0,
            replaygain: 1.0,
            // worst case: a full pipeline staging buffer, interleaved
            interleave_buffer: Vec::with_capacity(DEFAULT_BUFFER_FRAMES * channels as usize),
            underruns,
            underruns_reported: 0,
            last_underrun_log: Instant::now(),
            device_errored,
        }))
    }
}

impl Device for CpalDevice {
    fn open_device(&mut self, format: FormatInfo) -> Result<Box<dyn OutputStream>, OpenError> {
        if format.originating_provider != "cpal" {
            Err(OpenError::InvalidConfigProvider)
        } else {
            match format.sample_type {
                SampleFormat::Signed8 => self.create_stream::<i8>(format),
                SampleFormat::Signed16 => self.create_stream::<i16>(format),
                SampleFormat::Signed32 => self.create_stream::<i32>(format),
                SampleFormat::Unsigned8 => self.create_stream::<u8>(format),
                SampleFormat::Unsigned16 => self.create_stream::<u16>(format),
                SampleFormat::Unsigned32 => self.create_stream::<u32>(format),
                SampleFormat::Float32 => self.create_stream::<f32>(format),
                SampleFormat::Float64 => self.create_stream::<f64>(format),
                _ => Err(OpenError::InvalidSampleFormat),
            }
        }
    }

    fn get_supported_formats(&self) -> Result<Vec<SupportedFormat>, InfoError> {
        Ok(self
            .device
            .supported_output_configs()?
            .filter_map(|c| {
                let sample_type = c.sample_format().try_into().ok()?;
                Some(SupportedFormat {
                    originating_provider: "cpal",
                    sample_type,
                    sample_rates: (c.min_sample_rate(), c.max_sample_rate()),
                    buffer_size: match c.buffer_size() {
                        &cpal::SupportedBufferSize::Range { min, max } => {
                            BufferSize::Range(min, max)
                        }
                        cpal::SupportedBufferSize::Unknown => BufferSize::Unknown,
                    },
                    channels: ChannelSpec::Count(c.channels()),
                })
            })
            .collect())
    }

    fn get_default_format(&self) -> Result<FormatInfo, InfoError> {
        let format = self.device.default_output_config()?;
        Ok(FormatInfo {
            originating_provider: "cpal",
            sample_type: format.sample_format().try_into()?,
            sample_rate: format.sample_rate(),
            buffer_size: match format.buffer_size() {
                &cpal::SupportedBufferSize::Range { min, max } => BufferSize::Range(min, max),
                cpal::SupportedBufferSize::Unknown => BufferSize::Unknown,
            },
            channels: ChannelSpec::Count(format.channels()),
        })
    }

    fn get_name(&self) -> Result<String, InfoError> {
        self.device
            .description()
            .map_err(|v| v.into())
            .map(|v| v.name().to_string())
    }

    fn get_uid(&self) -> Result<String, InfoError> {
        self.device
            .description()
            .map_err(|v| v.into())
            .map(|v| v.name().to_string())
    }

    fn requires_matching_format(&self) -> bool {
        false
    }
}

struct CpalStream<T>
where
    T: SizedSample + Default,
{
    pub ring_buf: Producer<T>,
    pub stream: cpal::Stream,
    pub config: cpal::StreamConfig,
    pub device: cpal::Device,
    pub format: FormatInfo,
    pub buffer_size: usize,
    pub target_gain: Arc<AtomicF64>,
    /// most recent volume the user asked for. This is tracked separately
    /// from `target_gain` because pause-fades temporarily overwrite the
    /// shared atomic with 0.0. `play()` restores from this field.
    pub last_user_volume: f64,
    pub replaygain: f64,
    pub interleave_buffer: Vec<T>,
    pub underruns: Arc<AtomicU64>,
    /// keep track of the last log, so we don't log the same underrun multiple times
    underruns_reported: u64,
    last_underrun_log: Instant,
    device_errored: Arc<AtomicBool>,
}

impl<T> CpalStream<T>
where
    T: SizedSample + Default,
{
    fn report_underruns(&mut self) {
        let total = self.underruns.load(Ordering::Relaxed);
        if total > self.underruns_reported
            && self.last_underrun_log.elapsed() >= Duration::from_secs(1)
        {
            warn!(
                "audio callback underran {} time(s) ({} total)",
                total - self.underruns_reported,
                total
            );
            self.underruns_reported = total;
            self.last_underrun_log = Instant::now();
        }
    }
}

impl<T> OutputStream for CpalStream<T>
where
    T: CpalSample + SampleFrom<f64> + SampleFrom<f32>,
{
    fn close_stream(&mut self) -> Result<(), CloseError> {
        Ok(())
    }

    fn needs_input(&self) -> bool {
        true // will always be true as long as the submitting thread is not blocked by submit_frame
    }

    fn play(&mut self) -> Result<(), StateError> {
        self.target_gain
            .store(self.last_user_volume, Ordering::Relaxed);
        self.stream.play().map_err(|v| v.into())
    }

    fn pause(&mut self) -> Result<(), StateError> {
        self.target_gain.store(0.0, Ordering::Relaxed);
        std::thread::sleep(PAUSE_FADE_WAIT); // TODO: make this less hacky?
        self.stream.pause().map_err(|v| v.into())
    }

    fn reset(&mut self) -> Result<(), ResetError> {
        let (stream, prod, device_errored) = create_stream_internal::<T>(
            &self.device,
            self.config,
            self.buffer_size,
            self.target_gain.clone(),
            self.underruns.clone(),
        )?;

        self.stream = stream;
        self.ring_buf = prod;
        self.device_errored = device_errored;
        self.interleave_buffer.clear();

        Ok(())
    }

    fn set_volume(&mut self, volume: f64) -> Result<(), StateError> {
        self.last_user_volume = volume;
        self.target_gain.store(volume, Ordering::Relaxed);
        Ok(())
    }

    fn set_replaygain(&mut self, gain: f64) -> Result<(), StateError> {
        self.replaygain = gain;
        Ok(())
    }

    #[allow(clippy::needless_range_loop)]
    fn consume_from(
        &mut self,
        input: &mut ChannelConsumers<f64>,
    ) -> Result<usize, SubmissionError> {
        if self.device_errored.load(Ordering::Relaxed) {
            return Err(SubmissionError::DeviceError);
        }

        self.report_underruns();

        let available = input.potentially_available();
        if available == 0 {
            return Ok(0);
        }

        let read = input.try_read_to_staging(available);
        if read == 0 {
            return Ok(0);
        }

        let staging = input.staging();

        let channel_count = staging.len();
        let rg = self.replaygain;

        self.interleave_buffer.clear();
        debug_assert!(
            read * channel_count <= self.interleave_buffer.capacity(),
            "interleave buffer under-sized at stream creation"
        );

        for i in 0..read {
            for ch in 0..channel_count {
                let sample_f64 = staging[ch][i] * rg;
                self.interleave_buffer.push(T::sample_from(sample_f64));
            }
        }

        write_bounded(&mut self.ring_buf, &self.interleave_buffer)
            .map_err(|_| SubmissionError::WriteTimeout)?;

        Ok(read)
    }

    #[allow(clippy::needless_range_loop)]
    fn consume_from_f32(
        &mut self,
        input: &mut ChannelConsumers<f32>,
    ) -> Option<Result<usize, SubmissionError>> {
        // Only support f32 passthrough if this stream is f32
        if self.format.sample_type != SampleFormat::Float32 {
            return None;
        }

        if self.device_errored.load(Ordering::Relaxed) {
            return Some(Err(SubmissionError::DeviceError));
        }

        self.report_underruns();

        let available = input.potentially_available();
        if available == 0 {
            return Some(Ok(0));
        }

        let read = input.try_read_to_staging(available);
        if read == 0 {
            return Some(Ok(0));
        }

        let staging = input.staging();
        let channel_count = staging.len();
        let rg = self.replaygain as f32;

        self.interleave_buffer.clear();
        debug_assert!(
            read * channel_count <= self.interleave_buffer.capacity(),
            "interleave buffer under-sized at stream creation"
        );

        for i in 0..read {
            for ch in 0..channel_count {
                self.interleave_buffer
                    .push(<T as SampleFrom<f32>>::sample_from(staging[ch][i] * rg));
            }
        }

        if write_bounded(&mut self.ring_buf, &self.interleave_buffer).is_err() {
            return Some(Err(SubmissionError::WriteTimeout));
        }

        Some(Ok(read))
    }
}

make_unknown_error!(OpenError, ResetError);
make_unknown_error!(cpal::Error, StateError);
make_unknown_error!(cpal::Error, InfoError);
make_unknown_error!(cpal::Error, OpenError);
make_unknown_error!(cpal::Error, ListError);
make_unknown_error!(cpal::Error, FindError);
