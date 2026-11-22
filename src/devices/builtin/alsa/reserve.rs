//! The `org.freedesktop.ReserveDevice1` device reservation protocol. The original specification
//! is no longer online; `pw-reserve` (<https://docs.pipewire.org/page_man_pw-reserve_1.html>)
//! implements the same protocol.

use std::{
    path::Path,
    pin::pin,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use futures::{
    channel::oneshot,
    executor::block_on,
    future::{Either, select},
};
use tracing::{debug, warn};
use zbus::{
    Connection,
    fdo::{RequestNameFlags, RequestNameReply},
};

const SERVICE_PREFIX: &str = "org.freedesktop.ReserveDevice1.Audio";
const PATH_PREFIX: &str = "/org/freedesktop/ReserveDevice1/Audio";
const INTERFACE: &str = "org.freedesktop.ReserveDevice1";
/// Above WirePlumber (-20) and PulseAudio (0), below JACK (`i32::MAX`).
pub(super) const PRIORITY: i32 = 1;
const APPLICATION_NAME: &str = "Hummingbird";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ReserveError {
    /// The holder refused to release the card.
    Busy,
    /// We aren't allowed to own reservation names, usually because of a sandbox.
    Sandboxed,
    Other(String),
}

/// Shared session bus connection. `None` without a session bus, and so without a sound server.
fn connection() -> Option<&'static Connection> {
    static CONNECTION: OnceLock<Option<Connection>> = OnceLock::new();
    CONNECTION
        .get_or_init(|| match block_on(Connection::session()) {
            Ok(connection) => Some(connection),
            Err(error) => {
                warn!("no session bus; ALSA devices will be opened without reservation: {error}");
                None
            }
        })
        .as_ref()
}

fn service_name(card: i32) -> String {
    format!("{SERVICE_PREFIX}{card}")
}

fn object_path(card: i32) -> String {
    format!("{PATH_PREFIX}{card}")
}

fn is_access_denied(error: &zbus::Error) -> bool {
    // depending on what refused the request, this is either an FDO error or a method error
    format!("{error:?}").contains("AccessDenied")
}

/// Timeout for property queries. WirePlumber can be slow to answer while setting up a card.
const QUERY_TIMEOUT: Duration = Duration::from_millis(300);
/// How long to keep a reservation after its stream closes mid-playback, so a stream reopening
/// in another format can reuse it.
const LINGER: Duration = Duration::from_secs(2);
/// How long to keep retrying if the current owner refuses to release the card.
const ACQUIRE_PATIENCE: Duration = Duration::from_millis(2500);

/// Resolves to `None` if `future` takes longer than `timeout`.
async fn with_timeout<F: Future>(future: F, timeout: Duration) -> Option<F::Output> {
    let (timer_tx, timer_rx) = oneshot::channel::<()>();
    std::thread::spawn(move || {
        std::thread::sleep(timeout);
        let _ = timer_tx.send(());
    });
    match select(pin!(future), timer_rx).await {
        Either::Left((output, _)) => Some(output),
        Either::Right(_) => None,
    }
}

/// The device name the card's owner publishes (`alsa_card.*` for WirePlumber).
pub(super) fn application_device_name(card: i32) -> Option<String> {
    holder_property(card, "ApplicationDeviceName")
}

/// A string property of the card's owner. `None` if there's no owner or it doesn't respond.
pub(super) fn holder_property(card: i32, property: &str) -> Option<String> {
    let connection = connection()?;
    let name = service_name(card);
    let path = object_path(card);
    let body = (INTERFACE, property);
    let call = connection.call_method(
        Some(name.as_str()),
        path.as_str(),
        Some("org.freedesktop.DBus.Properties"),
        "Get",
        &body,
    );
    let reply = block_on(with_timeout(call, QUERY_TIMEOUT))?.ok()?;
    let value: zbus::OwnedVariant = reply.body().deserialize().ok()?;
    String::try_from(value).ok()
}

/// Used to tell a stream that another application has asked for its card over D-Bus.
#[derive(Default)]
pub(super) struct ReleaseSignal {
    requested: AtomicBool,
    /// Notified once the card is closed and the reservation has been dropped.
    waiters: Mutex<Vec<oneshot::Sender<()>>>,
}

impl ReleaseSignal {
    pub(super) fn requested(&self) -> bool {
        self.requested.load(Ordering::Acquire)
    }

    /// Tells every waiting `RequestRelease` caller that the card is now free.
    pub(super) fn released(&self) {
        self.requested.store(false, Ordering::Release);
        for waiter in self.waiters.lock().unwrap().drain(..) {
            let _ = waiter.send(());
        }
    }
}

/// State shared between a [`Reservation`] and its D-Bus object.
#[derive(Default)]
struct Holder {
    /// The stream using the card. `None` while lingering.
    stream: Mutex<Option<Arc<ReleaseSignal>>>,
    /// Set once the card was given to another application.
    revoked: AtomicBool,
}

struct ReserveDevice {
    device_name: String,
    holder: Arc<Holder>,
}

#[zbus::interface(name = "org.freedesktop.ReserveDevice1")]
impl ReserveDevice {
    async fn request_release(&self, priority: i32) -> bool {
        if priority <= PRIORITY {
            debug!(
                "refused release of {} to priority {priority}",
                self.device_name
            );
            return false;
        }
        let stream = self.holder.stream.lock().unwrap().clone();
        self.holder.revoked.store(true, Ordering::Release);
        if let Some(stream) = stream {
            let (sender, receiver) = oneshot::channel();
            stream.waiters.lock().unwrap().push(sender);
            stream.requested.store(true, Ordering::Release);
            // `Err` means the render thread exited, so the card is closed anyway
            let _ = receiver.await;
        }
        debug!("released {} to priority {priority}", self.device_name);
        true
    }

    #[zbus(property)]
    fn priority(&self) -> i32 {
        PRIORITY
    }

    #[zbus(property)]
    fn application_name(&self) -> String {
        APPLICATION_NAME.to_string()
    }

    #[zbus(property)]
    fn application_device_name(&self) -> String {
        self.device_name.clone()
    }
}

/// Reservations kept after their stream closed mid-playback, by card. The `u64` stops a stale
/// timer from dropping a newer one.
static LINGERING: Mutex<Vec<(i32, u64, Reservation)>> = Mutex::new(Vec::new());
static LINGER_ID: AtomicU64 = AtomicU64::new(0);
/// When we last gave each card back, keyed by card index.
static RELEASED_AT: Mutex<Vec<(i32, Instant)>> = Mutex::new(Vec::new());
/// How long WirePlumber can take to set up a card again after getting it back.
const REBUILD_WINDOW: Duration = Duration::from_secs(5);

/// Whether we gave `card` back recently enough that WirePlumber may still be setting it up.
/// Taking it again then hits a WirePlumber (0.5.19) race that adds a `.2` suffix to the sink
/// name, changing the user's default device.
pub(super) fn recently_released(card: i32) -> bool {
    RELEASED_AT
        .lock()
        .unwrap()
        .iter()
        .any(|(c, at)| *c == card && at.elapsed() < REBUILD_WINDOW)
}

/// A reservation for a card. Dropping it gives the card back.
pub(super) struct Reservation {
    connection: &'static Connection,
    card: i32,
    holder: Arc<Holder>,
}

impl Reservation {
    /// Reserves the card, reusing a lingering reservation or asking the owner to release it.
    /// `Ok(None)` if there's no session bus.
    pub(super) fn acquire(
        card: i32,
        release: Arc<ReleaseSignal>,
    ) -> Result<Option<Self>, ReserveError> {
        if let Some(reservation) = take_lingering(card) {
            if !reservation.holder.revoked.load(Ordering::Acquire) {
                *reservation.holder.stream.lock().unwrap() = Some(release);
                debug!("reusing reservation of ALSA card {card}");
                return Ok(Some(reservation));
            }
            // given away while lingering
            drop(reservation);
        }

        let Some(connection) = connection() else {
            return Ok(None);
        };
        let name = service_name(card);
        let name = name.as_str();
        let path = object_path(card);
        let device_name = application_device_name(card).unwrap_or_else(|| format!("hw:{card}"));
        let holder = Arc::new(Holder {
            stream: Mutex::new(Some(release)),
            revoked: AtomicBool::new(false),
        });

        let reservation = Self {
            connection,
            card,
            holder: holder.clone(),
        };
        block_on(async {
            connection
                .object_server()
                .at(
                    path.as_str(),
                    ReserveDevice {
                        device_name,
                        holder,
                    },
                )
                .await
                .map_err(|e| ReserveError::Other(e.to_string()))?;

            let flags = RequestNameFlags::DO_NOT_QUEUE | RequestNameFlags::ALLOW_REPLACEMENT;
            // whether we now own the name
            let request = |flags| async move {
                match connection.request_name_with_flags(name, flags).await {
                    Ok(RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner) => Ok(true),
                    Ok(RequestNameReply::Exists | RequestNameReply::InQueue)
                    | Err(zbus::Error::NameTaken) => Ok(false),
                    Err(error) if is_access_denied(&error) => Err(ReserveError::Sandboxed),
                    Err(error) => Err(ReserveError::Other(error.to_string())),
                }
            };

            if request(flags).await? {
                return Ok(());
            }

            // WirePlumber refuses while it's setting up a card, so retry for a bit
            let started = Instant::now();
            let mut delay = Duration::from_millis(100);
            loop {
                let granted = connection
                    .call_method(
                        Some(name),
                        path.as_str(),
                        Some(INTERFACE),
                        "RequestRelease",
                        &(PRIORITY,),
                    )
                    .await
                    .and_then(|reply| reply.body().deserialize::<bool>());
                match granted {
                    Ok(true) => break,
                    Err(error) if is_access_denied(&error) => return Err(ReserveError::Sandboxed),
                    Ok(false) => debug!("{name} refused RequestRelease"),
                    Err(error) => debug!("RequestRelease on {name} failed: {error}"),
                }
                // the owner may have released the name in the meantime
                if request(flags).await? {
                    return Ok(());
                }
                if started.elapsed() + delay > ACQUIRE_PATIENCE {
                    return Err(ReserveError::Busy);
                }
                std::thread::sleep(delay);
                delay *= 2;
            }

            if request(flags | RequestNameFlags::REPLACE_EXISTING).await? {
                Ok(())
            } else {
                Err(ReserveError::Busy)
            }
        })?;

        debug!("reserved ALSA card {card}");
        Ok(Some(reservation))
    }

    /// Keeps the reservation for [`LINGER`] after the stream closed the card, so a stream
    /// reopening on it can reuse it. Release requests are granted immediately meanwhile.
    pub(super) fn linger(self) {
        *self.holder.stream.lock().unwrap() = None;
        let card = self.card;
        let id = LINGER_ID.fetch_add(1, Ordering::Relaxed);
        let previous = {
            let mut lingering = LINGERING.lock().unwrap();
            let previous = lingering
                .iter()
                .position(|(c, _, _)| *c == card)
                .map(|index| lingering.swap_remove(index));
            lingering.push((card, id, self));
            previous
        };
        drop(previous);

        std::thread::spawn(move || {
            std::thread::sleep(LINGER);
            let expired = {
                let mut lingering = LINGERING.lock().unwrap();
                lingering
                    .iter()
                    .position(|(_, i, _)| *i == id)
                    .map(|index| lingering.swap_remove(index))
            };
            // dropped outside the lock, since releasing makes a D-Bus call
            drop(expired);
        });
    }
}

/// Whether we're still holding `card` with a lingering reservation.
pub(super) fn is_lingering(card: i32) -> bool {
    LINGERING.lock().unwrap().iter().any(|(c, _, _)| *c == card)
}

fn take_lingering(card: i32) -> Option<Reservation> {
    let mut lingering = LINGERING.lock().unwrap();
    let index = lingering.iter().position(|(c, _, _)| *c == card)?;
    Some(lingering.swap_remove(index).2)
}

/// Gives back every lingering card. Used when bit-perfect playback is turned off.
pub(super) fn release_lingering() {
    let lingering: Vec<_> = std::mem::take(&mut *LINGERING.lock().unwrap());
    drop(lingering);
}

impl Drop for Reservation {
    fn drop(&mut self) {
        let name = service_name(self.card);
        let path = object_path(self.card);
        block_on(async {
            if !self.holder.revoked.load(Ordering::Acquire)
                && let Err(error) = self.connection.release_name(name.as_str()).await
            {
                warn!("could not release {name}: {error}");
            }
            let _ = self
                .connection
                .object_server()
                .remove::<ReserveDevice, _>(path.as_str())
                .await;
        });
        let mut released = RELEASED_AT.lock().unwrap();
        released.retain(|(card, _)| *card != self.card);
        released.push((self.card, Instant::now()));
        debug!("released ALSA card {}", self.card);
    }
}

/// Whether a Flatpak sandbox lacks the session bus or `/dev/snd` permissions we need. Other
/// sandboxes are detected when they deny access.
pub(super) fn sandbox_blocks_access() -> bool {
    let Ok(info) = std::fs::read_to_string("/.flatpak-info") else {
        return false;
    };
    !Path::new("/dev/snd").exists() || !flatpak_allows_reservation(&info)
}

fn flatpak_allows_reservation(info: &str) -> bool {
    let mut section = "";
    for line in info.lines().map(str::trim) {
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = name;
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match section {
            // full access to the session bus
            "Context" if key == "sockets" && value.split(';').any(|s| s == "session-bus") => {
                return true;
            }
            "Session Bus Policy"
                if value == "own"
                    && (key == "org.freedesktop.ReserveDevice1.*"
                        || key == "org.freedesktop.*"
                        || key.starts_with(SERVICE_PREFIX)) =>
            {
                return true;
            }
            _ => {}
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::flatpak_allows_reservation;

    #[test]
    fn reads_flatpak_bus_policy() {
        let without = "[Application]\nname=org.example\n\n[Context]\nsockets=wayland;pulseaudio;\n\
                       \n[Session Bus Policy]\norg.mpris.MediaPlayer2.Hummingbird=own\n";
        assert!(!flatpak_allows_reservation(without));

        let with = format!("{without}org.freedesktop.ReserveDevice1.*=own\n");
        assert!(flatpak_allows_reservation(&with));

        let talk_only = format!("{without}org.freedesktop.ReserveDevice1.*=talk\n");
        assert!(!flatpak_allows_reservation(&talk_only));

        let full_bus = "[Context]\nsockets=wayland;session-bus;\n";
        assert!(flatpak_allows_reservation(full_bus));
    }
}
