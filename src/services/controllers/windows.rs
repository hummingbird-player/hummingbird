use std::{ffi::c_void, path::Path, time::Duration};

use async_trait::async_trait;
use raw_window_handle::RawWindowHandle;
use windows::{
    Foundation::TypedEventHandler,
    Media::{
        AutoRepeatModeChangeRequestedEventArgs, MediaPlaybackAutoRepeatMode, MediaPlaybackStatus,
        MediaPlaybackType, PlaybackPositionChangeRequestedEventArgs,
        ShuffleEnabledChangeRequestedEventArgs, SystemMediaTransportControls,
        SystemMediaTransportControlsButton, SystemMediaTransportControlsButtonPressedEventArgs,
        SystemMediaTransportControlsDisplayUpdater, SystemMediaTransportControlsTimelineProperties,
    },
    Storage::Streams::{DataWriter, InMemoryRandomAccessStream, RandomAccessStreamReference},
    Win32::{Foundation::HWND, System::WinRT::ISystemMediaTransportControlsInterop},
    core::HSTRING,
};

use crate::{
    media::metadata::Metadata,
    playback::{events::RepeatState, thread::PlaybackState},
    services::controllers::InitPlaybackController,
};

use super::{ControllerBridge, PlaybackController};

pub struct WindowsController {
    controls: SystemMediaTransportControls,
    display: SystemMediaTransportControlsDisplayUpdater,
    timeline: SystemMediaTransportControlsTimelineProperties,
    bridge: ControllerBridge,
}

impl WindowsController {
    pub fn connect_events(&mut self) -> anyhow::Result<()> {
        self.controls.SetIsEnabled(true)?;
        self.controls.SetIsNextEnabled(true)?;
        self.controls.SetIsPreviousEnabled(true)?;
        self.controls.SetIsPlayEnabled(true)?;
        self.controls.SetIsPauseEnabled(true)?;
        self.controls.SetIsStopEnabled(true)?;

        let bridge = self.bridge.clone();
        self.controls.ButtonPressed(&TypedEventHandler::<
            SystemMediaTransportControls,
            SystemMediaTransportControlsButtonPressedEventArgs,
        >::new(move |_, args| {
            let event = args.as_ref().unwrap().Button().unwrap();

            match event {
                SystemMediaTransportControlsButton::Play => bridge.play(),
                SystemMediaTransportControlsButton::Pause => bridge.pause(),
                SystemMediaTransportControlsButton::Next => bridge.next(),
                SystemMediaTransportControlsButton::Previous => bridge.previous(),
                SystemMediaTransportControlsButton::Stop => bridge.stop(),
                _ => (),
            }

            Ok(())
        }))?;

        let bridge = self.bridge.clone();
        self.controls
            .PlaybackPositionChangeRequested(&TypedEventHandler::<
                SystemMediaTransportControls,
                PlaybackPositionChangeRequestedEventArgs,
            >::new(move |_, args| {
                let position = args.as_ref().unwrap().RequestedPlaybackPosition().unwrap();

                // TimeSpan is measured in 100ns intervals
                bridge.seek(position.Duration as f64 / 10_000_000.0);

                Ok(())
            }))?;

        let bridge = self.bridge.clone();
        self.controls
            .ShuffleEnabledChangeRequested(&TypedEventHandler::<
                SystemMediaTransportControls,
                ShuffleEnabledChangeRequestedEventArgs,
            >::new(move |_, _| {
                // TODO: do better than this
                bridge.toggle_shuffle();

                Ok(())
            }))?;

        let bridge = self.bridge.clone();
        self.controls
            .AutoRepeatModeChangeRequested(&TypedEventHandler::<
                SystemMediaTransportControls,
                AutoRepeatModeChangeRequestedEventArgs,
            >::new(move |_, args| {
                let mode = args.as_ref().unwrap().RequestedAutoRepeatMode().unwrap();

                bridge.set_repeat(match mode {
                    MediaPlaybackAutoRepeatMode::List => RepeatState::Repeating,
                    MediaPlaybackAutoRepeatMode::Track => RepeatState::RepeatingOne,
                    _ => RepeatState::NotRepeating,
                });

                Ok(())
            }))?;

        Ok(())
    }
}

impl InitPlaybackController for WindowsController {
    fn init(
        bridge: ControllerBridge,
        handle: Option<RawWindowHandle>,
    ) -> anyhow::Result<Box<dyn PlaybackController>> {
        let interop: ISystemMediaTransportControlsInterop = windows::core::factory::<
            SystemMediaTransportControls,
            ISystemMediaTransportControlsInterop,
        >()?;

        let hwnd = match handle {
            Some(RawWindowHandle::Win32(handle)) => handle,
            _ => panic!("non-Win32 window handle/invalid window handle during creation of SMTC"),
        };

        let controls: SystemMediaTransportControls = unsafe {
            let pointer = hwnd.hwnd.get() as *mut c_void;
            interop.GetForWindow(HWND(pointer)).unwrap()
        };

        let display = controls.DisplayUpdater()?;
        let timeline = SystemMediaTransportControlsTimelineProperties::new()?;

        let mut controller = WindowsController {
            controls,
            display,
            timeline,
            bridge,
        };

        controller.connect_events()?;

        Ok(Box::new(controller))
    }
}

#[async_trait]
impl PlaybackController for WindowsController {
    async fn position_changed(&mut self, new_position: u64) -> anyhow::Result<()> {
        self.timeline
            .SetPosition(Duration::from_secs(new_position).into())?;
        self.controls.UpdateTimelineProperties(&self.timeline)?;

        Ok(())
    }
    async fn duration_changed(&mut self, new_duration: u64) -> anyhow::Result<()> {
        self.timeline.SetStartTime(Duration::from_secs(0).into())?;
        self.timeline
            .SetMinSeekTime(Duration::from_secs(0).into())?;
        self.timeline
            .SetMaxSeekTime(Duration::from_secs(new_duration).into())?;
        self.timeline
            .SetEndTime(Duration::from_secs(new_duration).into())?;
        self.timeline.SetPosition(Duration::from_secs(0).into())?;
        self.controls.UpdateTimelineProperties(&self.timeline)?;

        Ok(())
    }

    async fn volume_changed(&mut self, _new_volume: f64) -> anyhow::Result<()> {
        Ok(())
    }

    async fn metadata_changed(&mut self, metadata: &Metadata) -> anyhow::Result<()> {
        if let Some(title) = metadata.name.clone() {
            let string = HSTRING::from(title);
            self.display.MusicProperties().unwrap().SetTitle(&string)?;
        }

        if let Some(artist) = metadata.artist.clone() {
            let string = HSTRING::from(artist);
            self.display.MusicProperties().unwrap().SetArtist(&string)?;
        }

        if let Some(album) = metadata.album.clone() {
            let string = HSTRING::from(album);
            self.display
                .MusicProperties()
                .unwrap()
                .SetAlbumTitle(&string)?;
        }

        if let Some(track_number) = metadata.track_current {
            self.display
                .MusicProperties()
                .unwrap()
                .SetTrackNumber(track_number as u32)?;
        }

        if let Some(track_max) = metadata.track_max {
            self.display
                .MusicProperties()
                .unwrap()
                .SetAlbumTrackCount(track_max as u32)?;
        }

        self.display.Update()?;

        Ok(())
    }

    async fn album_art_changed(&mut self, album_art: &[u8]) -> anyhow::Result<()> {
        let stream = InMemoryRandomAccessStream::new().expect("could not create RAS");
        let writer = DataWriter::CreateDataWriter(&stream).unwrap();

        writer.WriteBytes(album_art)?;

        writer
            .StoreAsync()
            .expect("could not start store operation")
            .await?;

        writer.DetachStream()?;
        let reference = RandomAccessStreamReference::CreateFromStream(&stream)?;

        self.display.SetThumbnail(&reference)?;
        self.display.Update()?;

        Ok(())
    }

    async fn repeat_state_changed(&mut self, repeat_state: RepeatState) -> anyhow::Result<()> {
        self.controls.SetAutoRepeatMode(match repeat_state {
            RepeatState::NotRepeating => MediaPlaybackAutoRepeatMode::None,
            RepeatState::Repeating => MediaPlaybackAutoRepeatMode::List,
            RepeatState::RepeatingOne => MediaPlaybackAutoRepeatMode::Track,
        })?;

        Ok(())
    }

    async fn playback_state_changed(
        &mut self,
        playback_state: PlaybackState,
    ) -> anyhow::Result<()> {
        let playback_state = match playback_state {
            PlaybackState::Stopped => MediaPlaybackStatus::Stopped,
            PlaybackState::Playing => MediaPlaybackStatus::Playing,
            PlaybackState::Paused => MediaPlaybackStatus::Paused,
        };

        self.controls.SetPlaybackStatus(playback_state)?;

        Ok(())
    }
    async fn shuffle_state_changed(&mut self, shuffling: bool) -> anyhow::Result<()> {
        self.controls.SetShuffleEnabled(shuffling)?;

        Ok(())
    }
    async fn new_file(&mut self, path: &Path) -> anyhow::Result<()> {
        self.display.ClearAll()?;
        self.display.SetType(MediaPlaybackType::Music)?;
        let title_string = HSTRING::from(path.file_name().unwrap().to_str().unwrap());
        self.display
            .MusicProperties()
            .unwrap()
            .SetTitle(&title_string)?;
        self.display.Update()?;

        Ok(())
    }
}
