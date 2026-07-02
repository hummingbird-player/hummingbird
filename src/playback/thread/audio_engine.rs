use std::path::Path;

use tracing::{error, info, trace_span, warn};

use crate::{
    devices::{
        format::{ChannelSpec, FormatInfo, SampleFormat},
        mix::{ChannelMixer, MixOptions},
        resample::Resampler,
    },
    media::{
        errors::{PlaybackStartError, SeekError},
        pipeline::{AudioPipeline, DEFAULT_BUFFER_FRAMES, DecodeResult, output_frame_bound},
        traits::F32DecodeResult,
    },
    playback::thread::media_controller::CompleteMetadata,
    settings::playback::PlaybackSettings,
};

use super::device_controller::DeviceController;
use super::media_controller::MediaController;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum EngineState {
    /// No media loaded, engine is idle.
    Idle,
    /// Media is loaded and ready to play.
    Ready,
    Playing,
    Paused,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineCycleResult {
    Continue,
    Eof,
    /// A fatal decode error occurred - should skip to next track.
    FatalError(String),
    /// Nothing to do - not in playing state or no stream available.
    NothingToDo,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct OpenInfo {
    pub duration_ms: Option<u64>,
    pub channels: ChannelSpec,
    pub device_recreated: bool,
}

#[derive(Debug)]
pub enum EngineError {
    NoPipeline,
    /// Failed to get media information.
    MediaError(String),
    DecodeError(String),
    DeviceError(String),
    InvalidState(String),
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EngineError::NoPipeline => write!(f, "No audio pipeline configured"),
            EngineError::MediaError(s) => write!(f, "Media error: {}", s),
            EngineError::DecodeError(s) => write!(f, "Decode error: {}", s),
            EngineError::DeviceError(s) => write!(f, "Device error: {}", s),
            EngineError::InvalidState(s) => write!(f, "Invalid state: {}", s),
        }
    }
}

impl std::error::Error for EngineError {}

pub struct AudioEngine {
    media: MediaController,
    device: DeviceController,
    pipeline: Option<AudioPipeline>,
    resampler: Option<Resampler>,
    /// Mixer between source-channel resampler output and device-channel input.
    mixer: Option<ChannelMixer>,
    state: EngineState,
    /// Whether a stream reset is pending (e.g., after seek).
    pending_reset: bool,
}

impl AudioEngine {
    pub fn new() -> Self {
        Self {
            media: MediaController::new(),
            device: DeviceController::new(),
            pipeline: None,
            resampler: None,
            mixer: None,
            state: EngineState::Idle,
            pending_reset: false,
        }
    }

    /// Initialize the audio engine's providers and create the initial device stream.
    ///
    /// This should be called once at startup.
    pub fn initialize(&mut self) -> Result<(), EngineError> {
        self.device.initialize_provider();

        if let Err(e) = self.device.create_stream(None) {
            error!("Failed to create initial stream: {:?}", e);
            return Err(EngineError::DeviceError(format!(
                "Failed to create initial stream: {:?}",
                e
            )));
        }

        Ok(())
    }

    pub fn state(&self) -> EngineState {
        self.state
    }

    pub fn open(
        &mut self,
        path: &Path,
        preserve_resampler: bool,
    ) -> Result<OpenInfo, PlaybackStartError> {
        info!("AudioEngine: Opening track '{}'", path.display());

        if !preserve_resampler {
            self.reset_resampler();
        }

        let mut recreation_required = false;

        if self.state != EngineState::Playing
            && self.device.has_stream()
            && let Err(err) = self.device.reset()
        {
            warn!("Failed to reset device, forcing recreation: {:?}", err);
            recreation_required = true;
        }

        if self.device.has_stream()
            && let Err(err) = self.device.play()
        {
            warn!("Failed to play device, forcing recreation: {:?}", err);
            recreation_required = true;
        }

        // Preserve the resampler for gapless reuse; rebuild the mixer per track layout.
        self.pipeline = None;
        self.mixer = None;

        let media_info = self.media.open(path)?;

        let device_recreated = if recreation_required {
            if let Err(e) = self.device.recreate_stream(true, None) {
                error!("Failed to recreate stream: {:?}", e);
                return Err(PlaybackStartError::StreamError(format!(
                    "Failed to recreate stream: {:?}",
                    e
                )));
            }

            if let Err(e) = self.device.play() {
                error!("Device was recreated and we still can't play: {:?}", e);
                panic!("couldn't play device")
            }
            true
        } else {
            false
        };

        self.state = EngineState::Playing;

        if let Some(device_format) = self.device.current_format().cloned() {
            if let Err(e) = self.setup_pipeline(&device_format) {
                self.stop();
                return Err(PlaybackStartError::MediaError(format!(
                    "Failed to set up audio pipeline: {e}"
                )));
            }

            match self.process_decode_resample() {
                Ok(DecodeStepResult::Continue) | Ok(DecodeStepResult::Eof) => {}
                Ok(DecodeStepResult::FatalError(msg)) => {
                    self.stop();
                    return Err(PlaybackStartError::MediaError(msg));
                }
                Err(e) => {
                    self.stop();
                    return Err(PlaybackStartError::MediaError(e.to_string()));
                }
            }
        }

        Ok(OpenInfo {
            duration_ms: media_info.duration_ms,
            channels: media_info.channels,
            device_recreated,
        })
    }

    /// Resume playback.
    ///
    /// If paused, this will resume the device stream.
    /// If idle with no media, this returns an error.
    pub fn play(&mut self) -> Result<(), EngineError> {
        match self.state {
            EngineState::Playing => Ok(()),
            EngineState::Paused => {
                if self.device.has_stream() {
                    if self.pending_reset {
                        if let Err(err) = self.device.reset() {
                            warn!(
                                "Failed to reset stream, recreating device instead... {:?}",
                                err
                            );
                            let channels = self.device.current_format().map(|f| f.channels.clone());
                            if let Err(e) = self.device.recreate_stream(true, channels) {
                                return Err(EngineError::DeviceError(format!(
                                    "Failed to recreate stream: {:?}",
                                    e
                                )));
                            }
                        }
                        self.pending_reset = false;
                    }

                    if let Err(err) = self.device.play() {
                        warn!(
                            "Failed to restart playback, recreating device and retrying... {:?}",
                            err
                        );
                        let channels = self.device.current_format().map(|f| f.channels.clone());
                        if let Err(e) = self.device.recreate_stream(true, channels) {
                            return Err(EngineError::DeviceError(format!(
                                "Failed to recreate stream: {:?}",
                                e
                            )));
                        }

                        if let Err(e) = self.device.play() {
                            return Err(EngineError::DeviceError(format!(
                                "Failed to start playback after recreation: {:?}",
                                e
                            )));
                        }
                    }
                }

                self.state = EngineState::Playing;
                Ok(())
            }
            EngineState::Ready => {
                if self.device.has_stream()
                    && let Err(err) = self.device.play()
                {
                    return Err(EngineError::DeviceError(format!(
                        "Failed to start playback: {:?}",
                        err
                    )));
                }
                self.state = EngineState::Playing;
                Ok(())
            }
            EngineState::Idle => Err(EngineError::InvalidState(
                "Cannot play: no media loaded".to_string(),
            )),
        }
    }

    /// Pause playback.
    pub fn pause(&mut self) -> Result<(), EngineError> {
        if self.state != EngineState::Playing {
            return Ok(());
        }

        if let Err(e) = self.device.pause() {
            warn!("Failed to pause device: {:?}", e);
        }

        self.state = EngineState::Paused;
        Ok(())
    }

    /// Stop playback and clear all state.
    pub fn stop(&mut self) {
        self.media.close();
        self.clear_pipeline();
        self.state = EngineState::Idle;
    }

    /// Seek to the specified time in seconds.
    pub fn seek(&mut self, time: f64) -> Result<(), SeekError> {
        let result = self.media.seek(time);
        if result.is_ok() {
            self.pending_reset = true;
        }
        result
    }

    /// Set the playback volume (0.0 to 1.0).
    pub fn set_volume(&mut self, volume: f64) -> Result<(), EngineError> {
        self.device
            .set_volume(volume)
            .map_err(|e| EngineError::DeviceError(format!("Failed to set volume: {:?}", e)))
    }

    /// Set the ReplayGain multiplier (linear).
    pub fn set_replaygain(&mut self, gain: f64) -> Result<(), EngineError> {
        self.device
            .set_replaygain(gain)
            .map_err(|e| EngineError::DeviceError(format!("Failed to set RG: {:?}", e)))
    }

    /// Get the current playback position in milliseconds.
    pub fn position_ms(&self) -> Option<u64> {
        self.media.position_ms().ok()
    }

    /// Get the currently loaded track path, if any.
    pub fn current_path(&self) -> Option<&Path> {
        self.media.current_path()
    }

    /// Check for metadata updates and return them if available.
    pub fn check_metadata_update(&mut self) -> Option<CompleteMetadata> {
        self.media.check_metadata_update()
    }

    /// Get the current device format, if available.
    #[allow(dead_code)]
    pub fn current_format(&self) -> Option<&FormatInfo> {
        self.device.current_format()
    }

    /// Update settings that affect playback.
    ///
    /// Currently this is a placeholder for future settings that might affect
    /// the audio engine directly (e.g., resampler quality settings).
    pub fn update_settings(&mut self, _settings: &PlaybackSettings) {
        // Currently no engine-specific settings to update.
        // This method exists for future extensibility.
    }

    /// Enable or disable loop-aware decoding on the media stream.
    pub fn set_looping(&mut self, enabled: bool) {
        self.media.set_looping(enabled);
    }

    /// Process one cycle of the audio pipeline.
    ///
    /// Returns a result indicating whether to continue, handle EOF, or handle errors.
    pub fn process_cycle(&mut self) -> EngineCycleResult {
        if self.state != EngineState::Playing {
            return EngineCycleResult::NothingToDo;
        }

        if !self.device.has_stream() || !self.media.has_stream() {
            return EngineCycleResult::NothingToDo;
        }

        if self.pipeline.is_none() {
            let device_format = match self.device.current_format() {
                Some(fmt) => fmt.clone(),
                None => {
                    error!("No device format available");
                    return EngineCycleResult::NothingToDo;
                }
            };

            if let Err(e) = self.setup_pipeline(&device_format) {
                error!("Failed to setup audio pipeline: {:?}", e);
                return EngineCycleResult::NothingToDo;
            }
        }

        let result = match self.process_decode_resample() {
            Ok(result) => result,
            Err(e) => {
                error!("Audio engine error: {:?}", e);
                return EngineCycleResult::NothingToDo;
            }
        };

        match result {
            DecodeStepResult::Eof => {
                info!("EOF, track finished");
                return EngineCycleResult::Eof;
            }
            DecodeStepResult::FatalError(msg) => {
                error!("Fatal error in audio engine");
                return EngineCycleResult::FatalError(msg);
            }
            DecodeStepResult::Continue => {}
        }

        self.consume_to_device()
    }

    /// Consume samples from pipeline to device
    fn consume_to_device(&mut self) -> EngineCycleResult {
        let s = trace_span!("consume_from").entered();

        let Some(pipeline) = &mut self.pipeline else {
            return EngineCycleResult::NothingToDo;
        };

        let consume_result = match pipeline {
            AudioPipeline::Convert(p) => self.device.consume_from(&mut p.device_input),
            AudioPipeline::F32Passthrough(p) => match self
                .device
                .consume_from_f32(&mut p.device_input)
            {
                Some(result) => result,
                None => {
                    error!("Device doesn't support f32 passthrough but pipeline is F32Passthrough");
                    return EngineCycleResult::NothingToDo;
                }
            },
        };

        if let Err(err) = consume_result {
            warn!(parent: &s, ?err, "Failed to consume from pipeline: {err}");
            warn!(parent: &s, "Recreating device and retrying...");

            let channels = self.device.current_format().map(|f| f.channels.clone());
            if let Err(e) = self.device.recreate_stream(true, channels) {
                error!(parent: &s, "Failed to recreate stream: {:?}", e);
                return EngineCycleResult::NothingToDo;
            }

            let Some(pipeline) = &mut self.pipeline else {
                return EngineCycleResult::NothingToDo;
            };

            let retry_result = match pipeline {
                AudioPipeline::Convert(p) => self.device.consume_from(&mut p.device_input),
                AudioPipeline::F32Passthrough(p) => self
                    .device
                    .consume_from_f32(&mut p.device_input)
                    .unwrap_or(Err(super::device_controller::DeviceError::NoStream)),
            };

            if let Err(err) = retry_result {
                error!(parent: &s, ?err, "Failed to consume after recreation: {err}");
                error!(
                    "This likely indicates a problem with the audio device or driver\n\
                    (or an underlying issue in the used DeviceProvider)\n\
                    Please check your audio setup and try again."
                );
                panic!("Failed to consume from pipeline after recreation");
            }
        }

        EngineCycleResult::Continue
    }

    /// Set up the audio pipeline for a new track.
    fn setup_pipeline(&mut self, device_format: &FormatInfo) -> Result<(), EngineError> {
        let source_spec = self
            .media
            .channels()
            .map_err(|e| EngineError::MediaError(format!("Failed to get channels: {:?}", e)))?;

        let source_layout = source_spec.to_layout();
        let device_layout = device_format.channels.to_layout();

        let source_channel_count = source_layout.count().max(1);
        let device_channel_count = device_layout.count().max(1);
        let channels_match = source_layout == device_layout;

        let source_format = self.media.sample_format().unwrap_or(SampleFormat::Float64);

        let source_rate = self
            .media
            .sample_rate()
            .unwrap_or(device_format.sample_rate);

        let pipeline = AudioPipeline::new(
            source_channel_count,
            source_format,
            source_rate,
            device_format.sample_type,
            device_format.sample_rate,
            device_channel_count,
            channels_match,
            DEFAULT_BUFFER_FRAMES,
        );

        if pipeline.is_passthrough() {
            info!("Using f32 passthrough pipeline (no conversion needed)");
            self.mixer = None;
        } else {
            info!("Using f64 conversion pipeline");
            if channels_match {
                self.mixer = None;
            } else {
                let mut mixer =
                    ChannelMixer::new(source_layout, device_layout, MixOptions::default());
                // Keep the mixer whenever it remaps samples, or whenever the channel
                // counts differ, so the passthrough fallback is never asked to bridge
                // mismatched counts. Equal-count identity mixes fall through to passthrough.
                if mixer.needs_mixing() || source_channel_count != device_channel_count {
                    mixer.ensure_output_capacity(output_frame_bound(
                        source_rate,
                        device_format.sample_rate,
                        DEFAULT_BUFFER_FRAMES,
                    ));
                    self.mixer = Some(mixer);
                } else {
                    self.mixer = None;
                }
            }
        }

        self.pipeline = Some(pipeline);

        Ok(())
    }

    fn clear_pipeline(&mut self) {
        self.pipeline = None;
        self.resampler = None;
        self.mixer = None;
    }

    fn reset_resampler(&mut self) {
        if let Some(resampler) = &mut self.resampler {
            resampler.reset();
        }
        if let Some(mixer) = &mut self.mixer {
            mixer.reset();
        }
        if let Some(AudioPipeline::Convert(p)) = &mut self.pipeline {
            p.clear_resampler_output();
        }
    }

    /// Process the decode and resample steps.
    fn process_decode_resample(&mut self) -> Result<DecodeStepResult, EngineError> {
        let pipeline = self.pipeline.as_mut().ok_or(EngineError::NoPipeline)?;

        match pipeline {
            AudioPipeline::F32Passthrough(p) => {
                let decode_result = match self.media.decode_into_f32(&mut p.decoder_output) {
                    Ok(F32DecodeResult::Decoded(result)) => result,
                    Ok(F32DecodeResult::NotF32) => {
                        // Source is not f32, need to switch to conversion pipeline
                        warn!("Source format changed from f32, switching to conversion pipeline");
                        return Err(EngineError::DecodeError(
                            "Format changed, need pipeline recreation".to_string(),
                        ));
                    }
                    Err(e) => {
                        return Self::handle_decode_error(e);
                    }
                };

                match decode_result {
                    DecodeResult::Eof => {
                        info!("EOF from decode_into_f32");
                        Ok(DecodeStepResult::Eof)
                    }
                    DecodeResult::Decoded { .. } => {
                        // No resampling needed in passthrough mode
                        Ok(DecodeStepResult::Continue)
                    }
                }
            }
            AudioPipeline::Convert(p) => {
                let decode_result = match self.media.decode_into(&mut p.decoder_output) {
                    Ok(result) => result,
                    Err(e) => {
                        return Self::handle_decode_error(e);
                    }
                };

                match decode_result {
                    DecodeResult::Eof => {
                        info!("EOF from decode_into");
                        return Ok(DecodeStepResult::Eof);
                    }
                    DecodeResult::Decoded { rate, .. } => {
                        if rate == p.target_rate {
                            if self.resampler.take().is_some() {
                                info!("Source rate now matches device; dropping resampler");
                            }
                        } else {
                            let duration = self.media.frame_duration().unwrap_or(1024);
                            let needs_new_resampler = match &self.resampler {
                                Some(resampler) => !resampler.matches_params(
                                    rate,
                                    p.target_rate,
                                    duration,
                                    p.source_channel_count,
                                ),
                                None => true,
                            };

                            if needs_new_resampler {
                                if self.resampler.is_some() {
                                    info!(
                                        "Stream parameters changed mid-track (rate {} -> {}, \
                                         duration {}); rebuilding resampler",
                                        p.source_rate, rate, duration
                                    );
                                }
                                self.resampler = Some(Resampler::new(
                                    rate,
                                    p.target_rate,
                                    duration,
                                    p.source_channel_count as u16,
                                ));
                            }
                        }

                        p.source_rate = rate;
                    }
                }

                match &mut self.resampler {
                    Some(resampler) => {
                        resampler.process_into(
                            &mut p.resampler_input,
                            &mut p.resampler_output,
                            DEFAULT_BUFFER_FRAMES,
                        );
                    }
                    None => {
                        Resampler::passthrough_direct(
                            &mut p.resampler_input,
                            &mut p.resampler_output,
                            DEFAULT_BUFFER_FRAMES,
                        );
                    }
                }

                if let Some(mixer) = &mut self.mixer {
                    mixer.process(&p.resampler_output, &mut p.device_input_producers);
                } else if p.source_channel_count == p.device_channel_count {
                    Self::passthrough_to_device(&p.resampler_output, &mut p.device_input_producers);
                } else {
                    // setup_pipeline guarantees a mixer whenever counts differ; reaching
                    // here would drop audio and (via write_slices) risk a panic downstream.
                    warn!(
                        "No mixer for {} -> {} channel mismatch; dropping frames",
                        p.source_channel_count, p.device_channel_count
                    );
                }

                p.clear_resampler_output();

                Ok(DecodeStepResult::Continue)
            }
        }
    }

    fn passthrough_to_device(
        input: &[Vec<f64>],
        output: &mut crate::media::pipeline::ChannelProducers<f64>,
    ) {
        let frames = input.first().map(|v| v.len()).unwrap_or(0);
        if frames == 0 {
            return;
        }
        let slices: smallvec::SmallVec<[&[f64]; 8]> = input.iter().map(|v| v.as_slice()).collect();
        output.write_slices(&slices);
    }

    /// Handle decode errors uniformly
    fn handle_decode_error(
        e: crate::media::errors::PlaybackReadError,
    ) -> Result<DecodeStepResult, EngineError> {
        use crate::media::errors::PlaybackReadError;

        match e {
            PlaybackReadError::InvalidState => {
                error!("Thread state is invalid: decoder state is invalid");
                Err(EngineError::DecodeError(
                    "Decoder in invalid state".to_string(),
                ))
            }
            PlaybackReadError::NeverStarted => {
                error!("Thread state is invalid: playback never started");
                Err(EngineError::DecodeError(
                    "Playback never started".to_string(),
                ))
            }
            PlaybackReadError::Eof => {
                info!("EOF during decode");
                Ok(DecodeStepResult::Eof)
            }
            PlaybackReadError::Unknown(s) => {
                error!("Unknown decode error: {}", s);
                warn!("Samples may be skipped");
                Ok(DecodeStepResult::Continue)
            }
            PlaybackReadError::DecodeFatal(s) => {
                error!("Fatal decoding error: {}", s);
                Ok(DecodeStepResult::FatalError(s))
            }
        }
    }
}

impl Default for AudioEngine {
    fn default() -> Self {
        Self::new()
    }
}

/// Internal result type for the decode/resample step.
enum DecodeStepResult {
    Continue,
    Eof,
    FatalError(String),
}
