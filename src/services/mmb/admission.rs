//! Host-only session admission. Capture a grant at mailbox entry and carry it
//! with derived work; owned session messages remain independent of host policy
//! handles and are suitable for a future WASM boundary.
use crate::sources::TrackRef;
use std::sync::{
    Arc, LazyLock,
    atomic::{AtomicBool, Ordering},
};

pub trait Fence: Send + Sync {
    fn is_valid(&self) -> bool;
}
impl Fence for AtomicBool {
    fn is_valid(&self) -> bool {
        self.load(Ordering::Acquire)
    }
}

#[derive(Clone)]
pub struct Grant(Arc<dyn Fence>);
impl Grant {
    pub fn new(fence: Arc<dyn Fence>) -> Self {
        Self(fence)
    }
    pub fn denied() -> Self {
        static DENIED: LazyLock<Arc<AtomicBool>> =
            LazyLock::new(|| Arc::new(AtomicBool::new(false)));
        Self(DENIED.clone())
    }
    pub fn is_valid(&self) -> bool {
        self.0.is_valid()
    }
}

/// Host implementations must only read cheap local policy snapshots. Admission
/// is synchronous on the publisher; it must never invoke network/plugin code.
pub trait Policy: Send + Sync {
    /// None means there is no extra restriction for this reference.
    fn grant(&self, reference: &TrackRef) -> Option<Grant>;
}
