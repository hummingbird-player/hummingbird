use std::{
    mem::take,
    path::PathBuf,
    sync::{Arc, RwLock},
};

use rand::{rng, seq::SliceRandom};

use crate::{
    playback::{events::RepeatState, queue::QueueItemData, session_storage::PlaybackSessionData},
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
    storage_tx: tokio::sync::watch::Sender<PlaybackSessionData>,
}

impl QueueManager {
    fn normalize_repeat_state(
        playback_settings: &PlaybackSettings,
        state: RepeatState,
    ) -> RepeatState {
        if state == RepeatState::NotRepeating && playback_settings.always_repeat {
            RepeatState::Repeating
        } else {
            state
        }
    }

    fn item_is_playable(item: &QueueItemData) -> bool {
        item.get_path().exists()
    }

    fn first_playable_index(queue: &[QueueItemData]) -> Option<usize> {
        queue.iter().position(Self::item_is_playable)
    }

    fn last_playable_index(queue: &[QueueItemData]) -> Option<usize> {
        queue.iter().rposition(Self::item_is_playable)
    }

    fn next_playable_from(queue: &[QueueItemData], start: usize) -> Option<usize> {
        (start..queue.len()).find(|idx| Self::item_is_playable(&queue[*idx]))
    }

    fn prev_playable_before(queue: &[QueueItemData], end_exclusive: usize) -> Option<usize> {
        (0..end_exclusive)
            .rev()
            .find(|idx| Self::item_is_playable(&queue[*idx]))
    }

    pub fn new(
        queue: Arc<RwLock<Vec<QueueItemData>>>,
        playback_settings: PlaybackSettings,
        session: PlaybackSessionData,
        storage_tx: tokio::sync::watch::Sender<PlaybackSessionData>,
    ) -> Self {
        let PlaybackSessionData {
            original_queue: session_original_queue,
            queue_position: session_queue_position,
            shuffle,
            repeat,
            ..
        } = session;
        let (queue_len, original_queue) = {
            let queue = queue.read().expect("poisoned queue lock");
            let queue_len = queue.len();
            let original_queue = if shuffle && session_original_queue.len() == queue_len {
                session_original_queue
            } else if shuffle {
                queue.clone()
            } else {
                Vec::new()
            };

            (queue_len, original_queue)
        };
        let queue_position = session_queue_position.filter(|position| *position < queue_len);

        Self {
            repeat: Self::normalize_repeat_state(&playback_settings, repeat),
            playback_settings,
            queue,
            original_queue,
            shuffle,
            queue_next: queue_position.map_or(0, |position| position + 1),
            storage_tx,
        }
    }

    /// Get the current queue position (0-indexed).
    /// Returns None if no track is playing.
    pub fn current_position(&self) -> Option<usize> {
        let position = self.queue_next.checked_sub(1)?;
        (position < self.len()).then_some(position)
    }

    /// Get the current repeat state.
    pub fn repeat_state(&self) -> RepeatState {
        self.repeat
    }

    /// Get the queue length.
    pub fn len(&self) -> usize {
        self.queue.read().expect("poisoned queue lock").len()
    }

    /// Returns true when shuffle mode is enabled.
    pub fn is_shuffle_enabled(&self) -> bool {
        self.shuffle
    }

    /// Returns true if every queued item belongs to the same known album.
    pub fn all_items_same_album(&self) -> bool {
        let queue = self.queue.read().expect("poisoned queue lock");
        let Some(first_album) = queue.first().and_then(QueueItemData::get_db_album_id) else {
            return false;
        };

        queue
            .iter()
            .all(|item| item.get_db_album_id() == Some(first_album))
    }

    /// Get the first playable item in the queue along with its index.
    pub fn first_with_index(&self) -> Option<(QueueItemData, usize)> {
        self.queue
            .read()
            .expect("poisoned queue lock")
            .iter()
            .enumerate()
            .find(|(_, item)| Self::item_is_playable(item))
            .map(|(idx, item)| (item.clone(), idx))
    }

    /// Get the last item in the queue along with its index, if the queue is non-empty.
    pub fn last_with_index(&self) -> Option<(QueueItemData, usize)> {
        let queue = self.queue.read().expect("poisoned queue lock");
        Self::last_playable_index(&queue).map(|index| (queue[index].clone(), index))
    }

    /// Set the queue position directly (used after opening a track).
    pub fn set_position(&mut self, index: usize) {
        self.queue_next = index + 1;
        self.persist_session_state();
    }

    /// Set the repeat state.
    pub fn set_repeat(&mut self, state: RepeatState) {
        self.repeat = Self::normalize_repeat_state(&self.playback_settings, state);
        self.persist_session_state();
    }

    /// Update playback settings.
    pub fn update_settings(&mut self, settings: PlaybackSettings) {
        self.playback_settings = settings;

        if self.playback_settings.always_repeat && self.repeat == RepeatState::NotRepeating {
            self.repeat = RepeatState::Repeating;
            self.persist_session_state();
        }
    }

    /// Advance to the next track in the queue.
    ///
    /// Returns information about what track to play next, or if playback should stop.
    pub fn next(&mut self, user_initiated: bool) -> QueueNavigationResult {
        let result = {
            let mut queue = self.queue.write().expect("poisoned queue lock");

            if self.repeat == RepeatState::RepeatingOne
                && !user_initiated
                && let Some(path) = queue.get(self.queue_next.saturating_sub(1))
                && Self::item_is_playable(path)
            {
                return QueueNavigationResult::Unchanged {
                    path: path.get_path().clone(),
                };
            }

            if let Some(index) = Self::next_playable_from(&queue, self.queue_next) {
                self.queue_next = index + 1;
                QueueNavigationResult::Changed {
                    index,
                    path: queue[index].get_path().clone(),
                    reshuffled: Reshuffled::NotReshuffled,
                }
            } else if self.repeat == RepeatState::Repeating {
                if self.shuffle {
                    queue.shuffle(&mut rng());
                }
                if let Some(index) = Self::first_playable_index(&queue) {
                    self.queue_next = index + 1;
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
            } else {
                QueueNavigationResult::EndOfQueue
            }
        };

        if let QueueNavigationResult::Changed { reshuffled, .. } = &result {
            if *reshuffled == Reshuffled::Reshuffled {
                self.persist_session_with_queue();
            } else {
                self.persist_session_state();
            }
        }

        result
    }

    /// Go to the previous track in the queue.
    pub fn previous(&mut self) -> QueueNavigationResult {
        let result = {
            let mut queue = self.queue.write().expect("poisoned queue lock");

            if self.queue_next > 1
                && let Some(index) = Self::prev_playable_before(&queue, self.queue_next - 1)
            {
                self.queue_next = index + 1;
                QueueNavigationResult::Changed {
                    index,
                    path: queue[index].get_path().clone(),
                    reshuffled: Reshuffled::NotReshuffled,
                }
            } else if self.repeat == RepeatState::Repeating
                && !queue.is_empty()
                && let Some(index) = {
                    if self.shuffle {
                        queue.shuffle(&mut rng());
                    }
                    Self::last_playable_index(&queue)
                }
            {
                self.queue_next = index + 1;
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
        };

        if let QueueNavigationResult::Changed { reshuffled, .. } = &result {
            if *reshuffled == Reshuffled::Reshuffled {
                self.persist_session_with_queue();
            } else {
                self.persist_session_state();
            }
        }

        result
    }

    /// Jump to a specific index in the queue.
    pub fn jump(&mut self, index: usize) -> JumpResult {
        let queue = self.queue.read().expect("poisoned queue lock");

        if index < queue.len() && Self::item_is_playable(&queue[index]) {
            let path = queue[index].get_path().clone();
            drop(queue);
            self.queue_next = index + 1;
            self.persist_session_state();
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
        let pos = queue.iter().position(|item| item == &original_item);
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

        queue.push(item.clone());

        let index = queue.len() - 1;

        drop(queue);
        self.persist_session_with_queue();

        index
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

            let mut shuffled = items.clone();
            shuffled.shuffle(&mut rng());
            queue.extend(shuffled);
        } else {
            queue.extend(items.clone());
        }

        drop(queue);
        self.persist_session_with_queue();

        first_index
    }

    /// Insert a single item at a specific position.
    pub fn insert_item(&mut self, position: usize, item: QueueItemData) -> InsertResult {
        let mut queue = self.queue.write().expect("poisoned queue lock");

        let insert_pos = position.min(queue.len());

        if self.shuffle {
            self.original_queue.push(item.clone());
        }

        queue.insert(insert_pos, item.clone());

        drop(queue);

        let result = if insert_pos < self.queue_next {
            self.queue_next += 1;
            InsertResult::InsertedMovedCurrent {
                first_index: insert_pos,
                new_position: self.queue_next - 1,
            }
        } else {
            InsertResult::Inserted {
                first_index: insert_pos,
            }
        };

        self.persist_session_with_queue();
        result
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

        queue.splice(insert_pos..insert_pos, items.clone());

        drop(queue);

        let result = if insert_pos < self.queue_next {
            self.queue_next += items_len;
            InsertResult::InsertedMovedCurrent {
                first_index: insert_pos,
                new_position: self.queue_next - 1,
            }
        } else {
            InsertResult::Inserted {
                first_index: insert_pos,
            }
        };

        self.persist_session_with_queue();
        result
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

        let res = if index == current {
            let new_path = Self::next_playable_from(&queue, current)
                .and_then(|idx| queue.get(idx))
                .map(|v| v.get_path().clone());
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
        };

        drop(queue);
        self.persist_session_with_queue();

        res
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

        let res = if from == current {
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
        };

        drop(queue);
        self.persist_session_with_queue();

        res
    }

    /// Replace the entire queue with new items.
    ///
    /// If shuffle is enabled, the items are shuffled (but original order is preserved).
    pub fn replace_queue(&mut self, items: Vec<QueueItemData>) -> ReplaceResult {
        let mut queue = self.queue.write().expect("poisoned queue lock");

        if self.shuffle {
            let mut shuffled = items.clone();
            shuffled.shuffle(&mut rng());

            self.original_queue = items.clone();
            *queue = shuffled;
        } else {
            self.original_queue.clear();
            *queue = items.clone();
        }

        let first_item = Self::first_playable_index(&queue).map(|idx| queue[idx].clone());

        drop(queue);
        self.queue_next = 0;
        self.persist_session_with_queue();

        match first_item {
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

        drop(queue);
        self.persist_session_with_queue();
    }

    /// Toggle shuffle mode.
    pub fn toggle_shuffle(&mut self) -> ShuffleResult {
        let result = {
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
        };

        self.persist_session_with_queue();
        result
    }

    /// Persist the queue session when only playback state changed.
    ///
    /// This reuses the stored queue snapshot and updates fields like
    /// the current position, shuffle mode, and repeat mode.
    fn persist_session_state(&self) {
        let queue_position = self.current_position();
        let shuffle = self.shuffle;
        let repeat = self.repeat;

        self.storage_tx.send_modify(|session| {
            session.queue_position = queue_position;
            session.shuffle = shuffle;
            session.repeat = repeat;
        });
    }

    /// Persist the queue session when queue contents or ordering changed.
    ///
    /// This refreshes the stored queue alongside the current position,
    /// shuffle mode, and repeat mode.
    fn persist_session_with_queue(&self) {
        let queue = self.queue.read().expect("poisoned queue lock");
        let queue_snapshot = queue.clone();
        let queue_position = self
            .queue_next
            .checked_sub(1)
            .filter(|position| *position < queue.len());
        drop(queue);

        let original_queue = self.original_queue.clone();
        let shuffle = self.shuffle;
        let repeat = self.repeat;

        self.storage_tx.send_modify(move |session| {
            session.queue = queue_snapshot;
            session.original_queue = original_queue;
            session.queue_position = queue_position;
            session.shuffle = shuffle;
            session.repeat = repeat;
        });
    }
}
