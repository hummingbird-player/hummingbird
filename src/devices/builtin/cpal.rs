use crate::{
    devices::{
        errors::{
            CloseError, FindError, InfoError, InitializationError, ListError, OpenError,
            ResetError, StateError, SubmissionError,
        },
        format::{BufferSize, ChannelSpec, FormatInfo, SampleFormat, SupportedFormat},
        resample::SampleFrom,
        traits::{Device, DeviceProvider, OutputStream},
        util::{AtomicF64, Scale},
    },
    media::{pipeline::ChannelConsumers, playback::Mute},
    util::make_unknown_error,
};
use cpal::{
    Host, SizedSample,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use rb::{Producer, RB, RbConsumer, RbProducer, SpscRb};
use std::sync::Arc;

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
            .find(|dev| id == dev.name().as_deref().unwrap_or("NULL"))
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
        Ok(cpal::StreamConfig {
            channels: format.channels.count(),
            sample_rate: cpal::SampleRate(format.sample_rate),
            buffer_size: cpal::BufferSize::Default,
        })
    }
}

trait CpalSample: SizedSample + Default + Send + Sized + 'static + Mute + Scale {}

impl<T> CpalSample for T where T: SizedSample + Default + Send + Sized + 'static + Mute + Scale {}

fn create_stream_internal<T: CpalSample>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    buffer_size: usize,
    volume: Arc<AtomicF64>,
) -> Result<(cpal::Stream, Producer<T>), OpenError> {
    let rb: SpscRb<T> = SpscRb::new(buffer_size);
    let cons = rb.consumer();
    let prod = rb.producer();

    let stream = device.build_output_stream(
        config,
        move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
            let written = cons.read(data).unwrap_or(0);

            let volume = volume.load(std::sync::atomic::Ordering::Relaxed);

            // don't scale if the volume is close to 1, it could lead to (negligable) quality loss
            if volume <= 0.98 {
                for sample in &mut data[..written] {
                    *sample = sample.scale(volume);
                }
            }

            data[written..].iter_mut().for_each(|v| *v = T::muted())
        },
        move |_| {},
        None,
    )?;

    Ok((stream, prod))
}

impl CpalDevice {
    fn create_stream<T>(&mut self, format: FormatInfo) -> Result<Box<dyn OutputStream>, OpenError>
    where
        T: CpalSample + SampleFrom<f32>,
    {
        let config =
            cpal_config_from_info(&format).map_err(|_| OpenError::InvalidConfigProvider)?;

        let channels = match format.channels {
            ChannelSpec::Count(v) => v,
            _ => panic!("non cpal device"),
        };

        let buffer_size = ((200 * config.sample_rate.0 as usize) / 1000) * channels as usize;

        let volume = Arc::new(AtomicF64::new(1.0));

        let (stream, prod) =
            create_stream_internal::<T>(&self.device, &config, buffer_size, volume.clone())?;

        Ok(Box::new(CpalStream {
            ring_buf: prod,
            stream,
            format,
            config,
            buffer_size,
            device: self.device.clone(),
            volume,
            interleave_buffer: Vec::with_capacity(buffer_size),
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
                    sample_rates: (c.min_sample_rate().0, c.max_sample_rate().0),
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
            sample_rate: format.sample_rate().0,
            buffer_size: match format.buffer_size() {
                &cpal::SupportedBufferSize::Range { min, max } => BufferSize::Range(min, max),
                cpal::SupportedBufferSize::Unknown => BufferSize::Unknown,
            },
            channels: ChannelSpec::Count(format.channels()),
        })
    }

    fn get_name(&self) -> Result<String, InfoError> {
        self.device.name().map_err(|v| v.into())
    }

    fn get_uid(&self) -> Result<String, InfoError> {
        self.device.name().map_err(|v| v.into())
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
    pub volume: Arc<AtomicF64>,
    pub interleave_buffer: Vec<T>,
}

impl<T> OutputStream for CpalStream<T>
where
    T: CpalSample + SampleFrom<f32>,
{
    fn close_stream(&mut self) -> Result<(), CloseError> {
        Ok(())
    }

    fn needs_input(&self) -> bool {
        true // will always be true as long as the submitting thread is not blocked by submit_frame
    }

    fn get_current_format(&self) -> Result<&FormatInfo, InfoError> {
        Ok(&self.format)
    }

    fn play(&mut self) -> Result<(), StateError> {
        self.stream.play().map_err(|v| v.into())
    }

    fn pause(&mut self) -> Result<(), StateError> {
        self.stream.pause().map_err(|v| v.into())
    }

    fn reset(&mut self) -> Result<(), ResetError> {
        let (stream, prod) = create_stream_internal::<T>(
            &self.device,
            &self.config,
            self.buffer_size,
            self.volume.clone(),
        )?;

        self.stream = stream;
        self.ring_buf = prod;
        self.interleave_buffer.clear();

        Ok(())
    }

    fn set_volume(&mut self, volume: f64) -> Result<(), StateError> {
        self.volume
            .store(volume, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }

    fn consume_from(
        &mut self,
        input: &mut ChannelConsumers<f32>,
    ) -> Result<usize, SubmissionError> {
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

        // Interleave and convert to target format
        // Reuse the persistent interleave buffer
        self.interleave_buffer.clear();
        self.interleave_buffer.reserve(read * channel_count);

        for i in 0..read {
            for ch in 0..channel_count {
                let sample_f32 = staging[ch][i];
                self.interleave_buffer.push(T::sample_from(sample_f32));
            }
        }

        // Write to device ring buffer
        let mut slice: &[T] = &self.interleave_buffer;
        while !slice.is_empty() {
            if let Some(written) = self.ring_buf.write_blocking(slice) {
                slice = &slice[written..];
            }
        }

        Ok(read)
    }
}

make_unknown_error!(OpenError, ResetError);
make_unknown_error!(cpal::PlayStreamError, StateError);
make_unknown_error!(cpal::PauseStreamError, StateError);
make_unknown_error!(cpal::DeviceNameError, InfoError);
make_unknown_error!(cpal::DefaultStreamConfigError, InfoError);
make_unknown_error!(cpal::SupportedStreamConfigsError, InfoError);
make_unknown_error!(cpal::BuildStreamError, OpenError);
make_unknown_error!(cpal::DevicesError, ListError);
make_unknown_error!(cpal::DevicesError, FindError);
