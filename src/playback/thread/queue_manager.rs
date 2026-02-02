use std::{
    mem::take,
    path::PathBuf,
    sync::{Arc, RwLock},
};

use rand::{rng, seq::SliceRandom};

use crate::{
    playback::{events::RepeatState, queue::QueueItemData},
    settings::playback::PlaybackSettings,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reshuffled {
    Reshuffled,
    NotReshuffled,
}

#[derive(Debug, Clone)]
pub enum QueueNavigationResult {
    /// The queue position changed.
    Changed {
        index: usize,
        path: PathBuf,
        reshuffled: Reshuffled,
    },
    /// The current track should repeat (RepeatOne mode).
    Unchanged { path: PathBuf },
    /// End of queue reached.
    EndOfQueue,
}

#[derive(Debug, Clone)]
pub enum DequeueResult {
    /// An item was removed, queue position adjusted.
    Removed { new_position: usize },
    /// The currently playing item was removed.
    RemovedCurrent {
        /// The path of the next track to play, if any.
        new_path: Option<PathBuf>,
    },
    /// Nothing changed (index out of bounds).
    Unchanged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveResult {
    Moved,
    /// Item was moved and current position changed.
    MovedCurrent {
        new_position: usize,
    },
    /// Nothing changed (same position or invalid).
    Unchanged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertResult {
    /// Item(s) inserted, current position unchanged.
    Inserted { first_index: usize },
    /// Item(s) inserted and current position shifted.
    InsertedMovedCurrent {
        first_index: usize,
        new_position: usize,
    },
    /// Nothing changed (invalid position).
    Unchanged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShuffleResult {
    /// Shuffle was enabled.
    Shuffled,
    /// Shuffle was disabled, with the new position in the unshuffled queue.
    Unshuffled { new_position: usize },
}

#[derive(Debug, Clone)]
pub enum ReplaceResult {
    /// Queue replaced, contains the first item to play.
    Replaced { first_item: Option<QueueItemData> },
    /// Queue is empty after replacement.
    Empty,
}

#[derive(Debug, Clone)]
pub enum JumpResult {
    Jumped { path: PathBuf },
    OutOfBounds,
}

/// Manages the playback queue state.
///
/// This component handles all queue operations including navigation, shuffling,
/// repeat modes, and queue mutations. It does NOT handle side effects like
/// opening tracks or emitting events - those are the responsibility of the
/// PlaybackThread.
pub struct QueueManager {
    playback_settings: PlaybackSettings,
    /// The current queue. Shared with the UI thread for display.
    queue: Arc<RwLock<Vec<QueueItemData>>>,
    /// If shuffled, this holds the original (unshuffled) queue order.
    original_queue: Vec<QueueItemData>,
    /// Whether shuffle mode is enabled.
    shuffle: bool,
    /// Index of the next track to play.
    /// If queue_next == 1, we're on track 0.
    /// If queue_next == queue.len(), we're on the last track.
    queue_next: usize,
    repeat: RepeatState,
}

impl QueueManager {
    pub fn new(
        queue: Arc<RwLock<Vec<QueueItemData>>>,
        playback_settings: PlaybackSettings,
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
            queue_next: 0,
        }
    }

    /// Get the current queue position (0-indexed).
    /// Returns None if no track is playing.
    pub fn current_position(&self) -> Option<usize> {
        if self.queue_next > 0 {
            Some(self.queue_next - 1)
        } else {
            None
        }
    }

    /// Get the current repeat state.
    pub fn repeat_state(&self) -> RepeatState {
        self.repeat
    }

    /// Check if the queue is empty.
    pub fn is_empty(&self) -> bool {
        self.queue.read().expect("poisoned queue lock").is_empty()
    }

    /// Get the queue length.
    pub fn len(&self) -> usize {
        self.queue.read().expect("poisoned queue lock").len()
    }

    /// Get the first item in the queue.
    pub fn first(&self) -> Option<QueueItemData> {
        self.queue
            .read()
            .expect("poisoned queue lock")
            .first()
            .cloned()
    }

    /// Get the last item in the queue.
    pub fn last(&self) -> Option<QueueItemData> {
        self.queue
            .read()
            .expect("poisoned queue lock")
            .last()
            .cloned()
    }

    /// Set the queue position directly (used after opening a track).
    pub fn set_position(&mut self, index: usize) {
        self.queue_next = index + 1;
    }

    /// Set the repeat state.
    pub fn set_repeat(&mut self, state: RepeatState) {
        self.repeat = if state == RepeatState::NotRepeating && self.playback_settings.always_repeat
        {
            RepeatState::Repeating
        } else {
            state
        };
    }

    /// Update playback settings.
    pub fn update_settings(&mut self, settings: PlaybackSettings) {
        self.playback_settings = settings;

        if self.playback_settings.always_repeat && self.repeat == RepeatState::NotRepeating {
            self.repeat = RepeatState::Repeating;
        }
    }

    /// Advance to the next track in the queue.
    ///
    /// Returns information about what track to play next, or if playback should stop.
    pub fn next(&mut self, user_initiated: bool) -> QueueNavigationResult {
        let mut queue = self.queue.write().expect("poisoned queue lock");

        if self.repeat == RepeatState::RepeatingOne && !user_initiated {
            if let Some(path) = queue.get(self.queue_next.saturating_sub(1)) {
                return QueueNavigationResult::Unchanged {
                    path: path.get_path().clone(),
                };
            }
        }

        if self.queue_next < queue.len() {
            let index = self.queue_next;
            self.queue_next += 1;
            QueueNavigationResult::Changed {
                index,
                path: queue[index].get_path().clone(),
                reshuffled: Reshuffled::NotReshuffled,
            }
        } else if self.repeat == RepeatState::Repeating {
            if self.shuffle {
                queue.shuffle(&mut rng());
            }
            self.queue_next = 1;
            if queue.is_empty() {
                QueueNavigationResult::EndOfQueue
            } else {
                QueueNavigationResult::Changed {
                    index: 0,
                    path: queue[0].get_path().clone(),
                    reshuffled: if self.shuffle {
                        Reshuffled::Reshuffled
                    } else {
                        Reshuffled::NotReshuffled
                    },
                }
            }
        } else {
            QueueNavigationResult::EndOfQueue
        }
    }

    /// Go to the previous track in the queue.
    pub fn previous(&mut self) -> QueueNavigationResult {
        let mut queue = self.queue.write().expect("poisoned queue lock");

        if self.queue_next > 1 {
            self.queue_next -= 1;
            let index = self.queue_next - 1;
            QueueNavigationResult::Changed {
                index,
                path: queue[index].get_path().clone(),
                reshuffled: Reshuffled::NotReshuffled,
            }
        } else if self.queue_next == 1 && self.repeat == RepeatState::Repeating && !queue.is_empty()
        {
            if self.shuffle {
                queue.shuffle(&mut rng());
            }
            self.queue_next = queue.len();
            let index = self.queue_next - 1;
            QueueNavigationResult::Changed {
                index,
                path: queue[index].get_path().clone(),
                reshuffled: if self.shuffle {
                    Reshuffled::Reshuffled
                } else {
                    Reshuffled::NotReshuffled
                },
            }
        } else {
            QueueNavigationResult::EndOfQueue
        }
    }

    /// Jump to a specific index in the queue.
    pub fn jump(&mut self, index: usize) -> JumpResult {
        let queue = self.queue.read().expect("poisoned queue lock");

        if index < queue.len() {
            let path = queue[index].get_path().clone();
            drop(queue);
            self.queue_next = index + 1;
            JumpResult::Jumped { path }
        } else {
            JumpResult::OutOfBounds
        }
    }

    /// Jump to an index in the original (unshuffled) queue.
    /// If not shuffled, behaves like regular jump.
    pub fn jump_unshuffled(&mut self, index: usize) -> JumpResult {
        if !self.shuffle {
            return self.jump(index);
        }

        let original_item = match self.original_queue.get(index) {
            Some(item) => item.clone(),
            None => return JumpResult::OutOfBounds,
        };

        let queue = self.queue.read().expect("poisoned queue lock");
        let pos = queue
            .iter()
            .position(|item| item == &original_item);
        drop(queue);

        match pos {
            Some(shuffled_index) => self.jump(shuffled_index),
            None => JumpResult::OutOfBounds,
        }
    }

    /// Add a single item to the end of the queue.
    ///
    /// Returns the index where the item was added.
    pub fn queue_item(&mut self, item: QueueItemData) -> usize {
        let mut queue = self.queue.write().expect("poisoned queue lock");

        if self.shuffle {
            self.original_queue.push(item.clone());
        }

        queue.push(item);
        queue.len() - 1
    }

    /// Add multiple items to the end of the queue.
    ///
    /// If shuffle is enabled, the new items are shuffled before being added.
    /// Returns the index of the first item added.
    pub fn queue_items(&mut self, items: Vec<QueueItemData>) -> usize {
        if items.is_empty() {
            return self.len();
        }

        let mut queue = self.queue.write().expect("poisoned queue lock");
        let first_index = queue.len();

        if self.shuffle {
            self.original_queue.extend(items.clone());

            let mut shuffled = items;
            shuffled.shuffle(&mut rng());
            queue.extend(shuffled);
        } else {
            queue.extend(items);
        }

        first_index
    }

    /// Insert a single item at a specific position.
    pub fn insert_item(&mut self, position: usize, item: QueueItemData) -> InsertResult {
        let mut queue = self.queue.write().expect("poisoned queue lock");

        let insert_pos = position.min(queue.len());

        if self.shuffle {
            self.original_queue.push(item.clone());
        }

        queue.insert(insert_pos, item);

        if insert_pos < self.queue_next {
            self.queue_next += 1;
            InsertResult::InsertedMovedCurrent {
                first_index: insert_pos,
                new_position: self.queue_next - 1,
            }
        } else {
            InsertResult::Inserted {
                first_index: insert_pos,
            }
        }
    }

    /// Insert multiple items at a specific position.
    pub fn insert_items(&mut self, position: usize, items: Vec<QueueItemData>) -> InsertResult {
        if items.is_empty() {
            return InsertResult::Unchanged;
        }

        let mut queue = self.queue.write().expect("poisoned queue lock");

        let insert_pos = position.min(queue.len());
        let items_len = items.len();

        if self.shuffle {
            self.original_queue.extend(items.clone());
        }

        queue.splice(insert_pos..insert_pos, items);

        if insert_pos < self.queue_next {
            self.queue_next += items_len;
            InsertResult::InsertedMovedCurrent {
                first_index: insert_pos,
                new_position: self.queue_next - 1,
            }
        } else {
            InsertResult::Inserted {
                first_index: insert_pos,
            }
        }
    }

    /// Remove an item from the queue at the specified index.
    pub fn dequeue(&mut self, index: usize) -> DequeueResult {
        let mut queue = self.queue.write().expect("poisoned queue lock");

        if index >= queue.len() {
            return DequeueResult::Unchanged;
        }

        let removed = queue.remove(index);

        if self.shuffle {
            self.original_queue
                .iter()
                .position(|item| item == &removed)
                .map(|pos| self.original_queue.remove(pos));
        }

        let current = self.queue_next.saturating_sub(1);

        if index == current {
            let new_path = queue.get(current).map(|v| v.get_path().clone());
            DequeueResult::RemovedCurrent { new_path }
        } else if index < current {
            self.queue_next -= 1;
            DequeueResult::Removed {
                new_position: self.queue_next - 1,
            }
        } else {
            DequeueResult::Removed {
                new_position: current,
            }
        }
    }

    /// Move an item from one position to another.
    pub fn move_item(&mut self, from: usize, to: usize) -> MoveResult {
        if from == to {
            return MoveResult::Unchanged;
        }

        let mut queue = self.queue.write().expect("poisoned queue lock");

        if from >= queue.len() || to >= queue.len() {
            return MoveResult::Unchanged;
        }

        let item = queue.remove(from);
        queue.insert(to, item);

        let current = self.queue_next.saturating_sub(1);

        if from == current {
            // Moved the current track
            self.queue_next = to + 1;
            MoveResult::MovedCurrent { new_position: to }
        } else if from < current && to >= current {
            // Moved from before to after current
            self.queue_next -= 1;
            MoveResult::MovedCurrent {
                new_position: self.queue_next - 1,
            }
        } else if from > current && to <= current {
            // Moved from after to before current
            self.queue_next += 1;
            MoveResult::MovedCurrent {
                new_position: self.queue_next - 1,
            }
        } else {
            MoveResult::Moved
        }
    }

    /// Replace the entire queue with new items.
    ///
    /// If shuffle is enabled, the items are shuffled (but original order is preserved).
    pub fn replace_queue(&mut self, items: Vec<QueueItemData>) -> ReplaceResult {
        let mut queue = self.queue.write().expect("poisoned queue lock");

        if self.shuffle {
            let mut shuffled = items.clone();
            shuffled.shuffle(&mut rng());

            self.original_queue = items;
            *queue = shuffled;
        } else {
            self.original_queue.clear();
            *queue = items;
        }

        self.queue_next = 0;

        match queue.first().cloned() {
            Some(first) => ReplaceResult::Replaced {
                first_item: Some(first),
            },
            None => ReplaceResult::Empty,
        }
    }

    /// Clear the queue.
    pub fn clear(&mut self) {
        let mut queue = self.queue.write().expect("poisoned queue lock");
        queue.clear();
        self.original_queue.clear();
        self.queue_next = 0;
    }

    /// Toggle shuffle mode.
    pub fn toggle_shuffle(&mut self) -> ShuffleResult {
        let mut queue = self.queue.write().expect("poisoned queue lock");

        self.shuffle = !self.shuffle;

        if self.shuffle {
            self.original_queue = queue.clone();

            let start = self.queue_next.min(queue.len());
            if start < queue.len() {
                queue[start..].shuffle(&mut rng());
            }

            ShuffleResult::Shuffled
        } else {
            let current_item = if self.queue_next > 0 && self.queue_next <= queue.len() {
                Some(queue[self.queue_next - 1].clone())
            } else {
                None
            };

            let new_position = current_item
                .and_then(|target_item| {
                    self.original_queue
                        .iter()
                        .position(|item| item == &target_item)
                })
                .unwrap_or(0);

            *queue = take(&mut self.original_queue);
            self.queue_next = new_position + 1;

            ShuffleResult::Unshuffled { new_position }
        }
    }
}
