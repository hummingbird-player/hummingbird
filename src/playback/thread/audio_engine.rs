use std::path::Path;

use tracing::{error, info, trace_span, warn};

use crate::{
    devices::{
        format::{ChannelSpec, FormatInfo, SampleFormat},
        resample::Resampler,
    },
    media::{
        errors::{PlaybackStartError, SeekError},
        metadata::Metadata,
        pipeline::{AudioPipeline, DEFAULT_BUFFER_FRAMES, DecodeResult},
        traits::F32DecodeResult,
    },
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
    pub duration_secs: Option<u64>,
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
            state: EngineState::Idle,
            pending_reset: false,
        }
    }

    /// Initialize the audio engine's providers and create the initial device stream.
    ///
    /// This should be called once at startup.
    pub fn initialize(&mut self) -> Result<(), EngineError> {
        self.media.initialize_provider();
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

    pub fn open(&mut self, path: &Path) -> Result<OpenInfo, PlaybackStartError> {
        info!("AudioEngine: Opening track '{}'", path.display());

        self.reset_resampler();

        // Handle paused state - reset device if needed
        let mut recreation_required = false;

        if self.state == EngineState::Paused && self.device.has_stream() {
            if let Err(err) = self.device.reset() {
                warn!("Failed to reset device, forcing recreation: {:?}", err);
                recreation_required = true;
            }
        }

        if self.device.has_stream() {
            if let Err(err) = self.device.play() {
                warn!("Failed to play device, forcing recreation: {:?}", err);
                recreation_required = true;
            }
        }

        // Clear the pipeline for the new track, but preserve the resampler for gapless playback
        // The resampler will be reused if params match, or recreated in process_decode_resample if needed
        self.pipeline = None;

        let media_info = self.media.open(path)?;

        // Check if we need to recreate the stream for different channel count
        if self.device.needs_format_change(media_info.channels) {
            info!(
                "Channel count mismatch, re-opening with the correct channel count (if supported)"
            );
            recreation_required = true;
        }

        let device_recreated = if recreation_required {
            if let Err(e) = self.device.recreate_stream(true, Some(media_info.channels)) {
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

        Ok(OpenInfo {
            duration_secs: media_info.duration_secs,
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
                            let channels = self.device.current_format().map(|f| f.channels);
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
                        let channels = self.device.current_format().map(|f| f.channels);
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
                if self.device.has_stream() {
                    if let Err(err) = self.device.play() {
                        return Err(EngineError::DeviceError(format!(
                            "Failed to start playback: {:?}",
                            err
                        )));
                    }
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

    /// Get the current playback position in seconds.
    pub fn position_secs(&self) -> Option<u64> {
        self.media.position_secs().ok()
    }

    /// Check for metadata updates and return them if available.
    pub fn check_metadata_update(&mut self) -> Option<(Box<Metadata>, Option<Box<[u8]>>)> {
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

        // Set up pipeline if not already done
        if self.pipeline.is_none() {
            let device_format = match self.device.current_format() {
                Some(fmt) => *fmt,
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

        // Process decode -> resample (or passthrough)
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

        // Send samples to device
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
            AudioPipeline::F32Passthrough(p) => {
                // Try f32 passthrough first
                match self.device.consume_from_f32(&mut p.device_input) {
                    Some(result) => result.map_err(|e| e),
                    None => {
                        // Device doesn't support f32 passthrough, this shouldn't happen
                        // if pipeline was set up correctly
                        error!(
                            "Device doesn't support f32 passthrough but pipeline is F32Passthrough"
                        );
                        return EngineCycleResult::NothingToDo;
                    }
                }
            }
        };

        if let Err(err) = consume_result {
            warn!(parent: &s, ?err, "Failed to consume from pipeline: {err}");
            warn!(parent: &s, "Recreating device and retrying...");

            let channels = self.device.current_format().map(|f| f.channels);
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
                    .unwrap_or_else(|| Err(super::device_controller::DeviceError::NoStream)),
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

    //
    // Private helper methods
    //

    /// Set up the audio pipeline for a new track.
    ///
    /// This method determines whether to use f32 passthrough or f64 conversion pipeline
    /// based on the source and device formats.
    ///
    /// Note: This preserves the existing resampler if one exists. The resampler will be
    /// reused if its parameters match the new track, or recreated in process_decode_resample
    /// when the actual source rate becomes known after the first decode.
    fn setup_pipeline(&mut self, device_format: &FormatInfo) -> Result<(), EngineError> {
        let channels = self
            .media
            .channels()
            .map_err(|e| EngineError::MediaError(format!("Failed to get channels: {:?}", e)))?;

        let channel_count = channels.count() as usize;

        // Get source format to determine if passthrough is possible
        let source_format = self.media.sample_format().unwrap_or(SampleFormat::Float32); // Default to f32 if unknown

        // Get actual source sample rate from the media file
        let source_rate = self
            .media
            .sample_rate()
            .unwrap_or(device_format.sample_rate); // Fallback to device rate if unavailable

        let pipeline = AudioPipeline::new(
            channel_count,
            source_format,
            source_rate,
            device_format.sample_type,
            device_format.sample_rate,
            DEFAULT_BUFFER_FRAMES,
        );

        if pipeline.is_passthrough() {
            info!("Using f32 passthrough pipeline (no conversion needed)");
        } else {
            info!("Using f64 conversion pipeline");
        }

        self.pipeline = Some(pipeline);

        Ok(())
    }

    /// Clear the pipeline and resampler completely (e.g., on stop).
    /// For track transitions, prefer clearing only the pipeline to preserve the resampler for gapless playback.
    fn clear_pipeline(&mut self) {
        self.pipeline = None;
        self.resampler = None;
    }

    /// Reset the resampler's internal buffers (e.g., on track change).
    fn reset_resampler(&mut self) {
        if let Some(resampler) = &mut self.resampler {
            resampler.reset();
        }
    }

    /// Process the decode and resample steps.
    fn process_decode_resample(&mut self) -> Result<DecodeStepResult, EngineError> {
        let pipeline = self.pipeline.as_mut().ok_or(EngineError::NoPipeline)?;

        match pipeline {
            AudioPipeline::F32Passthrough(p) => {
                let decode_result = match self.media.decode_into_f32(&p.decoder_output) {
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
                let decode_result = match self.media.decode_into(&p.decoder_output) {
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
                        // Only recreate resampler if parameters actually changed
                        let duration = self.media.frame_duration().unwrap_or(1024);
                        let needs_new_resampler = match &self.resampler {
                            Some(resampler) => !resampler.matches_params(
                                rate,
                                p.target_rate,
                                duration,
                                p.channel_count,
                            ),
                            None => true,
                        };

                        if needs_new_resampler {
                            self.resampler = Some(Resampler::new(
                                rate,
                                p.target_rate,
                                duration,
                                p.channel_count as u16,
                            ));
                        }

                        p.source_rate = rate;
                    }
                }

                if let Some(resampler) = &mut self.resampler {
                    let _processed = resampler.process_ring_buffers(
                        &mut p.resampler_input,
                        &p.device_input_producers,
                        DEFAULT_BUFFER_FRAMES,
                    );
                }

                Ok(DecodeStepResult::Continue)
            }
        }
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
