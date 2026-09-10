use std::time::{Duration, Instant};

use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use tracing::{debug, error, info, trace_span, warn};

use crate::{
    devices::{
        format::{ChannelSpec, FormatInfo},
        mix::{ChannelMixer, MixOptions},
        resample::Resampler,
    },
    library::source::TrackRef,
    media::{
        errors::{PlaybackStartError, SeekError},
        pipeline::{AudioPipeline, DEFAULT_BUFFER_FRAMES, DecodeResult, output_frame_bound},
        traits::MediaResolver,
    },
    playback::{
        dsp::{
            equalizer::EqualizerProcessor,
            spectrum::{SpectrumTap, spectrum_tap},
        },
        events::PlaybackEvent,
        thread::media_controller::{CompleteMetadata, SeekOutcome},
    },
    settings::{equalizer::EqualizerSettings, playback::PlaybackSettings},
};

use super::device_controller::DeviceController;
use super::media_controller::MediaController;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineState {
    /// No media loaded, engine is idle.
    Idle,
    Playing,
    Paused,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DrainState {
    Inactive,
    SourceEnded,
    Draining,
    Drained,
}

/// Number of allowable rebuild attempts before giving up and skipping to the next track.
const MAX_REBUILD_ATTEMPTS: u32 = 8;
const DEVICE_RETRY_INTERVAL: Duration = Duration::from_millis(250);

/// Overrides the default behavior of the audio pipeline if the advertised format was wrong.
#[derive(Debug, Clone, Default)]
struct PipelineOverrides {
    source_spec: Option<ChannelSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineCycleResult {
    Continue,
    /// The source has ended, but queued audio may still be playing.
    SourceEof,
    /// Waiting for the decoder worker to reply. This is not an error.
    Pending,
    /// Output is full. No input was consumed and the controller should retry later.
    Backpressured,
    Eof,
    /// A fatal decode error occurred - should skip to next track.
    FatalError(String),
    /// Nothing to do - not in playing state or no stream available.
    NothingToDo,
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
    /// Whether to keep the resampler when the new source finishes opening.
    /// `None` means no open is pending. This does not control whether playback is paused.
    opening: Option<bool>,
    opened: Option<Result<Option<u64>, PlaybackStartError>>,
    timing: super::submission_timing::SubmissionTiming,
    starts_track: bool,
    track_serial: u64,
    previous_pipeline: Option<AudioPipeline>,
    previous_mixer: Option<ChannelMixer>,
    timing_delay_pending: bool,
    seek_outcome: Option<SeekOutcome>,
    device_retry_at: Option<Instant>,
}

impl AudioEngine {
    pub fn new(events_tx: UnboundedSender<PlaybackEvent>, tap: SpectrumTap) -> Self {
        Self::with_resolver(events_tx, tap, crate::media::traits::local_media_resolver())
    }

    pub fn with_resolver(
        events_tx: UnboundedSender<PlaybackEvent>,
        tap: SpectrumTap,
        resolver: std::sync::Arc<dyn MediaResolver>,
    ) -> Self {
        Self {
            media: MediaController::with_resolver(resolver),
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
            opening: None,
            opened: None,
            timing: super::submission_timing::SubmissionTiming::new(),
            starts_track: false,
            track_serial: 0,
            previous_pipeline: None,
            previous_mixer: None,
            timing_delay_pending: true,
            seek_outcome: None,
            device_retry_at: None,
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

    #[cfg(test)]
    pub(super) fn replace_media(&mut self, media: MediaController) {
        self.media = media;
    }

    pub fn open(
        &mut self,
        track: impl Into<TrackRef>,
        preserve_resampler: bool,
    ) -> Result<u64, PlaybackStartError> {
        if preserve_resampler {
            self.previous_pipeline = self.pipeline.take();
            self.previous_mixer = self.mixer.take();
        } else {
            self.previous_pipeline = None;
            self.previous_mixer = None;
            self.pipeline = None;
            self.timing.clear(None);
            if self.device.has_stream()
                && let Err(error) = self.device.reset()
            {
                self.device_failed(error);
            }
        }
        self.starts_track = true;
        self.seek_outcome = None;
        self.track_serial += 1;
        self.mixer = None;
        if !preserve_resampler {
            self.reset_resampler();
        }
        self.drain = DrainState::Inactive;
        self.opened = None;
        self.opening = Some(preserve_resampler);
        self.state = EngineState::Playing;
        self.media.open(track);
        Ok(self.track_serial)
    }

    pub fn take_opened(&mut self) -> Option<Result<Option<u64>, PlaybackStartError>> {
        self.opened.take()
    }

    pub fn take_started(&mut self) -> Option<u64> {
        self.timing.take_started()
    }

    pub fn source_has_pending_start(&self) -> bool {
        self.timing.has_track(self.track_serial)
    }
    pub fn has_pending_start(&self) -> bool {
        self.timing.has_started()
    }

    pub fn next_poll_delay(&self) -> Option<std::time::Duration> {
        self.media
            .next_poll_delay()
            .into_iter()
            .chain(self.device.next_poll_delay())
            .chain(
                self.device_retry_at
                    .map(|at| at.saturating_duration_since(Instant::now())),
            )
            .min()
    }

    pub fn shutdown(&mut self) {
        self.media.shutdown();
    }

    pub fn pending_poll_delay(&self) -> Duration {
        if self.device.has_stream() {
            Duration::from_millis(2)
        } else {
            self.next_poll_delay().unwrap_or(DEVICE_RETRY_INTERVAL)
        }
    }

    pub fn finish_playback(&mut self) {
        self.drain = DrainState::Draining;
        if let Err(error) = self.timing.finish_resampler() {
            error!("unable to finish playback timeline: {error}");
        }
        if let (Some(resampler), Some(pipeline)) = (&mut self.resampler, &mut self.pipeline) {
            resampler.flush_into(&mut pipeline.resampler_output);
            if let Err(e) =
                Self::route_resampler_output(pipeline, &mut self.mixer, &mut self.eq, &mut self.tap)
            {
                error!("failed to queue final resampler samples: {e:?}");
            }
        }
    }

    #[cfg(test)]
    pub fn decode_allocations(&self) -> u64 {
        self.media.decode_allocations()
    }

    fn finish_open(
        &mut self,
        media_info: super::media_controller::MediaInfo,
        preserve_resampler: bool,
    ) -> Result<Option<u64>, PlaybackStartError> {
        let track = self.media.current_track().unwrap();
        info!("AudioEngine: Opening track '{}'", track.display());
        if preserve_resampler
            && let Some(previous) = &mut self.previous_pipeline
            && (previous.source_rate != self.media.sample_rate().unwrap_or(previous.source_rate)
                || previous.decode_block.channels() != &media_info.channels)
            && let Some(mut old) = self.resampler.take()
        {
            self.timing
                .finish_resampler()
                .map_err(|error| PlaybackStartError::MediaError(error.into()))?;
            Self::flush_old_resampler(
                &mut old,
                previous,
                &mut self.previous_mixer,
                &mut self.eq,
                &mut self.tap,
            );
        }

        self.drain = DrainState::Inactive;
        self.rebuild_attempts = 0;

        if !preserve_resampler {
            self.reset_resampler();
        }

        if self.state == EngineState::Playing
            && self.device.has_stream()
            && let Err(err) = self.device.play()
        {
            self.device_failed(err);
        }

        // Preserve the resampler for gapless reuse; rebuild the mixer per track layout.
        self.pipeline = None;
        self.mixer = None;

        if let Some(device_format) = self.device.current_format().cloned()
            && let Err(e) = self.setup_pipeline(&device_format, PipelineOverrides::default())
        {
            self.stop();
            return Err(PlaybackStartError::MediaError(format!(
                "Failed to set up audio pipeline: {e}"
            )));
        }

        Ok(media_info.duration_ms)
    }

    /// Resume playback.
    ///
    /// If paused, this will resume the device stream.
    /// If idle with no media, this returns an error.
    pub fn play(&mut self) -> Result<(), EngineError> {
        if self.state == EngineState::Idle {
            return Err(EngineError::InvalidState(
                "Cannot play: no media loaded".to_string(),
            ));
        }
        if self.state == EngineState::Playing {
            return Ok(());
        }
        self.state = EngineState::Playing;
        if self.pending_reset && self.device.has_stream() {
            if let Err(error) = self.device.reset() {
                self.device_failed(error);
            }
            self.eq.reset();
            self.pending_reset = false;
        }
        if self.device.has_stream()
            && let Err(error) = self.device.play()
        {
            self.device_failed(error);
        }
        Ok(())
    }

    /// Pause playback.
    pub fn pause(&mut self) -> Result<(), EngineError> {
        if self.state != EngineState::Playing {
            return Ok(());
        }

        if self.device.has_stream()
            && let Err(e) = self.device.pause()
        {
            self.device_failed(e);
        }

        self.state = EngineState::Paused;
        Ok(())
    }

    /// Handle available worker replies and check whether pending device work has finished.
    /// Returns without waiting if either is still busy.
    pub fn poll(&mut self) {
        self.retry_device();
        self.media
            .set_decode_enabled(self.state == EngineState::Playing && self.device.has_stream());
        self.media.poll();
        if let Some(outcome) = self.media.take_seek_outcome() {
            if let SeekOutcome::Completed(position) = outcome {
                self.timing.seeked(position, self.track_serial);
            }
            self.seek_outcome = Some(outcome);
        }
        if let Some(result) = self.media.take_opened() {
            let preserve = self.opening.take().unwrap_or(false);
            let result = result.and_then(|info| self.finish_open(info, preserve));
            if result.is_err() {
                self.stop();
            }
            self.opened = Some(result);
        }
        if let Err(e) = self.device.poll() {
            self.device_failed(e);
        }
    }

    fn device_failed(&mut self, error: super::device_controller::DeviceError) {
        warn!("audio output unavailable: {error}; waiting for a usable device");
        self.device.close_stream();
        self.device_retry_at =
            (self.state != EngineState::Idle).then(|| Instant::now() + DEVICE_RETRY_INTERVAL);
    }

    fn retry_device(&mut self) {
        if self.state == EngineState::Idle
            || self.media.current_track().is_none()
            || self.device.has_stream()
            || self.device_retry_at.is_some_and(|at| at > Instant::now())
        {
            return;
        }

        let result = self.device.create_stream(None).and_then(|format| {
            if self.state == EngineState::Playing {
                self.device.play()?;
            }
            Ok(format)
        });
        let format = match result {
            Ok(format) => format,
            Err(error) => {
                debug!("audio output still unavailable: {error}");
                self.device.close_stream();
                self.device_retry_at = Some(Instant::now() + DEVICE_RETRY_INTERVAL);
                return;
            }
        };
        self.device_retry_at = None;
        self.pending_reset = false;
        self.sync_eq_format();

        let changed = self
            .pipeline
            .iter()
            .chain(self.previous_pipeline.iter())
            .any(|pipeline| {
                pipeline.target_rate != format.sample_rate
                    || pipeline.device_channel_count != usize::from(format.channels.count())
            });
        if changed {
            // prepared PCM is for the old device format; decode again from the submitted position
            let position = self.timing.position_for(self.track_serial).unwrap_or(0);
            self.starts_track |= self.timing.has_track(self.track_serial);
            self.clear_pipeline();
            self.timing.clear(Some(position));
            self.timing_delay_pending = true;
            if self.media.has_stream()
                && !self.media.is_seeking()
                && let Err(error) = self.media.seek(position as f64 / 1000.0)
            {
                warn!("could not restore position after output format changed: {error}");
            }
        }
    }

    /// Stop playback and clear all state.
    pub fn stop(&mut self) {
        self.clear_source();
        let _ = self.device.reset();
        let _ = self.device.pause();
    }

    pub fn complete(&mut self) {
        self.clear_source();
        self.device.finish();
    }

    fn clear_source(&mut self) {
        self.device_retry_at = None;
        self.media.close();
        self.opening = None;
        self.opened = None;
        self.previous_pipeline = None;
        self.timing.clear(None);
        self.clear_pipeline();
        self.state = EngineState::Idle;
    }

    /// Seek to the specified time in seconds.
    pub fn seek(&mut self, time: f64) -> Result<(), SeekError> {
        self.seek_outcome = None;
        let result = self.media.seek(time);
        if result.is_ok() {
            self.timing.clear(self.timing.position);
            self.previous_pipeline = None;
            self.previous_mixer = None;
            if let Some(pipeline) = &mut self.pipeline {
                pipeline.flush_buffers();
            }
            self.reset_resampler();
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
                self.device_failed(err);
            } else if let Err(err) = self.device.play() {
                self.device_failed(err);
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

    pub(super) fn take_seek_outcome(&mut self) -> Option<SeekOutcome> {
        self.seek_outcome.take()
    }

    /// Position of the last frames accepted by the device, not the decoder's read position.
    pub fn position_ms(&self) -> Option<u64> {
        self.timing.position
    }

    /// Get the currently loaded track reference, if any.
    pub fn current_track(&self) -> Option<&TrackRef> {
        self.media.current_track()
    }

    /// Check for metadata updates and return them if available.
    pub fn check_metadata_update(&mut self) -> Option<CompleteMetadata> {
        self.media.check_metadata_update()
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
        if format.sample_rate > 0 {
            self.eq.set_sample_rate(f64::from(format.sample_rate));
        }
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
        self.poll();
        if self.state != EngineState::Playing {
            return EngineCycleResult::NothingToDo;
        }

        if !self.device.has_stream() {
            return EngineCycleResult::Pending;
        }

        if self.opening.is_some() {
            if self.previous_pipeline.is_some() {
                let result = self.consume_to_device();
                if matches!(result, EngineCycleResult::FatalError(_)) {
                    return result;
                }
            }
            return EngineCycleResult::Pending;
        }
        if !self.media.has_stream() {
            return if self.media.current_track().is_some() {
                EngineCycleResult::Pending
            } else {
                EngineCycleResult::NothingToDo
            };
        }

        if self.previous_pipeline.is_some() {
            return self.consume_to_device();
        }

        if matches!(self.drain, DrainState::SourceEnded | DrainState::Draining) {
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

        // one small decode block can finish a much larger resampler chunk, so leave room for
        // the whole chunk - this thread also has to drain the device ring
        let output_bound = self.resampler.as_ref().map_or_else(
            || {
                self.pipeline
                    .as_ref()
                    .map_or(0, |p| p.decode_block.frame_capacity())
            },
            Resampler::output_frames_max,
        );
        if let Some(p) = &mut self.pipeline {
            p.ensure_device_input_capacity(output_bound);
        }
        let throttle = self
            .pipeline
            .as_ref()
            .is_some_and(|p| !p.can_accept_output(output_bound));
        if throttle {
            return self.consume_to_device();
        }

        let result = match self.process_decode_resample() {
            Ok(result) => result,
            Err(e) => {
                error!("Audio engine error: {:?}", e);
                return EngineCycleResult::FatalError(e.to_string());
            }
        };

        match result {
            DecodeStepResult::Pending => {
                let result = self.consume_to_device();
                return if result == EngineCycleResult::Continue {
                    EngineCycleResult::Pending
                } else {
                    result
                };
            }
            DecodeStepResult::Eof => {
                info!("EOF, draining pipeline to the device");
                self.drain = DrainState::SourceEnded;
                return EngineCycleResult::SourceEof;
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

    /// Submit the remaining audio without treating a full device as a source failure.
    fn drain_cycle(&mut self) -> EngineCycleResult {
        match self.consume_to_device() {
            EngineCycleResult::Continue => {}
            EngineCycleResult::Backpressured => return EngineCycleResult::Backpressured,
            other => return other,
        }

        let empty = match &self.pipeline {
            Some(p) => p.device_input.potentially_available() == 0,
            None => true,
        };

        if self.drain == DrainState::SourceEnded {
            return EngineCycleResult::Pending;
        }
        if empty && self.device.queued_frames() == 0 {
            self.drain = DrainState::Drained;
            info!("EOF, track finished");
            return EngineCycleResult::Eof;
        }

        EngineCycleResult::Backpressured
    }

    /// Consume samples from pipeline to device
    fn consume_to_device(&mut self) -> EngineCycleResult {
        if let Some(previous) = &mut self.previous_pipeline {
            match self.device.consume_from(&mut previous.device_input) {
                Ok(accepted) => {
                    self.timing.accept(accepted);
                    if previous.device_input.potentially_available() > 0 {
                        return EngineCycleResult::Backpressured;
                    }
                    if self.opening.is_some() {
                        return EngineCycleResult::Pending;
                    }
                    self.previous_pipeline = None;
                    self.previous_mixer = None;
                }
                Err(e) => {
                    self.device_failed(e);
                    return EngineCycleResult::Pending;
                }
            }
        }
        let _span = trace_span!("consume_from").entered();

        let Some(pipeline) = &mut self.pipeline else {
            return EngineCycleResult::NothingToDo;
        };

        let had_input = pipeline.device_input.potentially_available() > 0;
        let consume_result = self.device.consume_from(&mut pipeline.device_input);

        match consume_result {
            Err(error) => {
                self.device_failed(error);
                return EngineCycleResult::Pending;
            }
            Ok(accepted) => {
                self.timing.accept(accepted);
                if had_input && accepted == 0 {
                    return EngineCycleResult::Backpressured;
                }
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
            source_spec,
            device_channel_count,
            source_rate,
            device_format.sample_rate,
            DEFAULT_BUFFER_FRAMES,
        )
        .map_err(|error| EngineError::MediaError(format!("invalid source format: {error:?}")))?;

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
        self.previous_pipeline = None;
        self.previous_mixer = None;
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
        self.starts_track |= self.timing.has_track(self.track_serial);
        self.timing.clear(self.timing.position);
        self.timing_delay_pending = true;
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
        self.timing_delay_pending = true;
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

        let has_pending_input = p.decode_offset < p.decode_block.frames();
        if !has_pending_input {
            p.decode_offset = 0;
            p.decode_block.clear();
            let decode_result = match self.media.decode_into(&mut p.decode_block) {
                Ok(Some(result)) => result,
                Ok(None) => return Ok(DecodeStepResult::Pending),
                Err(e) => {
                    return Self::handle_decode_error(e);
                }
            };

            match decode_result {
                DecodeResult::Eof => {
                    info!("EOF from decode_into");
                    return Ok(DecodeStepResult::Eof);
                }
                DecodeResult::Decoded => {
                    let rate = p.decode_block.sample_rate();
                    if rate == p.target_rate {
                        if let Some(mut old) = self.resampler.take() {
                            info!("Source rate now matches device; dropping resampler");
                            self.timing
                                .finish_resampler()
                                .map_err(|error| EngineError::InvalidState(error.into()))?;
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
                                self.timing
                                    .finish_resampler()
                                    .map_err(|error| EngineError::InvalidState(error.into()))?;
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
                            p.ensure_resampler_output_capacity(resampler.output_frames_max());
                            self.timing_delay_pending = true;
                            self.resampler = Some(resampler);
                        }
                    }

                    p.source_rate = rate;
                }
            }
        }

        let input_offset = p.decode_offset;
        match &mut self.resampler {
            Some(resampler) => {
                let consumed = resampler.process_block(
                    &p.decode_block,
                    p.decode_offset,
                    &mut p.resampler_output,
                    p.decode_block.frames() - p.decode_offset,
                );
                p.decode_offset += consumed;
            }
            None => {
                let consumed = Resampler::passthrough_block(
                    &p.decode_block,
                    p.decode_offset,
                    &mut p.resampler_output,
                    p.decode_block.frames() - p.decode_offset,
                );
                p.decode_offset += consumed;
            }
        }

        let mut starts_track = std::mem::take(&mut self.starts_track).then_some(self.track_serial);
        if self.timing_delay_pending {
            self.timing_delay_pending = false;
            if let Some(resampler) = &self.resampler {
                let delay = resampler.output_delay();
                if delay > 0 {
                    self.timing
                        .delay(
                            delay,
                            p.decode_block.position_ms().unwrap_or(0) as f64,
                            starts_track.take(),
                        )
                        .map_err(|error| EngineError::InvalidState(error.into()))?;
                }
            }
        }
        self.timing
            .input(
                p.decode_offset - input_offset,
                p.source_rate,
                p.target_rate,
                p.decode_block
                    .position_ms()
                    .map(|ms| ms as f64 + input_offset as f64 * 1000.0 / p.source_rate as f64),
                input_offset == 0 && p.decode_block.discontinuity().is_some(),
                starts_track,
            )
            .map_err(|error| EngineError::InvalidState(error.into()))?;
        Self::route_resampler_output(p, &mut self.mixer, &mut self.eq, &mut self.tap).map_err(
            |e| EngineError::InvalidState(format!("device-stage handoff failed: {e:?}")),
        )?;

        Ok(DecodeStepResult::Continue)
    }

    fn route_resampler_output(
        p: &mut AudioPipeline,
        mixer: &mut Option<ChannelMixer>,
        eq: &mut EqualizerProcessor,
        tap: &mut SpectrumTap,
    ) -> Result<(), crate::media::pipeline::WriteError> {
        // priming or a format change can exceed the earlier bound, especially with an old
        // resampler tail still queued, so keep that audio when growing the ring
        let frames = p.resampler_output.first().map_or(0, Vec::len);
        p.ensure_device_input_capacity(p.device_input.potentially_available() + frames);
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

        if result.is_ok() {
            p.clear_resampler_output();
        }
        result
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
        tap.push_post(planes, tapped, eq.audible());
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
            if let Err(e) = Self::route_resampler_output(p, mixer, eq, tap) {
                error!("failed to hand the resampler tail to the device stage: {e:?}");
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
        let slices: smallvec::SmallVec<[&[f64]; crate::media::pipeline::MAX_AUDIO_CHANNELS]> =
            input.iter().map(|v| &v[..frames]).collect();

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
    Pending,
    Continue,
    Eof,
    FatalError(String),
    Rebuild(PipelineOverrides),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devices::channels::{ChannelLayout, ChannelPosition};
    use crate::devices::{
        builtin::dummy::{self, DummyDevice},
        errors::{FindError, InfoError, InitializationError, ListError, OpenError},
        format::SupportedFormat,
        traits::{Device, DeviceProvider, OutputStream},
    };
    use crate::playback::tests::harness::{
        configure_device_death, configure_dummy_device, engine_lock, engine_playing,
        i16_test_signal, run_to_eof, write_wav_i16,
    };
    use crate::settings::equalizer::{EqBandKind, EqBandSettings, EqualizerSettings};
    use std::{cell::Cell, rc::Rc};

    struct SwitchingOutput {
        available: Rc<Cell<bool>>,
        queries: Rc<Cell<usize>>,
        rate: Rc<Cell<u32>>,
    }

    impl DeviceProvider for SwitchingOutput {
        fn initialize(&mut self) -> Result<(), InitializationError> {
            Ok(())
        }
        fn get_devices(&mut self) -> Result<Vec<Box<dyn Device>>, ListError> {
            Ok(vec![])
        }
        fn get_device_by_uid(&mut self, _: &str) -> Result<Box<dyn Device>, FindError> {
            self.get_default_device()
        }
        fn get_default_device(&mut self) -> Result<Box<dyn Device>, FindError> {
            self.queries.set(self.queries.get() + 1);
            Ok(Box::new(SwitchingDevice {
                available: self.available.clone(),
                rate: self.rate.clone(),
            }))
        }
    }

    struct SwitchingDevice {
        available: Rc<Cell<bool>>,
        rate: Rc<Cell<u32>>,
    }

    impl Device for SwitchingDevice {
        fn get_default_format(&self) -> Result<FormatInfo, InfoError> {
            if !self.available.get() {
                return Err(InfoError::None);
            }
            let mut format = DummyDevice {}.get_default_format()?;
            format.sample_rate = self.rate.get();
            Ok(format)
        }
        fn open_device(&mut self, format: FormatInfo) -> Result<Box<dyn OutputStream>, OpenError> {
            DummyDevice {}.open_device(format)
        }
        fn get_supported_formats(&self) -> Result<Vec<SupportedFormat>, InfoError> {
            DummyDevice {}.get_supported_formats()
        }
        fn get_name(&self) -> Result<String, InfoError> {
            DummyDevice {}.get_name()
        }
        fn get_uid(&self) -> Result<String, InfoError> {
            DummyDevice {}.get_uid()
        }
        fn requires_matching_format(&self) -> bool {
            true
        }
    }

    enum RecoveryScenario {
        SameFormat,
        NewTrack,
        ChangedRate,
        Stop,
    }

    fn output_disappears(scenario: RecoveryScenario) {
        let change_rate = matches!(scenario, RecoveryScenario::ChangedRate);
        let replace_track = matches!(scenario, RecoveryScenario::NewTrack);
        let _guard = engine_lock();
        configure_dummy_device(44_100, "S16", 2);
        configure_device_death(2000);
        let dir = crate::test_support::TestDir::new("missing-output");
        let path = dir.join("source.wav");
        let samples = i16_test_signal(20_000, 2);
        write_wav_i16(&path, 44_100, 2, &samples);
        let capture = dummy::install_capture();
        let mut engine = engine_playing(&path);
        let available = Rc::new(Cell::new(false));
        let queries = Rc::new(Cell::new(0));
        let rate = Rc::new(Cell::new(44_100));
        engine.device.set_provider(Box::new(SwitchingOutput {
            available: available.clone(),
            queries: queries.clone(),
            rate: rate.clone(),
        }));
        let deadline = Instant::now() + Duration::from_secs(5);
        while engine.device.has_stream() {
            engine.process_cycle();
            assert!(Instant::now() < deadline, "device did not fault");
            std::thread::park_timeout(Duration::from_millis(1));
        }

        let accepted = capture.lock().unwrap()[0].len();
        let position = engine.position_ms().unwrap();
        assert!(position > 0);
        engine.device_retry_at = Some(Instant::now());
        engine.poll();
        assert!(!engine.device.has_stream());
        assert_eq!(queries.get(), 1);
        engine.device_retry_at = Some(Instant::now() + Duration::from_secs(60));
        for _ in 0..100 {
            assert_eq!(engine.process_cycle(), EngineCycleResult::Pending);
        }
        assert_eq!(
            queries.get(),
            1,
            "missing output was polled in a tight loop"
        );
        assert_eq!(engine.position_ms(), Some(position));
        assert_eq!(capture.lock().unwrap()[0].len(), accepted);

        if matches!(scenario, RecoveryScenario::Stop) {
            engine.stop();
            available.set(true);
            for _ in 0..100 {
                engine.poll();
            }
            assert_eq!(engine.state(), EngineState::Idle);
            assert!(engine.device_retry_at.is_none());
            assert!(!engine.device.has_stream());
            assert_eq!(queries.get(), 1);
            dummy::uninstall_capture();
            return;
        }

        if replace_track {
            engine.open(&path, false).unwrap();
            capture.lock().unwrap().iter_mut().for_each(Vec::clear);
            while engine.opening.is_some() {
                engine.poll();
                assert!(Instant::now() < deadline);
                std::thread::park_timeout(Duration::from_millis(1));
            }
        }
        engine.pause().unwrap();
        engine.set_volume(0.5).unwrap();
        available.set(true);
        if change_rate {
            rate.set(48_000);
        }
        engine.device_retry_at = Some(Instant::now());
        engine.poll();
        assert!(engine.device.has_stream());
        assert_eq!(engine.state(), EngineState::Paused);
        if change_rate {
            let mut reference = crate::playback::thread::decoder::Decoder::new();
            reference.open(&path).unwrap();
            reference
                .seek(
                    position as f64 / 1000.0,
                    &crate::media::traits::MediaSeekToken::new(),
                )
                .unwrap();
            let expected_position = reference.position_ms().unwrap();
            reference.close();
            while engine.media.is_seeking() {
                engine.poll();
                assert!(Instant::now() < deadline);
                std::thread::park_timeout(Duration::from_millis(1));
            }
            assert_eq!(engine.position_ms(), Some(expected_position));
        }
        let before_resume = capture.lock().unwrap()[0].len();
        for _ in 0..10 {
            assert_eq!(engine.process_cycle(), EngineCycleResult::NothingToDo);
        }
        assert_eq!(capture.lock().unwrap()[0].len(), before_resume);
        engine.set_volume(1.0).unwrap();
        engine.play().unwrap();
        run_to_eof(&mut engine, 100_000);
        if !change_rate {
            let captured = capture.lock().unwrap();
            for ch in 0..2 {
                let expected: Vec<f64> = samples
                    .iter()
                    .skip(ch)
                    .step_by(2)
                    .map(|&sample| sample as f64 / 32768.0)
                    .collect();
                assert_eq!(captured[ch], expected, "recovery dropped or duplicated PCM");
            }
        } else {
            assert_eq!(engine.pipeline.as_ref().unwrap().target_rate, 48_000);
        }
        engine.stop();
        dummy::uninstall_capture();
    }

    #[test]
    fn missing_default_output_retries_and_preserves_buffered_pcm() {
        output_disappears(RecoveryScenario::SameFormat);
    }

    #[test]
    fn opening_a_track_without_output_recovers_when_output_returns() {
        output_disappears(RecoveryScenario::NewTrack);
    }

    #[test]
    fn changed_output_rate_rebuilds_from_submitted_position() {
        output_disappears(RecoveryScenario::ChangedRate);
    }

    #[test]
    fn stopping_while_output_is_missing_cancels_retries() {
        output_disappears(RecoveryScenario::Stop);
    }

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
            ..Default::default()
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
        let mut p = AudioPipeline::new(2.into(), 2, 48_000, 48_000, 64).unwrap();
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
            AudioEngine::route_resampler_output(&mut p, &mut None, &mut eq, &mut tap).unwrap();
            assert_eq!(p.device_input.try_read_to_staging(64), 64);
            out_peak = peak(p.device_input.staging());
        }

        assert!(out_peak > in_peak * 10.0);
        assert!(out_peak < in_peak * 25.0);
    }

    #[test]
    fn route_passthrough_bypassed_eq_is_bit_exact() {
        let mut p = AudioPipeline::new(2.into(), 2, 48_000, 48_000, 64).unwrap();
        let mut eq = EqualizerProcessor::new(48_000.0, 2);
        eq.set_config(&config(EqBandKind::Bell, 1_000.0, 24.0, false));

        let dry = sine(1_000.0, 64, 48_000.0);
        let (mut tap, _consumer) = spectrum_tap();
        p.resampler_output = vec![dry.clone(), dry.clone()];
        AudioEngine::route_resampler_output(&mut p, &mut None, &mut eq, &mut tap).unwrap();

        assert_eq!(p.device_input.try_read_to_staging(64), 64);
        assert_eq!(p.device_input.staging()[0], dry);
    }

    #[test]
    fn route_mixer_path_applies_eq_after_mixing() {
        let mut p = AudioPipeline::new(1.into(), 2, 48_000, 48_000, 64).unwrap();
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
            AudioEngine::route_resampler_output(&mut p, &mut mixer, &mut eq, &mut tap).unwrap();
            assert_eq!(p.device_input.try_read_to_staging(64), 64);
            assert_eq!(p.device_input.staging().len(), 2);
            out_peak = peak(p.device_input.staging());
        }

        assert!(out_peak < 0.01);
    }
}
