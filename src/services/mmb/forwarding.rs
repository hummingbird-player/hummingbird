//! Host policy for source-specific direct-scrobbler exclusions. A grant is
//! captured when a start enters the mailbox, not when a busy worker receives it.
//! Revocation survives an intervening exclude/re-enable and never changes the
//! global service preference. No policy handles enter the session wire contract.
use crate::sources::{SourceId, TrackRef};
use std::{
    collections::HashMap,
    sync::{
        Arc, RwLock,
        atomic::{AtomicBool, Ordering},
    },
};

use super::admission::{self, Grant};

#[derive(Default)]
pub struct Policy {
    allowed: RwLock<HashMap<SourceId, Arc<AtomicBool>>>,
}
impl Policy {
    /// Only configured, nonexcluded remote sources are admitted. Source enable,
    /// credentials and source-server reporting settings are independent policies.
    pub fn configure(&self, sources: impl IntoIterator<Item = SourceId>) {
        // Settings notifications also include frequent unrelated changes such
        // as volume. Keep the usual small unchanged source set allocation-free.
        let sources: smallvec::SmallVec<[SourceId; 4]> = sources
            .into_iter()
            .filter(|source| !source.is_local())
            .collect();
        let mut allowed = self.allowed.write().unwrap_or_else(|e| e.into_inner());
        if sources.len() == allowed.len()
            && sources.iter().enumerate().all(|(index, source)| {
                allowed.contains_key(source) && !sources[..index].contains(source)
            })
        {
            return;
        }
        let mut next = HashMap::with_capacity(sources.len());
        for source in sources {
            let grant = allowed
                .remove(&source)
                .unwrap_or_else(|| Arc::new(AtomicBool::new(true)));
            next.entry(source).or_insert(grant);
        }
        for grant in allowed.values() {
            grant.store(false, Ordering::Release);
        }
        *allowed = next;
    }
    pub fn grant(&self, reference: &TrackRef) -> Option<Grant> {
        if reference.source().is_local() {
            return None;
        }
        Some(
            self.allowed
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .get(reference.source())
                .map(|flag| Grant::new(flag.clone()))
                .unwrap_or_else(Grant::denied),
        )
    }
}
impl admission::Policy for Policy {
    fn grant(&self, reference: &TrackRef) -> Option<Grant> {
        self.grant(reference)
    }
}
impl Drop for Policy {
    fn drop(&mut self) {
        for grant in self
            .allowed
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .values()
        {
            grant.store(false, Ordering::Release);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_policies_are_independent_and_unchanged_settings_preserve_grants_without_allocating()
    {
        let source = SourceId::new("server");
        let reference = TrackRef::from_database(source.clone(), "song".into());
        let lastfm = Policy::default();
        let listenbrainz = Policy::default();
        lastfm.configure([source.clone()]);
        listenbrainz.configure([source.clone()]);
        let lastfm_start = lastfm.grant(&reference).unwrap();
        let listenbrainz_start = listenbrainz.grant(&reference).unwrap();
        let (_, allocations) = crate::test_support::alloc_guard::count_allocations(|| {
            for _ in 0..100 {
                lastfm.configure([source.clone()]);
            }
        });
        assert_eq!(allocations, 0);
        assert!(lastfm_start.is_valid());
        lastfm.configure([]);
        assert!(!lastfm_start.is_valid());
        assert!(listenbrainz_start.is_valid());
        assert!(lastfm.grant(&TrackRef::local("song")).is_none());
        lastfm.configure([source]);
        assert!(!lastfm_start.is_valid());
        assert!(lastfm.grant(&reference).unwrap().is_valid());
        drop(listenbrainz);
        assert!(!listenbrainz_start.is_valid());
        let extra = SourceId::new("removed-source");
        lastfm.configure([reference.source().clone(), extra.clone()]);
        let removed = lastfm
            .grant(&TrackRef::from_database(extra, "song".into()))
            .unwrap();
        lastfm.configure([reference.source().clone(), reference.source().clone()]);
        assert!(!removed.is_valid());
    }
}
