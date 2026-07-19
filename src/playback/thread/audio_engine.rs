use std::path::Path;

use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use tracing::{error, info, trace_span, warn};

use crate::{
    devices::{
        format::{ChannelSpec, FormatInfo},
        mix::{ChannelMixer, MixOptions},
        resample::Resampler,
    },
    media::{
        errors::{PlaybackStartError, SeekError},
        pipeline::{AudioPipeline, DEFAULT_BUFFER_FRAMES, DecodeResult, output_frame_bound},
    },
    playback::{
        dsp::{
            equalizer::EqualizerProcessor,
            spectrum::{SpectrumTap, spectrum_tap},
        },
        events::PlaybackEvent,
        thread::media_controller::CompleteMetadata,
    },
    settings::{equalizer::EqualizerSettings, playback::PlaybackSettings},
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DrainState {
    Inactive,
    Draining { cycles: u32 },
    Drained,
}

const MAX_DRAIN_CYCLES: u32 = 1024;

/// Number of allowable rebuild attempts before giving up and skipping to the next track.
const MAX_REBUILD_ATTEMPTS: u32 = 8;

/// Overrides the default behavior of the audio pipeline if the advertised format was wrong.
#[derive(Debug, Clone, Default)]
struct PipelineOverrides {
    source_spec: Option<ChannelSpec>,
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
    /// Parametric EQ, runs on the post-mix device-rate block.
    eq: EqualizerProcessor,
    /// Spectrum taps bracketing the EQ stage, gated by the analyzer's UI flag.
    tap: SpectrumTap,
    /// Event channel to the UI, used to report the device stream rate.
    events_tx: UnboundedSender<PlaybackEvent>,
    /// Last rate reported to the UI, events only fire on change.
    reported_sample_rate: u32,
    state: EngineState,
    /// Whether a stream reset is pending (e.g., after seek).
    pending_reset: bool,
    drain: DrainState,
    /// Consecutive rebuilds without a decode producing audio; see [`MAX_REBUILD_ATTEMPTS`].
    rebuild_attempts: u32,
}

impl AudioEngine {
    pub fn new(events_tx: UnboundedSender<PlaybackEvent>, tap: SpectrumTap) -> Self {
        Self {
            media: MediaController::new(),
            device: DeviceController::new(),
            pipeline: None,
            resampler: None,
            mixer: None,
            // re-synced to the real stream format on stream creation
            eq: EqualizerProcessor::new(48_000.0, 2),
            tap,
            events_tx,
            reported_sample_rate: 0,
            state: EngineState::Idle,
            pending_reset: false,
            drain: DrainState::Inactive,
            rebuild_attempts: 0,
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

        self.sync_eq_format();

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

        self.drain = DrainState::Inactive;
        self.rebuild_attempts = 0;

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
            self.sync_eq_format();

            if let Err(e) = self.device.play() {
                error!("Device was recreated and we still can't play: {:?}", e);
                self.stop();
                return Err(PlaybackStartError::StreamError(format!(
                    "Device was recreated but playback could not start: {e:?}"
                )));
            }
            true
        } else {
            false
        };

        self.state = EngineState::Playing;

        if let Some(device_format) = self.device.current_format().cloned() {
            if let Err(e) = self.setup_pipeline(&device_format, PipelineOverrides::default()) {
                self.stop();
                return Err(PlaybackStartError::MediaError(format!(
                    "Failed to set up audio pipeline: {e}"
                )));
            }

            // Decode the first packet, and if the actual format does not match the advertised
            // format attempt to rebuild the pipeline.
            let mut attempts = 0;
            loop {
                match self.process_decode_resample() {
                    Ok(DecodeStepResult::Continue) | Ok(DecodeStepResult::Eof) => break,
                    Ok(DecodeStepResult::Rebuild(overrides)) => {
                        attempts += 1;
                        if attempts > MAX_REBUILD_ATTEMPTS {
                            self.stop();
                            return Err(PlaybackStartError::MediaError(
                                "audio format kept changing while priming the pipeline".to_string(),
                            ));
                        }
                        if let Err(e) = self.rebuild_pipeline(overrides) {
                            self.stop();
                            return Err(PlaybackStartError::MediaError(format!(
                                "Failed to rebuild audio pipeline: {e}"
                            )));
                        }
                    }
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
                            self.sync_eq_format();
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
                        self.sync_eq_format();

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

    /// Advance deferred device work (currently: completing an async pause fade), once per main-loop
    /// iteration.
    pub fn poll(&mut self) {
        if let Err(e) = self.device.poll() {
            warn!("device poll failed: {:?}", e);
        }
    }

    /// Stop playback and clear all state.
    pub fn stop(&mut self) {
        // flush the resampler's tail to the device so the last track isn't truncated
        if self.drain == DrainState::Drained && self.state == EngineState::Playing {
            self.flush_tail_to_device();
        }

        self.media.close();
        self.clear_pipeline();
        self.state = EngineState::Idle;
    }

    /// Seek to the specified time in seconds.
    pub fn seek(&mut self, time: f64) -> Result<(), SeekError> {
        let result = self.media.seek(time);
        if result.is_ok() {
            // a seek out of the EOF region resumes normal decoding
            self.drain = DrainState::Inactive;
            self.rebuild_attempts = 0;

            if self.state == EngineState::Playing {
                self.flush_for_seek();
            } else {
                self.pending_reset = true;
            }
        }
        result
    }

    /// Drop everything buffered before a seek: the device's own queue, both pipeline ring buffers,
    /// and the resampler/mixer/EQ state, so post-seek audio plays cleanly and immediately.
    fn flush_for_seek(&mut self) {
        if self.device.has_stream() {
            if let Err(err) = self.device.reset() {
                warn!("Failed to reset device on seek: {:?}", err);
            } else if let Err(err) = self.device.play() {
                warn!("Failed to resume device after seek reset: {:?}", err);
            }
        }

        if let Some(resampler) = &mut self.resampler {
            resampler.reset();
        }
        if let Some(mixer) = &mut self.mixer {
            mixer.reset();
        }
        self.eq.reset();
        if let Some(pipeline) = &mut self.pipeline {
            pipeline.flush_buffers();
        }

        self.pending_reset = false;
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

    /// Sync the EQ to the current stream format. Called after every stream (re)creation.
    fn sync_eq_format(&mut self) {
        let Some(format) = self.device.current_format() else {
            return;
        };
        if self.reported_sample_rate != format.sample_rate {
            self.reported_sample_rate = format.sample_rate;
            let _ = self
                .events_tx
                .send(PlaybackEvent::SampleRateChanged(format.sample_rate));
        }
        self.eq.set_sample_rate(f64::from(format.sample_rate));
        self.eq
            .set_channel_count(format.channels.to_layout().count().max(1));
    }

    /// Update settings that affect playback.
    pub fn update_settings(&mut self, settings: &PlaybackSettings) {
        self.eq.set_config(&settings.equalizer);
    }

    /// Apply new equalizer settings live.
    pub fn set_equalizer(&mut self, settings: &EqualizerSettings) {
        self.eq.set_config(settings);
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

        if matches!(self.drain, DrainState::Draining { .. }) {
            return self.drain_cycle();
        }

        if self.pipeline.is_none() {
            let device_format = match self.device.current_format() {
                Some(fmt) => fmt.clone(),
                None => {
                    error!("No device format available");
                    return EngineCycleResult::NothingToDo;
                }
            };

            if let Err(e) = self.setup_pipeline(&device_format, PipelineOverrides::default()) {
                error!("Failed to setup audio pipeline: {:?}", e);
                return EngineCycleResult::NothingToDo;
            }
        }

        // don't decode unless the device can accept it, otherwise we drop audio
        let frame_duration = self
            .media
            .frame_duration()
            .map(|d| d as usize)
            .unwrap_or(DEFAULT_BUFFER_FRAMES);
        let throttle = self
            .pipeline
            .as_ref()
            .is_some_and(|p| !p.can_accept_decode(frame_duration));
        if throttle {
            return self.consume_to_device();
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
                info!("EOF, draining pipeline to the device");
                self.drain = DrainState::Draining { cycles: 0 };
                return self.drain_cycle();
            }
            DecodeStepResult::FatalError(msg) => {
                error!("Fatal error in audio engine");
                return EngineCycleResult::FatalError(msg);
            }
            DecodeStepResult::Rebuild(overrides) => {
                self.rebuild_attempts += 1;
                if self.rebuild_attempts > MAX_REBUILD_ATTEMPTS {
                    error!(
                        "pipeline rebuilt {} times without progress; skipping track",
                        self.rebuild_attempts
                    );
                    return EngineCycleResult::FatalError(
                        "pipeline rebuild loop (format kept changing)".to_string(),
                    );
                }
                if let Err(e) = self.rebuild_pipeline(overrides) {
                    error!("Failed to rebuild audio pipeline: {:?}", e);
                    return EngineCycleResult::NothingToDo;
                }

                return EngineCycleResult::Continue;
            }
            DecodeStepResult::Continue => {
                self.rebuild_attempts = 0;
            }
        }

        self.consume_to_device()
    }

    /// Send as much as much as the device can accept, then wait for it to drain. If this happens
    /// too many times in a row (more than MAX_DRAIN_CYCLES) we just give up
    fn drain_cycle(&mut self) -> EngineCycleResult {
        if let DrainState::Draining { cycles } = &mut self.drain {
            *cycles += 1;
            if *cycles > MAX_DRAIN_CYCLES {
                warn!("pipeline drain did not finish within {MAX_DRAIN_CYCLES} cycles");
                warn!("reporting EOF with audio still buffered");
                self.drain = DrainState::Drained;
                return EngineCycleResult::Eof;
            }
        }

        if let Some(p) = &mut self.pipeline {
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
            Self::route_resampler_output(p, &mut self.mixer, &mut self.eq, &mut self.tap);
        }

        match self.consume_to_device() {
            EngineCycleResult::Continue => {}
            other => return other,
        }

        let empty = match &self.pipeline {
            Some(p) => {
                p.resampler_input.potentially_available() == 0
                    && p.device_input.potentially_available() == 0
            }
            None => true,
        };

        if empty {
            self.drain = DrainState::Drained;
            info!("EOF, track finished");
            return EngineCycleResult::Eof;
        }

        EngineCycleResult::Continue
    }

    /// Consume samples from pipeline to device
    fn consume_to_device(&mut self) -> EngineCycleResult {
        let s = trace_span!("consume_from").entered();

        let Some(pipeline) = &mut self.pipeline else {
            return EngineCycleResult::NothingToDo;
        };

        let consume_result = self.device.consume_from(&mut pipeline.device_input);

        if let Err(err) = consume_result {
            warn!(parent: &s, ?err, "Failed to consume from pipeline: {err}");
            warn!(parent: &s, "Recreating device and retrying...");

            let channels = self.device.current_format().map(|f| f.channels.clone());
            if let Err(e) = self.device.recreate_stream(true, channels) {
                error!(parent: &s, "Failed to recreate stream: {:?}", e);
                return EngineCycleResult::NothingToDo;
            }
            self.sync_eq_format();

            let Some(pipeline) = &mut self.pipeline else {
                return EngineCycleResult::NothingToDo;
            };

            let retry_result = self.device.consume_from(&mut pipeline.device_input);

            if let Err(err) = retry_result {
                error!(parent: &s, ?err, "Failed to consume after recreation: {err}");
                error!(
                    "This likely indicates a problem with the audio device or driver\n\
                    (or an underlying issue in the used DeviceProvider)\n\
                    Please check your audio setup and try again."
                );

                return EngineCycleResult::FatalError(format!(
                    "audio device unusable after recreation: {err}"
                ));
            }
        }

        EngineCycleResult::Continue
    }

    /// Set up the audio pipeline for a new track. `overrides` substitutes parameters the media
    /// stream advertised incorrectly (rebuild path). Otherwise, the pipeline is set up according
    /// to the media stream's advertised format.
    fn setup_pipeline(
        &mut self,
        device_format: &FormatInfo,
        overrides: PipelineOverrides,
    ) -> Result<(), EngineError> {
        let source_spec = match overrides.source_spec {
            Some(spec) => spec,
            None => self
                .media
                .channels()
                .map_err(|e| EngineError::MediaError(format!("Failed to get channels: {:?}", e)))?,
        };

        let source_layout = source_spec.to_layout();
        let device_layout = device_format.channels.to_layout();

        let source_channel_count = source_layout.count().max(1);
        let device_channel_count = device_layout.count().max(1);
        let channels_match = source_layout == device_layout;

        let source_rate = self
            .media
            .sample_rate()
            .unwrap_or(device_format.sample_rate);

        let pipeline = AudioPipeline::new(
            source_channel_count,
            device_channel_count,
            source_rate,
            device_format.sample_rate,
            DEFAULT_BUFFER_FRAMES,
        );

        if channels_match {
            self.mixer = None;
        } else {
            let mut mixer = ChannelMixer::new(source_layout, device_layout, MixOptions::default());

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

        self.pipeline = Some(pipeline);

        Ok(())
    }

    fn clear_pipeline(&mut self) {
        self.pipeline = None;
        self.resampler = None;
        self.mixer = None;
        self.eq.reset();
        self.drain = DrainState::Inactive;
        self.rebuild_attempts = 0;
    }

    /// Rebuild the pipeline mid-track after a format/rate/channel mismatch. Drops all buffered
    /// audio, including the resampler.
    fn rebuild_pipeline(&mut self, overrides: PipelineOverrides) -> Result<(), EngineError> {
        let device_format =
            self.device.current_format().cloned().ok_or_else(|| {
                EngineError::DeviceError("no device format for rebuild".to_string())
            })?;

        info!(
            "Rebuilding audio pipeline (channels={:?})",
            overrides.source_spec
        );

        self.pipeline = None;
        self.mixer = None;
        if let Some(resampler) = &mut self.resampler {
            resampler.reset();
        }

        self.setup_pipeline(&device_format, overrides)
    }

    fn reset_resampler(&mut self) {
        if let Some(resampler) = &mut self.resampler {
            resampler.reset();
        }
        if let Some(mixer) = &mut self.mixer {
            mixer.reset();
        }
        if let Some(p) = &mut self.pipeline {
            p.clear_resampler_output();
        }
    }

    /// Process the decode and resample steps.
    fn process_decode_resample(&mut self) -> Result<DecodeStepResult, EngineError> {
        let p = self.pipeline.as_mut().ok_or(EngineError::NoPipeline)?;

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
                    if let Some(mut old) = self.resampler.take() {
                        info!("Source rate now matches device; dropping resampler");
                        Self::flush_old_resampler(
                            &mut old,
                            p,
                            &mut self.mixer,
                            &mut self.eq,
                            &mut self.tap,
                        );
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
                        if let Some(mut old) = self.resampler.take() {
                            info!(
                                "Stream parameters changed (rate {} -> {}, \
                                 duration {}); flushing and rebuilding resampler",
                                p.source_rate, rate, duration
                            );
                            Self::flush_old_resampler(
                                &mut old,
                                p,
                                &mut self.mixer,
                                &mut self.eq,
                                &mut self.tap,
                            );
                        }
                        let resampler = Resampler::new(
                            rate,
                            p.target_rate,
                            duration,
                            p.source_channel_count as u16,
                        );
                        // a cycle can push several blocks through the resampler, so make sure the
                        // handoff buffer can absorb the worst case without reallocating later
                        let blocks = DEFAULT_BUFFER_FRAMES.div_ceil(duration.max(1) as usize);
                        p.ensure_resampler_output_capacity(blocks * resampler.output_frames_max());
                        self.resampler = Some(resampler);
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

        Self::route_resampler_output(p, &mut self.mixer, &mut self.eq, &mut self.tap);

        Ok(DecodeStepResult::Continue)
    }

    fn route_resampler_output(
        p: &mut AudioPipeline,
        mixer: &mut Option<ChannelMixer>,
        eq: &mut EqualizerProcessor,
        tap: &mut SpectrumTap,
    ) {
        let result = if let Some(mixer) = mixer {
            let frames = mixer.mix(&p.resampler_output);
            Self::eq_with_tap(eq, tap, mixer.output_planes_mut(), frames);
            Self::passthrough_to_device(
                mixer.output_planes(),
                frames,
                &mut p.device_input_producers,
            )
        } else if p.source_channel_count == p.device_channel_count {
            let frames = p.resampler_output.iter().map(Vec::len).min().unwrap_or(0);
            Self::eq_with_tap(eq, tap, &mut p.resampler_output, frames);
            Self::passthrough_to_device(&p.resampler_output, frames, &mut p.device_input_producers)
        } else {
            warn!(
                "No mixer for {} -> {} channel mismatch; dropping frames",
                p.source_channel_count, p.device_channel_count
            );
            Ok(())
        };

        // the device ring is sized for a worst-case cycle and drained on this same thread, so a
        // failed write means an engine bug - make it loud instead of losing audio silently
        if let Err(e) = result {
            error!("failed to hand resampler output to the device stage: {e:?}");
        }

        p.clear_resampler_output();
    }

    /// Tap the planes around the EQ stage, pre and post rings always see the same frames.
    fn eq_with_tap(
        eq: &mut EqualizerProcessor,
        tap: &mut SpectrumTap,
        planes: &mut [Vec<f64>],
        frames: usize,
    ) {
        let tapped = tap.push_pre(planes, frames);
        eq.process(planes, frames);
        tap.push_post(planes, tapped);
    }

    /// Flush the resampler and clear its output.
    fn flush_old_resampler(
        old: &mut Resampler,
        p: &mut AudioPipeline,
        mixer: &mut Option<ChannelMixer>,
        eq: &mut EqualizerProcessor,
        tap: &mut SpectrumTap,
    ) {
        if old.channels() != p.source_channel_count {
            warn!(
                "dropping resampler tail: channel count changed ({} -> {})",
                old.channels(),
                p.source_channel_count
            );
            return;
        }

        let flushed = old.flush_into(&mut p.resampler_output);
        if flushed > 0 {
            info!("flushed {flushed} tail frames from the previous resampler");
            Self::route_resampler_output(p, mixer, eq, tap);
        }
    }

    fn flush_tail_to_device(&mut self) {
        let Some(resampler) = &mut self.resampler else {
            return;
        };
        let Some(p) = &mut self.pipeline else {
            return;
        };

        Self::flush_old_resampler(resampler, p, &mut self.mixer, &mut self.eq, &mut self.tap);

        // do it a few times to ensure all buffered frames are flushed (in case the entire buffer is
        // not consumed in a single pass)
        for _ in 0..8 {
            if p.device_input.potentially_available() == 0 {
                break;
            }
            if let Err(err) = self.device.consume_from(&mut p.device_input) {
                warn!("failed to hand the flushed tail to the device: {err}");
                break;
            }
        }
    }

    fn passthrough_to_device(
        input: &[Vec<f64>],
        frames: usize,
        output: &mut crate::media::pipeline::ChannelProducers<f64>,
    ) -> Result<(), crate::media::pipeline::WriteError> {
        if frames == 0 {
            return Ok(());
        }
        let slices: smallvec::SmallVec<[&[f64]; 8]> = input.iter().map(|v| &v[..frames]).collect();

        output.write_slices(&slices)
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
            PlaybackReadError::ChannelCountChanged(count) => {
                warn!("decoded channel count changed to {count}; rebuilding pipeline");
                Ok(DecodeStepResult::Rebuild(PipelineOverrides {
                    source_spec: Some(ChannelSpec::Count(count.min(usize::from(u16::MAX)) as u16)),
                }))
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
        Self::new(unbounded_channel().0, spectrum_tap().0)
    }
}

/// Internal result type for the decode/resample step.
enum DecodeStepResult {
    Continue,
    Eof,
    FatalError(String),
    Rebuild(PipelineOverrides),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devices::channels::{ChannelLayout, ChannelPosition};
    use crate::settings::equalizer::{EqBandKind, EqBandSettings, EqualizerSettings};

    fn sine(frequency: f64, frames: usize, sample_rate: f64) -> Vec<f64> {
        (0..frames)
            .map(|i| (2.0 * std::f64::consts::PI * frequency * i as f64 / sample_rate).sin())
            .collect()
    }

    fn config(kind: EqBandKind, frequency: f64, gain_db: f64, enabled: bool) -> EqualizerSettings {
        EqualizerSettings {
            enabled,
            bands: vec![EqBandSettings {
                kind,
                frequency,
                gain_db,
                q: 1.0,
                enabled: true,
            }],
        }
    }

    fn peak(planes: &[Vec<f64>]) -> f64 {
        planes
            .iter()
            .flat_map(|p| p.iter())
            .fold(0.0_f64, |peak, &s| peak.max(s.abs()))
    }

    #[test]
    fn route_passthrough_applies_eq() {
        let mut p = AudioPipeline::new(2, 2, 48_000, 48_000, 64);
        let mut eq = EqualizerProcessor::new(48_000.0, 2);
        eq.set_config(&config(EqBandKind::Bell, 1_000.0, 24.0, true));

        let dry = sine(1_000.0, 64 * 64, 48_000.0);
        let in_peak = peak(std::slice::from_ref(&dry));
        let (mut tap, _consumer) = spectrum_tap();
        let mut out_peak = 0.0;
        // the gain ramp needs a few blocks to converge
        for block in 0..64 {
            let chunk: Vec<f64> = dry[block * 64..(block + 1) * 64].to_vec();
            p.resampler_output = vec![chunk.clone(), chunk];
            AudioEngine::route_resampler_output(&mut p, &mut None, &mut eq, &mut tap);
            assert_eq!(p.device_input.try_read_to_staging(64), 64);
            out_peak = peak(p.device_input.staging());
        }

        assert!(out_peak > in_peak * 10.0);
        assert!(out_peak < in_peak * 25.0);
    }

    #[test]
    fn route_passthrough_bypassed_eq_is_bit_exact() {
        let mut p = AudioPipeline::new(2, 2, 48_000, 48_000, 64);
        let mut eq = EqualizerProcessor::new(48_000.0, 2);
        eq.set_config(&config(EqBandKind::Bell, 1_000.0, 24.0, false));

        let dry = sine(1_000.0, 64, 48_000.0);
        let (mut tap, _consumer) = spectrum_tap();
        p.resampler_output = vec![dry.clone(), dry.clone()];
        AudioEngine::route_resampler_output(&mut p, &mut None, &mut eq, &mut tap);

        assert_eq!(p.device_input.try_read_to_staging(64), 64);
        assert_eq!(p.device_input.staging()[0], dry);
    }

    #[test]
    fn route_mixer_path_applies_eq_after_mixing() {
        let mut p = AudioPipeline::new(1, 2, 48_000, 48_000, 64);
        let mut mixer = Some(ChannelMixer::new(
            ChannelLayout::Positioned(ChannelPosition::FRONT_CENTER),
            ChannelLayout::Positioned(ChannelPosition::FRONT_LEFT | ChannelPosition::FRONT_RIGHT),
            MixOptions::default(),
        ));
        let mut eq = EqualizerProcessor::new(48_000.0, 2);
        eq.set_config(&config(EqBandKind::Notch, 1_000.0, 0.0, true));

        let dry = sine(1_000.0, 64 * 64, 48_000.0);
        let (mut tap, _consumer) = spectrum_tap();
        let mut out_peak = 1.0;
        for block in 0..64 {
            p.resampler_output = vec![dry[block * 64..(block + 1) * 64].to_vec()];
            AudioEngine::route_resampler_output(&mut p, &mut mixer, &mut eq, &mut tap);
            assert_eq!(p.device_input.try_read_to_staging(64), 64);
            assert_eq!(p.device_input.staging().len(), 2);
            out_peak = peak(p.device_input.staging());
        }

        assert!(out_peak < 0.01);
    }
}
