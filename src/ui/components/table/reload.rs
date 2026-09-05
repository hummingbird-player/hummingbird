//! One active row query and one coalesced follow-up per table.
#[derive(Default)]
pub(super) struct Reload {
    running: bool,
    dirty: bool,
    sort: u64,
}
impl Reload {
    pub fn request(&mut self, sort_changed: bool) -> Option<u64> {
        if sort_changed {
            self.sort = self.sort.wrapping_add(1);
        }
        if self.running {
            self.dirty = true;
            return None;
        }
        self.running = true;
        self.dirty = false;
        Some(self.sort)
    }
    pub fn finish(&mut self, sort: u64) -> (bool, bool) {
        self.running = false;
        (sort == self.sort, self.dirty)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn many_catalog_commits_queue_one_followup_without_starving_partial_results() {
        let mut state = Reload::default();
        let first = state.request(false).unwrap();
        for _ in 0..10_000 {
            assert_eq!(state.request(false), None);
        }
        assert_eq!(state.finish(first), (true, true));
        let second = state.request(false).unwrap();
        assert_eq!(state.finish(second), (true, false));
    }
    #[test]
    fn sort_changes_discard_old_order_and_coalesce_to_the_latest_request() {
        let mut state = Reload::default();
        let first = state.request(false).unwrap();
        assert_eq!(state.request(true), None);
        assert_eq!(state.request(false), None);
        assert_eq!(state.request(true), None);
        assert_eq!(state.finish(first), (false, true));
        let latest = state.request(false).unwrap();
        assert_eq!(state.finish(latest), (true, false));
    }
}
