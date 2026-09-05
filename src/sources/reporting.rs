//! Host-owned reporting persistence and delivery. Backend contracts contain only
//! owned song IDs/timestamps; database leases and account fences stay here.
pub mod delivery;
pub mod live;
pub mod outbox;
pub mod policy;
