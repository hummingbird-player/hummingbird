use objc2_avf_audio::{AVAudioSession, AVAudioSessionCategoryPlayback};
use tracing::warn;

/// Puts the app's audio session in the playback category.
///
/// Without this the app uses the default (solo ambient) category, which iOS silences when the
/// ring/silent switch is set to silent and which stops when the screen locks.
pub fn configure_audio_session() {
    // SAFETY: the shared session is a process-wide singleton and these calls have no
    // preconditions beyond being made from a thread with an autorelease pool, which objc2 provides
    unsafe {
        let session = AVAudioSession::sharedInstance();
        let Some(category) = AVAudioSessionCategoryPlayback else {
            warn!(
                "AVAudioSessionCategoryPlayback is unavailable; audio may follow the silent switch"
            );
            return;
        };
        if let Err(error) = session.setCategory_error(category) {
            warn!("Couldn't set the audio session category: {error:?}");
        }
        if let Err(error) = session.setActive_error(true) {
            warn!("Couldn't activate the audio session: {error:?}");
        }
    }
}
