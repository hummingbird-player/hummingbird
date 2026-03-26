use std::{
    fs,
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};

use file_rotate::{
    ContentLimit, FileRotate,
    compression::Compression,
    suffix::{AppendTimestamp, FileLimit},
};
use tracing_subscriber::{
    Layer,
    fmt::{self, MakeWriter, format::FmtSpan},
    layer::SubscriberExt,
    util::SubscriberInitExt,
};

const DEFAULT_LOG_FILTER: &str = "info,symphonia=warn,zbus=warn";
const LOG_FILE_NAME: &str = "hummingbird.log";
const MAX_LOG_FILE_SIZE: usize = 1024 * 1024;
const MAX_LOG_FILES: usize = 4;

type RotatingLogFile = FileRotate<AppendTimestamp>;
type SharedLogFile = Arc<Mutex<Option<RotatingLogFile>>>;

static LOG_FILE: OnceLock<SharedLogFile> = OnceLock::new();

#[cfg(test)]
enum TestStderr {
    Capture(Arc<Mutex<Vec<u8>>>),
    Fail,
}

#[cfg(test)]
thread_local! {
    static TEST_STDERR: std::cell::RefCell<Option<TestStderr>> = const {
        std::cell::RefCell::new(None)
    };
}

/// Initializes logging to stderr and, when available, to a rotating log file.
pub fn init(data_dir: &Path) -> anyhow::Result<()> {
    let env = tracing_subscriber::EnvFilter::builder().parse(filter_value())?; // inform user they have a malformed filter
    let file_writer = open_file_make_writer(data_dir);

    if let Some(writer) = &file_writer {
        let _ = LOG_FILE.set(writer.file.clone());
    }

    let stderr_layer = fmt::layer()
        .with_writer(StderrMakeWriter)
        .with_ansi(io::stderr().is_terminal())
        .with_thread_names(true) // nice to have until we replace with tasks
        .with_span_events(FmtSpan::NEW | FmtSpan::CLOSE) // async can be noisy
        .with_timer(fmt::time::uptime()) // date's useless
        .with_filter(env.clone());
    let file_layer = file_writer.map(|writer| {
        fmt::layer()
            .with_writer(writer)
            .with_ansi(false)
            .with_thread_names(true)
            .with_span_events(FmtSpan::NEW | FmtSpan::CLOSE)
            .with_timer(fmt::time::uptime())
            .with_filter(env)
    });

    let subscriber = tracing_subscriber::registry()
        .with(stderr_layer)
        .with(file_layer);

    #[cfg(feature = "console")]
    let subscriber = subscriber.with(console_subscriber::spawn());

    subscriber.init();
    Ok(())
}

/// Flushes stderr and the active log file.
pub fn flush() {
    let _ = io::stderr().flush();

    if let Some(file) = LOG_FILE.get()
        && let Ok(mut state) = file.lock()
        && let Some(state) = state.as_mut()
    {
        let _ = state.flush();
        let _ = fs::File::open(active_log_path()).and_then(|file| file.sync_data());
    }
}

pub fn active_log_path() -> PathBuf {
    active_log_path_in(&crate::paths::data_dir())
}

fn active_log_path_in(data_dir: &Path) -> PathBuf {
    data_dir.join(LOG_FILE_NAME)
}

fn filter_value() -> String {
    ["HUMMINGBIRD_LOG", "RUST_LOG"] // prefer Hummingbird-specific variable
        .iter() // find the first one that's set at all
        .find_map(|key| std::env::var(key).ok()) // even if it's empty
        .filter(|value| !value.is_empty()) // NOW we can check is_empty and use default
        .unwrap_or_else(|| DEFAULT_LOG_FILTER.to_owned())
}

fn open_file_make_writer(data_dir: &Path) -> Option<FileMakeWriter> {
    Some(FileMakeWriter::new(FileRotate::new(
        active_log_path_in(data_dir),
        AppendTimestamp::default(FileLimit::MaxFiles(MAX_LOG_FILES)),
        ContentLimit::BytesSurpassed(MAX_LOG_FILE_SIZE),
        Compression::None,
        None,
    )))
}

/// Creates stderr writers for the tracing stderr layer.
#[derive(Clone, Copy)]
struct StderrMakeWriter;

impl<'a> MakeWriter<'a> for StderrMakeWriter {
    type Writer = StderrWriter;

    fn make_writer(&'a self) -> Self::Writer {
        StderrWriter {
            buffer: Vec::with_capacity(256),
        }
    }
}

/// Buffers a single formatted log record before writing it to stderr.
struct StderrWriter {
    buffer: Vec<u8>,
}

impl StderrWriter {
    fn commit(&mut self) {
        if self.buffer.is_empty() {
            return;
        }

        let buffer = std::mem::take(&mut self.buffer);
        let _ = write_stderr(&buffer);
    }
}

impl Write for StderrWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.buffer.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.commit();
        Ok(())
    }
}

impl Drop for StderrWriter {
    fn drop(&mut self) {
        self.commit();
    }
}

fn write_stderr(buffer: &[u8]) -> io::Result<()> {
    #[cfg(test)]
    {
        let handled = TEST_STDERR.with(|stderr| match &*stderr.borrow() {
            Some(TestStderr::Capture(stderr)) => {
                let result = match stderr.lock() {
                    Ok(mut stderr) => stderr.write_all(buffer),
                    Err(_) => Err(io::Error::other("test stderr lock poisoned")),
                };
                Some(result)
            }
            Some(TestStderr::Fail) => Some(Err(io::Error::other("test stderr failure"))),
            None => None,
        });

        if let Some(result) = handled {
            return result;
        }
    }

    let mut stderr = io::stderr().lock();
    stderr.write_all(buffer)
}

/// Creates file writers that share access to the rotating log file state.
#[derive(Clone)]
struct FileMakeWriter {
    file: SharedLogFile,
}

impl FileMakeWriter {
    fn new(file: RotatingLogFile) -> Self {
        Self {
            file: Arc::new(Mutex::new(Some(file))),
        }
    }
}

impl<'a> MakeWriter<'a> for FileMakeWriter {
    type Writer = FileWriter;

    fn make_writer(&'a self) -> Self::Writer {
        FileWriter {
            file: self.file.clone(),
            buffer: Vec::with_capacity(256),
        }
    }
}

/// Buffers a single formatted log record before writing it to the shared file.
struct FileWriter {
    file: SharedLogFile,
    buffer: Vec<u8>,
}

impl FileWriter {
    fn commit(&mut self) {
        if self.buffer.is_empty() {
            return;
        }

        let buffer = std::mem::take(&mut self.buffer);
        let Ok(mut state) = self.file.lock() else {
            return;
        };

        let failed = match state.as_mut() {
            Some(state) => state.write_all(&buffer).is_err(),
            None => false,
        };

        if failed {
            *state = None;
        }
    }
}

impl Write for FileWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.buffer.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.commit();
        Ok(())
    }
}

impl Drop for FileWriter {
    fn drop(&mut self) {
        self.commit();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_ID: AtomicU64 = AtomicU64::new(0);

    fn temp_dir() -> PathBuf {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("hummingbird-log-test-{id}"));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn active_log_path_uses_standard_file_name() {
        let dir = temp_dir();
        assert_eq!(super::active_log_path_in(&dir), dir.join(LOG_FILE_NAME));
        let _ = fs::remove_dir_all(dir);
    }

    fn reset_test_stderr() {
        TEST_STDERR.with(|stderr| {
            stderr.borrow_mut().take();
        });
    }

    fn override_stderr(buffer: Arc<Mutex<Vec<u8>>>) -> impl Drop {
        struct Guard;

        impl Drop for Guard {
            fn drop(&mut self) {
                reset_test_stderr();
            }
        }

        TEST_STDERR.with(|stderr| {
            *stderr.borrow_mut() = Some(TestStderr::Capture(buffer));
        });

        Guard
    }

    fn fail_stderr() -> impl Drop {
        struct Guard;

        impl Drop for Guard {
            fn drop(&mut self) {
                reset_test_stderr();
            }
        }

        TEST_STDERR.with(|stderr| {
            *stderr.borrow_mut() = Some(TestStderr::Fail);
        });

        Guard
    }

    fn log_with_layers(file_writer: Option<FileMakeWriter>) {
        let subscriber = tracing_subscriber::registry()
            .with(fmt::layer().with_writer(StderrMakeWriter).without_time())
            .with(file_writer.map(|writer| {
                fmt::layer()
                    .with_writer(writer)
                    .with_ansi(false)
                    .without_time()
            }));

        tracing::subscriber::with_default(subscriber, || {
            tracing::info!("integration log test");
        });
    }

    /// File sink setup failures leave stderr-only logging available.
    #[test]
    fn file_logging_failure_falls_back_to_stderr_only() {
        let dir = temp_dir();
        let file_path = dir.join("not-a-directory");
        fs::write(&file_path, b"x").unwrap();

        assert!(open_file_make_writer(&dir).is_some());
        assert!(open_file_make_writer(&file_path).is_none());

        let _ = fs::remove_dir_all(dir);
    }

    /// A non-terminal stderr sink still allows the log file to be written.
    #[test]
    fn non_tty_stderr_still_writes_to_log_file() {
        let dir = temp_dir();
        let active_path = dir.join(LOG_FILE_NAME);
        let stderr_buffer = Arc::new(Mutex::new(Vec::new()));
        let _stderr = override_stderr(stderr_buffer.clone());

        log_with_layers(open_file_make_writer(&dir));

        let stderr = String::from_utf8(stderr_buffer.lock().unwrap().clone()).unwrap();
        let file = fs::read_to_string(&active_path).unwrap();

        assert!(stderr.contains("integration log test"));
        assert!(file.contains("integration log test"));

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn failing_stderr_still_writes_to_log_file() {
        let dir = temp_dir();
        let active_path = dir.join(LOG_FILE_NAME);
        let _stderr = fail_stderr();

        log_with_layers(open_file_make_writer(&dir));

        let file = fs::read_to_string(&active_path).unwrap();
        assert!(file.contains("integration log test"));

        let _ = fs::remove_dir_all(dir);
    }
}
