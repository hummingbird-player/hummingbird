//! Wake the playback thread when commands arrive or the last sender disconnects.

use std::sync::{Arc, OnceLock};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender, error::SendError};

use super::events::PlaybackCommand;

#[derive(Clone)]
pub struct CommandSender(
    Option<UnboundedSender<PlaybackCommand>>,
    Arc<OnceLock<std::thread::Thread>>,
);

impl CommandSender {
    pub fn send(&self, command: PlaybackCommand) -> Result<(), SendError<PlaybackCommand>> {
        let result = self.0.as_ref().unwrap().send(command);
        if let Some(thread) = self.1.get() {
            thread.unpark();
        }
        result
    }

    pub fn bind_current_thread(&self) {
        let _ = self.1.set(std::thread::current());
    }
}

impl Drop for CommandSender {
    fn drop(&mut self) {
        self.0.take();
        if let Some(thread) = self.1.get() {
            thread.unpark();
        }
    }
}

pub fn channel() -> (CommandSender, UnboundedReceiver<PlaybackCommand>) {
    let (tx, rx) = mpsc::unbounded_channel();
    (CommandSender(Some(tx), Arc::new(OnceLock::new())), rx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dropping_the_last_sender_wakes_a_parked_receiver() {
        let (tx, rx) = channel();
        let bind = tx.clone();
        let (ready_tx, ready) = std::sync::mpsc::channel();
        let (done_tx, done) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            bind.bind_current_thread();
            drop(bind);
            ready_tx.send(()).unwrap();
            while !rx.is_closed() {
                std::thread::park();
            }
            done_tx.send(()).unwrap();
        });
        ready
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        drop(tx);
        done.recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn cloned_senders_preserve_order_and_return_unsent_commands() {
        let (tx, mut rx) = channel();
        tx.send(PlaybackCommand::Pause).unwrap();
        tx.clone().send(PlaybackCommand::Play).unwrap();
        assert_eq!(rx.try_recv().unwrap(), PlaybackCommand::Pause);
        assert_eq!(rx.try_recv().unwrap(), PlaybackCommand::Play);
        drop(rx);
        assert_eq!(
            tx.send(PlaybackCommand::Stop).unwrap_err().0,
            PlaybackCommand::Stop
        );
    }
}
