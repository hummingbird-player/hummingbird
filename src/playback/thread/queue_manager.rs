use std::{
    mem::take,
    path::PathBuf,
    rc::Rc,
    sync::{Arc, RwLock},
};

use rand::{rng, seq::SliceRandom};

use crate::{
    playback::{events::RepeatState, queue::QueueItemData},
    settings::playback::PlaybackSettings,
};

pub enum Reshuffled {
    Reshuffled,
    NotReshuffled,
}

pub enum QueueNextResult {
    Changed(usize, PathBuf, Reshuffled),
    Unchanged,
    EndOfQueue,
}

pub enum DequeueResult {
    Removed {
        index: usize,
    },
    /// The current item was removed. If the current item was removed, the next item in the queue
    /// should be played, if there is one.
    RemovedCurrent {
        new_path: Option<PathBuf>,
    },
    Unchanged,
}

pub enum MoveResult {
    Moved,
    /// This is returned whenever the current track is moved, whether directly or indirectly.
    MovedCurrent {
        current_to: usize,
    },
    Unchanged,
}

pub enum InsertResult {
    Inserted,
    InsertMovedCurrent { current_to: usize },
    Unchanged,
}

pub enum ShuffleResult {
    Shuffled,
    Unshuffled(usize),
}

pub struct QueueManager {
    playback_settings: Rc<PlaybackSettings>,
    /// The current queue. Do not hold an indefinite lock on this queue - it is read by the
    /// UI thread.
    // This lock will be said to be poisoned even if it is not poisoned because no other code
    // should hold a lock on it.
    queue: Arc<RwLock<Vec<QueueItemData>>>,
    /// If the queue is shuffled, this is a copy of the original (unshuffled) queue.
    original_queue: Vec<QueueItemData>,
    shuffle: bool,
    /// The index after the current item in the queue. This can be out of bounds if the current
    /// track is the last track in the queue.
    queue_next: usize,
    /// Whether or not the queue should be repeated when the end of the queue is reached.
    repeat: RepeatState,
}

impl QueueManager {
    pub fn new(
        queue: Arc<RwLock<Vec<QueueItemData>>>,
        playback_settings: Rc<PlaybackSettings>,
    ) -> Self {
        Self {
            repeat: if playback_settings.always_repeat {
                RepeatState::Repeating
            } else {
                RepeatState::NotRepeating
            },
            playback_settings,
            queue,
            original_queue: Vec::new(),
            shuffle: false,
            queue_next: 1,
        }
    }

    /// Get the next track in the queue and advance the queue index.
    ///
    /// Returns `Changed` when the queue is advanced, `Unchanged` if the current track repeats, and
    /// `EndOfQueue` if the end of the queue is reached. If `Unchanged` is returned, the current
    /// track should be played again.
    pub fn next(&mut self, user_initiated: bool) -> QueueNextResult {
        let mut queue = self.queue.write().expect("poisoned queue lock");

        if self.repeat == RepeatState::RepeatingOne && !user_initiated {
            return QueueNextResult::Unchanged;
        }

        if self.queue_next >= queue.len() {
            if self.repeat == RepeatState::Repeating {
                queue.shuffle(&mut rng());
                self.queue_next = 1;
                QueueNextResult::Changed(0, queue[0].get_path().clone(), Reshuffled::Reshuffled)
            } else {
                QueueNextResult::EndOfQueue
            }
        } else {
            self.queue_next += 1;
            QueueNextResult::Changed(
                self.queue_next - 1,
                queue[self.queue_next - 1].get_path().clone(),
                Reshuffled::NotReshuffled,
            )
        }
    }

    /// Get the previous track in the queue and rewind the queue index.
    ///
    /// Returns `Changed` when the queue is rewinded, and `EndOfQueue` when the beginning of the
    /// queue is reached.
    pub fn previous(&mut self) -> QueueNextResult {
        let mut queue = self.queue.write().expect("poisoned queue lock");

        if self.queue_next == 0 {
            if self.repeat == RepeatState::Repeating {
                queue.shuffle(&mut rng());
                self.queue_next = queue.len() - 1;
                QueueNextResult::Changed(
                    self.queue_next - 1,
                    queue[self.queue_next - 1].get_path().clone(),
                    Reshuffled::Reshuffled,
                )
            } else {
                QueueNextResult::EndOfQueue
            }
        } else {
            self.queue_next -= 1;
            QueueNextResult::Changed(
                self.queue_next - 1,
                queue[self.queue_next - 1].get_path().clone(),
                Reshuffled::NotReshuffled,
            )
        }
    }

    /// Add a new queue item to the queue.
    ///
    /// Returns the index of the newly added item.
    pub fn queue(&mut self, item: QueueItemData) -> usize {
        let mut queue = self.queue.write().expect("poisoned queue lock");

        if self.shuffle {
            self.original_queue.push(item.clone());
        }

        queue.push(item);
        queue.len() - 1
    }

    /// Add a list of new queue items to the queue.
    ///
    /// Returns the index of the first queue item added.
    pub fn queue_items(&mut self, items: Vec<QueueItemData>) -> usize {
        let mut queue = self.queue.write().expect("poisoned queue lock");

        let pre_len = queue.len();

        if self.shuffle {
            self.original_queue.extend(items.clone());
        }

        queue.extend(items);
        pre_len
    }

    /// Remove an item from the queue at the specified index.
    ///
    /// Returns `RemovedCurrent` if the current item was removed, `Removed` (with the new queue
    /// position) if an item before the current item was removed, or `Unchanged` if no item was
    /// removed. If the current item was removed, the next item is returned as the new current
    /// item.
    pub fn dequeue(&mut self, index: usize) -> DequeueResult {
        let mut queue = self.queue.write().expect("poisoned queue lock");

        let removed = queue.remove(index);

        if self.shuffle {
            self.original_queue.retain(|item| removed != *item);
        }

        if index == self.queue_next - 1 {
            DequeueResult::RemovedCurrent {
                new_path: queue.get(self.queue_next - 1).map(|v| v.get_path().clone()),
            }
        } else if index < self.queue_next - 1 {
            self.queue_next -= 1;
            DequeueResult::Removed {
                index: self.queue_next - 1,
            }
        } else {
            DequeueResult::Unchanged
        }
    }

    /// Moves an item from one position to another within the queue.
    ///
    /// Returns `MovedCurrent` if the current queue position changed, `Moved` if the item was moved,
    /// and `Unchanged` if the target and destination are the same.
    pub fn move_item(&mut self, from: usize, to: usize) -> MoveResult {
        let mut queue = self.queue.write().expect("poisoned queue lock");

        if from == to {
            return MoveResult::Unchanged;
        }

        let removed = queue.remove(from);
        queue.insert(to, removed);

        let current_playing = if self.queue_next > 0 {
            self.queue_next - 1
        } else {
            0
        };

        if from == current_playing {
            self.queue_next += 1;
            MoveResult::MovedCurrent {
                current_to: self.queue_next - 1,
            }
        } else if from < current_playing && to >= current_playing {
            self.queue_next += 1;
            MoveResult::MovedCurrent {
                current_to: self.queue_next - 1,
            }
        } else if from >= current_playing && to < current_playing {
            self.queue_next -= 1;
            MoveResult::MovedCurrent {
                current_to: self.queue_next - 1,
            }
        } else {
            MoveResult::Moved
        }
    }

    /// Insert a queue item at a specified position in the queue.
    ///
    /// Returns `Inserted` if the item was inserted, `InsertMovedCurrent` if the item was inserted
    /// and moved the current position, and `Unchanged` if the position is invalid.
    pub fn insert_item(&mut self, index: usize, item: QueueItemData) -> InsertResult {
        let mut queue = self.queue.write().expect("poisoned queue lock");

        if index > queue.len() {
            return InsertResult::Unchanged;
        }

        if self.shuffle {
            self.original_queue.push(item.clone());
        }

        queue.insert(index, item);

        if index <= self.queue_next {
            self.queue_next += 1;
            InsertResult::InsertMovedCurrent {
                current_to: self.queue_next - 1,
            }
        } else {
            InsertResult::Inserted
        }
    }

    /// Insert a list of queue items at the specified position in the queue.
    ///
    /// Returns `Inserted` if the items were inserted, `InsertMovedCurrent` if the items were inserted
    /// and moved the current position, and `Unchanged` if the position is invalid.
    pub fn insert_items(&mut self, index: usize, items: Vec<QueueItemData>) -> InsertResult {
        let mut queue = self.queue.write().expect("poisoned queue lock");

        let items_len = items.len();

        if index > queue.len() {
            return InsertResult::Unchanged;
        }

        queue.splice(index..index, items);

        if index <= self.queue_next {
            self.queue_next += items_len;
            InsertResult::InsertMovedCurrent {
                current_to: self.queue_next - 1,
            }
        } else {
            InsertResult::Inserted
        }
    }

    /// Replace the current queue with the given items.
    ///
    /// Returns the path to the first item in the new queue, if any. This file should be played
    /// immediately.
    pub fn replace_queue(&mut self, items: Vec<QueueItemData>) -> Option<QueueItemData> {
        let mut queue = self.queue.write().expect("poisoned queue lock");

        if self.shuffle {
            let mut shuffled = items.clone();
            shuffled.shuffle(&mut rng());

            self.original_queue = items;

            queue.clear();
            queue.extend(shuffled);
        } else {
            *queue = items;
        }

        self.queue_next = 0;

        queue.first().cloned()
    }

    /// Clear the current queue.
    pub fn clear_queue(&mut self) {
        let mut queue = self.queue.write().expect("poisoned queue lock");

        queue.clear();
        self.queue_next = 0;
    }

    /// Toggle shuffle mode.
    ///
    /// Returns the shuffle state and the new current track number, if it changed.
    pub fn toggle_shuffle(&mut self) -> ShuffleResult {
        let mut queue = self.queue.write().expect("poisoned queue lock");

        self.shuffle = !self.shuffle;

        if self.shuffle {
            self.original_queue = queue.clone();
            let length = queue.len();
            queue[self.queue_next..length].shuffle(&mut rng());
            ShuffleResult::Shuffled
        } else {
            // find current track in the shuffled queue and turn it back into the original position
            let current_track = &queue[self.queue_next - 1];
            self.queue_next = self
                .original_queue
                .iter()
                .position(|item| item.get_path() == current_track.get_path())
                .unwrap()
                + 1;

            *queue = take(&mut self.original_queue);

            ShuffleResult::Unshuffled(self.queue_next - 1)
        }
    }
}
