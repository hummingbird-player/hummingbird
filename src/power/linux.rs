use std::sync::OnceLock;

use tokio::sync::Mutex;
use zbus::Connection;

const SERVICE: &str = "org.freedesktop.ScreenSaver";
const PATH: &str = "/org/freedesktop/ScreenSaver";

struct State {
    connection: Option<Connection>,
    cookie: Option<u32>,
}

static STATE: OnceLock<Mutex<State>> = OnceLock::new();

fn state() -> &'static Mutex<State> {
    STATE.get_or_init(|| {
        Mutex::new(State {
            connection: None,
            cookie: None,
        })
    })
}

async fn connection(state: &mut State) -> Option<Connection> {
    if state.connection.is_none() {
        match Connection::session().await {
            Ok(conn) => state.connection = Some(conn),
            Err(e) => {
                tracing::warn!("failed to connect to session bus: {e}");
                return None;
            }
        }
    }
    state.connection.clone()
}

pub struct PlatformPower;

impl PlatformPower {
    pub fn new() -> Self {
        Self
    }

    pub fn inhibit(&mut self) {
        crate::RUNTIME.spawn(async {
            let mut state = state().lock().await;
            if state.cookie.is_some() {
                return;
            }
            let Some(conn) = connection(&mut state).await else {
                return;
            };
            let result: Result<u32, _> = conn
                .call_method(
                    Some(SERVICE),
                    PATH,
                    Some(SERVICE),
                    "Inhibit",
                    &("Hummingbird", "Playing media"),
                )
                .await
                .and_then(|r| r.body().deserialize());

            match result {
                Ok(cookie) => state.cookie = Some(cookie),
                Err(e) => tracing::warn!("failed to inhibit screen saver: {e}"),
            }
        });
    }

    pub fn uninhibit(&mut self) {
        crate::RUNTIME.spawn(async {
            let mut state = state().lock().await;
            let Some(cookie) = state.cookie.take() else {
                return;
            };
            let Some(conn) = connection(&mut state).await else {
                return;
            };
            if let Err(e) = conn
                .call_method(Some(SERVICE), PATH, Some(SERVICE), "UnInhibit", &(cookie))
                .await
            {
                tracing::warn!("failed to uninhibit screen saver: {e}");
            }
        });
    }
}
