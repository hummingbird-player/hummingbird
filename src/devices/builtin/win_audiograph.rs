use std::{slice::from_raw_parts_mut, sync::Arc};

use rb::{Producer, RB, RbConsumer, RbProducer, SpscRb};
use tracing::error;
use windows::{
    Devices::Enumeration::{DeviceClass, DeviceInformation},
    Foundation::TypedEventHandler,
    Media::{
        Audio::{
            AudioDeviceOutputNode, AudioFrameInputNode, AudioGraph, AudioGraphSettings,
            FrameInputNodeQuantumStartedEventArgs,
        },
        AudioBufferAccessMode, AudioFrame,
        Render::AudioRenderCategory,
    },
    Win32::System::WinRT::IMemoryBufferByteAccess,
    core::Interface,
};

use crate::{
    devices::{
        errors::{
            CloseError, FindError, InfoError, InitializationError, ListError, OpenError,
            ResetError, StateError, SubmissionError,
        },
        format::{BufferSize, ChannelSpec, FormatInfo, SampleFormat, SupportedFormat},
        traits::{Device, DeviceProvider, OutputStream},
        util::{AtomicGainRamp, GainRamp, Packed, Scale, duration_ms_to_pcm_frames},
    },
    media::pipeline::ChannelConsumers,
    util::make_unknown_error,
};

const VOLUME_RAMP_DURATION_MS: u32 = 15;

/// Windows Audio Graph backend
///
/// Audio Graph is the most managed of the Windows backends: you can throw nearly any stream at
/// any device and have it play. Unlike WASAPI, it supports multiple output formats to the same
/// device, and unlike XAudio2 and DirectSound, it supports low-latency mode.
#[derive(Default)]
pub struct AudioGraphProvider {}

impl DeviceProvider for AudioGraphProvider {
    fn initialize(&mut self) -> Result<(), InitializationError> {
        Ok(())
    }

    fn get_devices(&mut self) -> Result<Vec<Box<dyn Device>>, ListError> {
        let devices = DeviceInformation::FindAllAsyncDeviceClass(DeviceClass::AudioRender);

        Ok(devices
            .and_then(|v| v.join())?
            .into_iter()
            .map(|device| Box::new(AudioGraphDevice::from(device)) as Box<dyn Device>)
            .collect())
    }

    fn get_default_device(&mut self) -> Result<Box<dyn Device>, FindError> {
        Ok(Box::new(AudioGraphDevice::new()) as Box<dyn Device>)
    }

    fn get_device_by_uid(&mut self, id: &str) -> Result<Box<dyn Device>, FindError> {
        let devices_result = DeviceInformation::FindAllAsyncDeviceClass(DeviceClass::AudioRender);

        let Ok(devices) = devices_result.and_then(|v| v.join()) else {
            return Err(FindError::Unknown("couldn't get device".to_string()));
        };

        devices
            .into_iter()
            .find(|v| v.Id().unwrap_or_default() == id)
            .ok_or(FindError::DeviceDoesNotExist)
            .map(|device| Box::new(AudioGraphDevice::from(device)) as Box<dyn Device>)
    }
}

pub struct AudioGraphDevice {
    graph: AudioGraph,
    device_out: AudioDeviceOutputNode,
}

impl AudioGraphDevice {
    pub fn new() -> Self {
        let settings = AudioGraphSettings::Create(AudioRenderCategory::Media)
            .expect("Could not create default audio settings!");

        let graph_async =
            AudioGraph::CreateAsync(&settings).expect("Could not create default audio graph!");

        let graph = graph_async
            .join()
            .expect("Waiting for asynchronous operation failed: AudioGraph::CreateAsync");

        if let Err(status) = graph.Status() {
            error!("Error initializing graph! {:?}", status)
        }

        let graph_final = graph.Graph().unwrap();

        let device_out = graph_final
            .CreateDeviceOutputNodeAsync()
            .expect("Could not attach audio device to audio graph")
            .join()
            .expect("couldn't get attached audio device");

        if let Err(status) = device_out.Status() {
            error!("Error initializing output device! {:?}", status)
        }

        AudioGraphDevice {
            graph: graph_final,
            device_out: device_out.DeviceOutputNode().unwrap(),
        }
    }
}

impl From<DeviceInformation> for AudioGraphDevice {
    fn from(value: DeviceInformation) -> Self {
        let settings = AudioGraphSettings::Create(AudioRenderCategory::Media)
            .expect("Could not create default audio settings!");

        settings
            .SetPrimaryRenderDevice(&value)
            .expect("Could not set audio device!");

        let graph_async =
            AudioGraph::CreateAsync(&settings).expect("Could not create default audio graph!");

        let graph = graph_async
            .join()
            .expect("Waiting for asynchronous operation failed: AudioGraph::CreateAsync");

        if let Err(status) = graph.Status() {
            error!("Error initializing graph! {:?}", status)
        }

        let graph_final = graph.Graph().unwrap();

        let device_out = graph_final
            .CreateDeviceOutputNodeAsync()
            .expect("Could not attach audio device to audio graph")
            .join()
            .expect("couldn't get attached audio device");

        if let Err(status) = device_out.Status() {
            error!("Error initializing output device! {:?}", status)
        }

        AudioGraphDevice {
            graph: graph_final,
            device_out: device_out.DeviceOutputNode().unwrap(),
        }
    }
}

impl Device for AudioGraphDevice {
    fn open_device(&mut self, format: FormatInfo) -> Result<Box<dyn OutputStream>, OpenError> {
        self.graph.Start()?;
        self.device_out.Start()?;

        let properties = self.graph.EncodingProperties()?;

        properties
            .SetChannelCount(format.channels.count() as u32)
            .map_err(|_| OpenError::InvalidSampleFormat)?;

        let input_node = self.graph.CreateFrameInputNodeWithFormat(&properties)?;

        input_node.AddOutgoingConnection(&self.device_out)?;

        input_node.Stop()?;

        let buffer_size = match format.buffer_size {
            BufferSize::Fixed(v) => v,
            _ => panic!("invalid buffer_size (wrong provider?)"),
        };

        let rb_size =
            buffer_size as usize * size_of::<f32>() * format.channels.count() as usize * 3;

        let rb: SpscRb<u8> = SpscRb::new(rb_size);
        let cons = rb.consumer();
        let prod = rb.producer();

        let volume = Arc::new(AtomicGainRamp::new(1.0));
        let pcm_channel_count = format.channels.count() as usize;
        let mut volume_ramp = GainRamp::from_shared(&volume);

        let handler =
            TypedEventHandler::<AudioFrameInputNode, FrameInputNodeQuantumStartedEventArgs>::new(
                move |sender, args| {
                    let samples = args.as_ref().unwrap().RequiredSamples()?;

                    if samples == 0 {
                        return windows_result::Result::Ok(());
                    }

                    let required_capacity =
                        (samples as u32) * size_of::<f32>() as u32 * pcm_channel_count as u32;

                    let frame = AudioFrame::Create(required_capacity)?;

                    // this is all in a seperate scope so it all drops before we submit
                    {
                        let lock = frame.LockBuffer(AudioBufferAccessMode::Write)?;
                        let reference = lock.CreateReference()?;

                        // why?
                        let write = reference.cast::<IMemoryBufferByteAccess>()?;

                        let slice;

                        unsafe {
                            // what the fuck?
                            let mut value = std::ptr::null_mut();
                            let mut capacity = 0;
                            write
                                .GetBuffer(&mut value, &mut capacity)
                                .expect("this must work or memory will be corrupted");

                            slice = from_raw_parts_mut(value, capacity as usize);
                        }

                        let read = cons.read(slice).unwrap_or(0);
                        // should be fine? IEEE says that 0.0 is 0x00000000...
                        slice[read..].iter_mut().for_each(|v| *v = 0);

                        volume_ramp.sync_from_shared(&volume);

                        let written_samples = read / size_of::<f32>();
                        let frames_written = written_samples / pcm_channel_count;
                        let has_partial_frame = written_samples % pcm_channel_count != 0;

                        if volume_ramp.is_ramping() || volume_ramp.current_gain() <= 0.98 {
                            unsafe {
                                let samples_f32 = std::slice::from_raw_parts_mut(
                                    slice.as_mut_ptr().cast::<f32>(),
                                    written_samples,
                                );

                                for frame in 0..frames_written {
                                    let gain = volume_ramp.advance();

                                    for sample in &mut samples_f32[(frame * pcm_channel_count)
                                        ..((frame + 1) * pcm_channel_count)]
                                    {
                                        *sample = sample.scale(gain);
                                    }
                                }

                                if has_partial_frame {
                                    let gain = volume_ramp.advance();
                                    for sample in &mut samples_f32
                                        [(frames_written * pcm_channel_count)..written_samples]
                                    {
                                        *sample = sample.scale(gain);
                                    }
                                }
                            }
                        } else {
                            for _ in 0..frames_written {
                                let _ = volume_ramp.advance();
                            }

                            if has_partial_frame {
                                let _ = volume_ramp.advance();
                            }
                        }

                        let lock_result = lock.Close();

                        if let Err(err) = lock_result {
                            error!("Error closing frame lock: {}", err);
                            return Err(err);
                        }
                    }

                    let add_result = sender.as_ref().unwrap().AddFrame(&frame);

                    if let Err(err) = add_result {
                        error!("Error adding frame: {}", err);
                        return Err(err);
                    }

                    windows_result::Result::Ok(())
                },
            );

        input_node.QuantumStarted(&handler)?;
        input_node.SetOutgoingGain(1.0)?;

        let stream = AudioGraphStream {
            node: input_node,
            producer: prod,
            interleaved_buffer: Vec::with_capacity(buffer_size as usize),
            packed_buffer: Vec::with_capacity(buffer_size as usize),
            volume,
            volume_ramp_duration_pcm_frames: duration_ms_to_pcm_frames(
                format.sample_rate,
                VOLUME_RAMP_DURATION_MS,
            ),
            last_replaygain: 1.0,
        };

        Ok(Box::new(stream) as Box<dyn OutputStream>)
    }

    fn get_supported_formats(&self) -> Result<Vec<SupportedFormat>, InfoError> {
        let properties = self.graph.EncodingProperties()?;
        let sample_rate = properties.SampleRate()?;
        let buffer_size = self.graph.SamplesPerQuantum()?;
        let channels = properties.ChannelCount()?;

        Ok(vec![SupportedFormat {
            originating_provider: "win_audiograph",
            sample_type: SampleFormat::Float32,
            sample_rates: (sample_rate, sample_rate),
            buffer_size: BufferSize::Fixed(buffer_size as u32),
            channels: ChannelSpec::Count(channels as u16),
        }])
    }

    fn get_default_format(&self) -> Result<FormatInfo, InfoError> {
        let properties = self.graph.EncodingProperties()?;
        let sample_rate = properties.SampleRate()?;
        let buffer_size = self.graph.SamplesPerQuantum()?;
        let channels = properties.ChannelCount()?;

        Ok(FormatInfo {
            originating_provider: "win_audiograph",
            sample_type: SampleFormat::Float32,
            sample_rate,
            buffer_size: BufferSize::Fixed(buffer_size as u32),
            channels: ChannelSpec::Count(channels as u16),
        })
    }

    fn get_name(&self) -> Result<String, InfoError> {
        let device = self
            .graph
            .PrimaryRenderDevice()
            .map_err(|_| InfoError::DeviceIsDefaultAlways)?;

        device.Name().map_err(|e| e.into()).map(|v| v.to_string())
    }

    fn get_uid(&self) -> Result<String, InfoError> {
        let device = self
            .graph
            .PrimaryRenderDevice()
            .map_err(|_| InfoError::DeviceIsDefaultAlways)?;

        device.Id().map_err(|e| e.into()).map(|v| v.to_string())
    }

    fn requires_matching_format(&self) -> bool {
        true
    }
}

pub struct AudioGraphStream {
    pub node: AudioFrameInputNode,
    pub producer: Producer<u8>,
    interleaved_buffer: Vec<f32>,
    packed_buffer: Vec<u8>,
    volume: Arc<AtomicGainRamp>,
    volume_ramp_duration_pcm_frames: u32,
    last_replaygain: f64,
}

impl OutputStream for AudioGraphStream {
    fn close_stream(&mut self) -> Result<(), CloseError> {
        self.node.Close().map_err(|e| e.into())
    }

    fn needs_input(&self) -> bool {
        true
    }

    fn play(&mut self) -> Result<(), StateError> {
        self.node.Start().map_err(|e| e.into())
    }

    fn pause(&mut self) -> Result<(), StateError> {
        self.node.Stop().map_err(|e| e.into())
    }

    fn reset(&mut self) -> Result<(), ResetError> {
        self.node.Reset().map_err(|e| e.into())
    }

    fn set_volume(&mut self, volume: f64) -> Result<(), StateError> {
        self.volume
            .set_target(volume, self.volume_ramp_duration_pcm_frames);
        Ok(())
    }

    fn set_replaygain(&mut self, gain: f64) -> Result<(), StateError> {
        self.last_replaygain = gain;
        Ok(())
    }

    #[allow(clippy::needless_range_loop)]
    fn consume_from(
        &mut self,
        input: &mut ChannelConsumers<f64>,
    ) -> Result<usize, SubmissionError> {
        let available = input.potentially_available();
        if available == 0 {
            return Ok(0);
        }

        self.node.Start().expect("couldn't start");

        let read = input.try_read_to_staging(available);
        if read == 0 {
            return Ok(0);
        }

        let staging = input.staging();

        let channel_count = staging.len();
        let rg = self.last_replaygain;

        // Interleave and convert f64 to f32, then pack to bytes using persistent buffers
        self.interleaved_buffer.clear();
        self.interleaved_buffer.reserve(read * channel_count);
        for i in 0..read {
            for ch in 0..channel_count {
                self.interleaved_buffer.push((staging[ch][i] * rg) as f32);
            }
        }

        self.packed_buffer.clear();
        self.packed_buffer.extend(self.interleaved_buffer.pack());
        let mut slice: &[u8] = &self.packed_buffer;

        while !slice.is_empty() {
            if let Some(written) = self.producer.write_blocking(slice) {
                slice = &slice[written..];
            }
        }

        Ok(read)
    }

    #[allow(clippy::needless_range_loop)]
    fn consume_from_f32(
        &mut self,
        input: &mut ChannelConsumers<f32>,
    ) -> Option<Result<usize, SubmissionError>> {
        let available = input.potentially_available();
        if available == 0 {
            return Some(Ok(0));
        }

        self.node.Start().expect("couldn't start");

        let read = input.try_read_to_staging(available);
        if read == 0 {
            return Some(Ok(0));
        }

        let staging = input.staging();

        let channel_count = staging.len();
        let rg = self.last_replaygain as f32;

        // Interleave f32 samples, then pack to bytes
        self.interleaved_buffer.clear();
        self.interleaved_buffer.reserve(read * channel_count);
        for i in 0..read {
            for ch in 0..channel_count {
                self.interleaved_buffer.push(staging[ch][i] * rg);
            }
        }

        self.packed_buffer.clear();
        self.packed_buffer.extend(self.interleaved_buffer.pack());
        let mut slice: &[u8] = &self.packed_buffer;

        while !slice.is_empty() {
            if let Some(written) = self.producer.write_blocking(slice) {
                slice = &slice[written..];
            }
        }

        Some(Ok(read))
    }
}

make_unknown_error!(windows_result::Error, StateError);
make_unknown_error!(windows_result::Error, ResetError);
make_unknown_error!(windows_result::Error, CloseError);
make_unknown_error!(windows_result::Error, InfoError);
make_unknown_error!(windows_result::Error, OpenError);
make_unknown_error!(windows_result::Error, ListError);
