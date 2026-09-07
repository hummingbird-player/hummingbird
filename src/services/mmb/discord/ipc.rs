use std::path::{Path, PathBuf};

use anyhow::{Context, bail, ensure};
use discord_rich_presence::activity::Activity;
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

#[cfg(unix)]
type Stream = tokio::net::UnixStream;
#[cfg(windows)]
type Stream = tokio::net::windows::named_pipe::NamedPipeClient;

const HANDSHAKE: u32 = 0;
const FRAME: u32 = 1;
const CLOSE: u32 = 2;
const PING: u32 = 3;
const PONG: u32 = 4;
const MAX_FRAME_BYTES: usize = 1024 * 1024;

pub(super) struct IpcClient<S = Stream> {
    stream: S,
    nonce: u64,
}

impl IpcClient {
    pub async fn connect(client_id: &str) -> anyhow::Result<Self> {
        for path in socket_paths() {
            if let Ok(stream) = open(&path).await {
                return Self::handshake(stream, client_id).await;
            }
        }
        bail!("Discord IPC socket not found");
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> IpcClient<S> {
    pub(super) async fn handshake(stream: S, client_id: &str) -> anyhow::Result<Self> {
        let mut client = Self { stream, nonce: 0 };
        client
            .write(HANDSHAKE, &json!({"v": 1, "client_id": client_id}))
            .await?;
        let response = client.receive().await?;
        ensure!(
            response["evt"] == "READY",
            "Discord rejected the IPC handshake"
        );
        Ok(client)
    }

    pub async fn set_activity(&mut self, activity: Option<Activity<'_>>) -> anyhow::Result<()> {
        self.nonce += 1;
        let nonce = self.nonce.to_string();
        self.write(
            FRAME,
            &json!({
                "cmd": "SET_ACTIVITY",
                "args": {"pid": std::process::id(), "activity": activity},
                "nonce": nonce,
            }),
        )
        .await?;

        loop {
            let response = self.receive().await?;
            if response["nonce"] == nonce {
                ensure!(response["evt"] != "ERROR", "Discord rejected the activity");
                return Ok(());
            }
        }
    }

    async fn write(&mut self, opcode: u32, value: &Value) -> anyhow::Result<()> {
        let bytes = serde_json::to_vec(value)?;
        self.write_bytes(opcode, &bytes).await
    }

    async fn write_bytes(&mut self, opcode: u32, bytes: &[u8]) -> anyhow::Result<()> {
        ensure!(
            bytes.len() <= MAX_FRAME_BYTES,
            "Discord IPC frame is too large"
        );
        self.stream.write_u32_le(opcode).await?;
        self.stream.write_u32_le(bytes.len() as u32).await?;
        self.stream.write_all(bytes).await?;
        Ok(())
    }

    async fn receive(&mut self) -> anyhow::Result<Value> {
        loop {
            let opcode = self
                .stream
                .read_u32_le()
                .await
                .context("Discord IPC closed")?;
            let length = self.stream.read_u32_le().await? as usize;
            ensure!(length <= MAX_FRAME_BYTES, "Discord IPC frame is too large");
            let mut bytes = vec![0; length];
            self.stream.read_exact(&mut bytes).await?;
            match opcode {
                FRAME => return Ok(serde_json::from_slice(&bytes)?),
                PING => self.write_bytes(PONG, &bytes).await?,
                CLOSE => bail!("Discord closed the IPC connection"),
                _ => bail!("Unexpected Discord IPC opcode: {opcode}"),
            }
        }
    }
}

#[cfg(unix)]
async fn open(path: &Path) -> std::io::Result<Stream> {
    Stream::connect(path).await
}

#[cfg(windows)]
async fn open(path: &Path) -> std::io::Result<Stream> {
    // opening a busy pipe fails immediately; the service can try again on its next update
    tokio::net::windows::named_pipe::ClientOptions::new().open(path)
}

#[cfg(unix)]
fn socket_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for key in ["XDG_RUNTIME_DIR", "TMPDIR", "TMP", "TEMP"] {
        let Some(base) = std::env::var_os(key) else {
            continue;
        };
        let mut base = PathBuf::from(base);
        if key == "XDG_RUNTIME_DIR" && std::env::var_os("SNAP").is_some() {
            base.pop();
        }
        for i in 0..10 {
            for subpath in [
                "",
                "app/com.discordapp.Discord",
                "app/dev.vencord.Vesktop",
                ".flatpak/com.discordapp.Discord/xdg-run",
                ".flatpak/dev.vencord.Vesktop/xdg-run",
                "snap.discord-canary",
                "snap.discord",
            ] {
                paths.push(base.join(subpath).join(format!("discord-ipc-{i}")));
            }
        }
    }
    paths
}

#[cfg(windows)]
fn socket_paths() -> Vec<PathBuf> {
    (0..10)
        .map(|i| PathBuf::from(format!(r"\\?\pipe\discord-ipc-{i}")))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::io::{DuplexStream, duplex};

    async fn packet(stream: &mut DuplexStream) -> (u32, Vec<u8>) {
        let opcode = stream.read_u32_le().await.unwrap();
        let length = stream.read_u32_le().await.unwrap();
        let mut bytes = vec![0; length as usize];
        stream.read_exact(&mut bytes).await.unwrap();
        (opcode, bytes)
    }

    async fn accept(mut stream: DuplexStream) -> IpcClient<DuplexStream> {
        let (opcode, bytes) = packet(&mut stream).await;
        assert_eq!(opcode, HANDSHAKE);
        assert_eq!(
            serde_json::from_slice::<Value>(&bytes).unwrap(),
            json!({"v": 1, "client_id": "test"})
        );
        let mut server = IpcClient { stream, nonce: 0 };
        server.write(FRAME, &json!({"evt": "READY"})).await.unwrap();
        server
    }

    #[tokio::test]
    async fn handshake_activity_and_clear_use_framed_requests_and_matching_replies() {
        let (client, server) = duplex(16);
        let server = tokio::spawn(async move {
            let mut server = accept(server).await;
            for active in [true, false] {
                let request = server.receive().await.unwrap();
                assert_eq!(request["cmd"], "SET_ACTIVITY");
                assert_eq!(request["args"]["pid"], std::process::id());
                assert_eq!(request["args"]["activity"].is_object(), active);
                if active {
                    assert_eq!(request["args"]["activity"]["details"], "a song");
                }
                // a ping or an unrelated reply can arrive before the activity reply
                server.write_bytes(PING, b"ping").await.unwrap();
                assert_eq!(packet(&mut server.stream).await, (PONG, b"ping".to_vec()));
                server
                    .write(FRAME, &json!({"nonce": "unrelated"}))
                    .await
                    .unwrap();
                server
                    .write(FRAME, &json!({"nonce": request["nonce"]}))
                    .await
                    .unwrap();
            }
        });
        let mut client = IpcClient::handshake(client, "test").await.unwrap();
        client
            .set_activity(Some(Activity::new().details("a song")))
            .await
            .unwrap();
        client.set_activity(None).await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), server)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn cancelling_a_stalled_handshake_closes_the_stream() {
        let (client, mut server) = duplex(1024);
        let client = tokio::spawn(IpcClient::handshake(client, "test"));
        assert_eq!(packet(&mut server).await.0, HANDSHAKE);
        client.abort();
        assert!(matches!(client.await, Err(error) if error.is_cancelled()));
        let mut byte = [0];
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), server.read(&mut byte))
                .await
                .unwrap()
                .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn timeout_closes_a_stalled_handshake() {
        let (client, mut server) = duplex(1024);
        let timeout = tokio::time::timeout(
            Duration::from_millis(30),
            IpcClient::handshake(client, "test"),
        );
        assert!(timeout.await.is_err());
        assert_eq!(packet(&mut server).await.0, HANDSHAKE);
        let mut byte = [0];
        assert_eq!(server.read(&mut byte).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn malformed_oversized_and_rejected_frames_fail() {
        for (opcode, bytes) in [
            (FRAME, b"not json".to_vec()),
            (CLOSE, b"{}".to_vec()),
            (99, b"{}".to_vec()),
            (FRAME, br#"{"evt":"ERROR"}"#.to_vec()),
        ] {
            let (client, server) = duplex(1024);
            let mut server = IpcClient {
                stream: server,
                nonce: 0,
            };
            server.write_bytes(opcode, &bytes).await.unwrap();
            assert!(IpcClient::handshake(client, "test").await.is_err());
        }
        let (client, mut server) = duplex(1024);
        server.write_u32_le(FRAME).await.unwrap();
        server
            .write_u32_le(MAX_FRAME_BYTES as u32 + 1)
            .await
            .unwrap();
        assert!(IpcClient::handshake(client, "test").await.is_err());
    }

    #[tokio::test]
    async fn activity_errors_are_not_reported_as_success() {
        let (client, server) = duplex(1024);
        let server = tokio::spawn(async move {
            let mut server = accept(server).await;
            let request = server.receive().await.unwrap();
            server
                .write(FRAME, &json!({"nonce": request["nonce"], "evt": "ERROR"}))
                .await
                .unwrap();
        });
        let mut client = IpcClient::handshake(client, "test").await.unwrap();
        assert!(client.set_activity(None).await.is_err());
        server.await.unwrap();
    }
}
