use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};

use tracing_subscriber::{
    Layer,
    fmt::{self, MakeWriter, format::FmtSpan},
    layer::SubscriberExt,
    util::SubscriberInitExt,
};

const DEFAULT_LOG_FILTER: &str = "info,blade_graphics=warn,symphonia=warn,zbus=warn";
const LOG_FILE_NAME: &str = "hummingbird.log";
const OLD_LOG_FILE_NAME: &str = "hummingbird.log.old";
const MAX_LOG_FILE_SIZE: u64 = 1024 * 1024;

type SharedLogFile = Arc<Mutex<Option<RotatingLogFile>>>;

static LOG_FILE: OnceLock<SharedLogFile> = OnceLock::new();

#[cfg(test)]
enum TestStdout {
    Capture(Arc<Mutex<Vec<u8>>>),
    Fail,
}

#[cfg(test)]
thread_local! {
    static TEST_STDOUT: std::cell::RefCell<Option<TestStdout>> = const {
        std::cell::RefCell::new(None)
    };
}

/// Initializes logging to stdout and, when available, to a rotating log file.
pub fn init(data_dir: &Path) -> anyhow::Result<()> {
    let filter = filter_value();
    let file_writer = open_file_make_writer(data_dir);

    if let Some(writer) = &file_writer {
        let _ = LOG_FILE.set(writer.file.clone());
    }

    let stdout_layer = fmt::layer()
        .with_writer(StdoutMakeWriter)
        .with_thread_names(true)
        .with_span_events(FmtSpan::NEW | FmtSpan::CLOSE)
        .with_timer(fmt::time::uptime())
        .with_filter(tracing_subscriber::EnvFilter::builder().parse(&filter)?);
    let file_layer = file_writer.map(|writer| {
        fmt::layer()
            .with_writer(writer)
            .with_ansi(false)
            .with_thread_names(true)
            .with_span_events(FmtSpan::NEW | FmtSpan::CLOSE)
            .with_timer(fmt::time::uptime())
            .with_filter(
                tracing_subscriber::EnvFilter::builder()
                    .parse(&filter)
                    .unwrap(),
            )
    });

    let subscriber = tracing_subscriber::registry()
        .with(stdout_layer)
        .with(file_layer);

    #[cfg(feature = "console")]
    let subscriber = subscriber.with(console_subscriber::spawn());

    subscriber.init();
    Ok(())
}

/// Flushes stdout and asks the OS to persist the active log file's data.
pub fn flush() {
    let _ = io::stdout().flush();

    if let Some(file) = LOG_FILE.get()
        && let Ok(mut state) = file.lock()
        && let Some(state) = state.as_mut()
    {
        let _ = state.sync();
    }
}

fn filter_value() -> String {
    ["HUMMINGBIRD_LOG", "RUST_LOG"]
        .iter()
        .find_map(|key| std::env::var(key).ok())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_LOG_FILTER.to_owned())
}

fn open_file_make_writer(data_dir: &Path) -> Option<FileMakeWriter> {
    RotatingLogFile::open(data_dir, MAX_LOG_FILE_SIZE)
        .ok()
        .map(FileMakeWriter::new)
}

/// Creates stdout writers for the tracing stdout layer.
#[derive(Clone, Copy)]
struct StdoutMakeWriter;

impl<'a> MakeWriter<'a> for StdoutMakeWriter {
    type Writer = StdoutWriter;

    fn make_writer(&'a self) -> Self::Writer {
        StdoutWriter {
            buffer: Vec::with_capacity(256),
        }
    }
}

/// Buffers a single formatted log record before writing it to stdout.
struct StdoutWriter {
    buffer: Vec<u8>,
}

impl StdoutWriter {
    fn commit(&mut self) {
        if self.buffer.is_empty() {
            return;
        }

        let buffer = std::mem::take(&mut self.buffer);
        let _ = write_stdout(&buffer);
    }
}

impl Write for StdoutWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.buffer.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.commit();
        Ok(())
    }
}

impl Drop for StdoutWriter {
    fn drop(&mut self) {
        self.commit();
    }
}

fn write_stdout(buffer: &[u8]) -> io::Result<()> {
    #[cfg(test)]
    {
        let handled = TEST_STDOUT.with(|stdout| match &*stdout.borrow() {
            Some(TestStdout::Capture(stdout)) => {
                let result = match stdout.lock() {
                    Ok(mut stdout) => stdout.write_all(buffer),
                    Err(_) => Err(io::Error::other("test stdout lock poisoned")),
                };
                Some(result)
            }
            Some(TestStdout::Fail) => Some(Err(io::Error::other("test stdout failure"))),
            None => None,
        });

        if let Some(result) = handled {
            return result;
        }
    }

    let mut stdout = io::stdout().lock();
    stdout.write_all(buffer)
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
            Some(state) => state.write_record(&buffer).is_err(),
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

/// Writes log records to `hummingbird.log` and rotates to `.old` before the
/// next write would push the active file past the size limit.
struct RotatingLogFile {
    file: Option<File>,
    active_path: PathBuf,
    old_path: PathBuf,
    current_len: u64,
    max_len: u64,
}

impl RotatingLogFile {
    fn open(data_dir: &Path, max_len: u64) -> io::Result<Self> {
        fs::create_dir_all(data_dir)?;

        let active_path = data_dir.join(LOG_FILE_NAME);
        let old_path = data_dir.join(OLD_LOG_FILE_NAME);
        let current_len = fs::metadata(&active_path)
            .map(|metadata| metadata.len())
            .unwrap_or(0);

        let mut file = Self {
            file: None,
            active_path,
            old_path,
            current_len,
            max_len,
        };

        if file.current_len > file.max_len {
            file.rotate_existing()?;
        } else {
            file.file = Some(Self::open_append(&file.active_path)?);
        }

        Ok(file)
    }

    fn open_append(path: &Path) -> io::Result<File> {
        OpenOptions::new().create(true).append(true).open(path)
    }

    fn open_truncated(path: &Path) -> io::Result<File> {
        OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)
    }

    /// Writes one fully formatted log record, rotating first if needed.
    fn write_record(&mut self, record: &[u8]) -> io::Result<()> {
        let record_len = record.len() as u64;
        if self.current_len > 0 && self.current_len.saturating_add(record_len) > self.max_len {
            self.rotate()?;
        }

        self.file().expect("log file missing").write_all(record)?;
        self.current_len = self.current_len.saturating_add(record_len);
        Ok(())
    }

    fn rotate(&mut self) -> io::Result<()> {
        self.close();
        self.rotate_existing()
    }

    /// Copies the current active log to `.old` and reopens the active file truncated.
    fn rotate_existing(&mut self) -> io::Result<()> {
        if self.active_path.exists() {
            fs::copy(&self.active_path, &self.old_path)?;
        }

        self.file = Some(Self::open_truncated(&self.active_path)?);
        self.current_len = 0;
        Ok(())
    }

    fn file(&mut self) -> Option<&mut File> {
        self.file.as_mut()
    }

    fn close(&mut self) {
        self.file.take();
    }

    fn sync(&mut self) -> io::Result<()> {
        if let Some(file) = self.file() {
            file.sync_data()?;
        }

        Ok(())
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

    fn read_file(path: &Path) -> Vec<u8> {
        fs::read(path).unwrap_or_default()
    }

    fn reset_test_stdout() {
        TEST_STDOUT.with(|stdout| {
            stdout.borrow_mut().take();
        });
    }

    fn override_stdout(buffer: Arc<Mutex<Vec<u8>>>) -> impl Drop {
        struct Guard;

        impl Drop for Guard {
            fn drop(&mut self) {
                reset_test_stdout();
            }
        }

        TEST_STDOUT.with(|stdout| {
            *stdout.borrow_mut() = Some(TestStdout::Capture(buffer));
        });

        Guard
    }

    fn fail_stdout() -> impl Drop {
        struct Guard;

        impl Drop for Guard {
            fn drop(&mut self) {
                reset_test_stdout();
            }
        }

        TEST_STDOUT.with(|stdout| {
            *stdout.borrow_mut() = Some(TestStdout::Fail);
        });

        Guard
    }

    fn log_with_layers(file_writer: Option<FileMakeWriter>) {
        let subscriber = tracing_subscriber::registry()
            .with(fmt::layer().with_writer(StdoutMakeWriter).without_time())
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

    /// Writing a small record keeps the active log file in place.
    #[test]
    fn write_below_threshold_does_not_rotate() {
        let dir = temp_dir();
        let active_path = dir.join(LOG_FILE_NAME);
        let old_path = dir.join(OLD_LOG_FILE_NAME);

        {
            let mut log = RotatingLogFile::open(&dir, 1024).unwrap();
            log.write_record(b"hello\n").unwrap();
            log.close();
        }

        assert_eq!(read_file(&active_path), b"hello\n");
        assert!(!old_path.exists());

        let _ = fs::remove_dir_all(dir);
    }

    /// Rotation happens before a record would push the file past the limit.
    #[test]
    fn rotates_before_the_crossing_write() {
        let dir = temp_dir();
        let active_path = dir.join(LOG_FILE_NAME);
        let old_path = dir.join(OLD_LOG_FILE_NAME);

        fs::write(&active_path, vec![b'a'; 1023]).unwrap();

        {
            let mut log = RotatingLogFile::open(&dir, 1024).unwrap();
            log.write_record(b"bc").unwrap();
            log.close();
        }

        assert_eq!(read_file(&old_path), vec![b'a'; 1023]);
        assert_eq!(read_file(&active_path), b"bc");

        let _ = fs::remove_dir_all(dir);
    }

    /// A new rotation overwrites any previous backup file.
    #[test]
    fn rotation_replaces_existing_backup() {
        let dir = temp_dir();
        let active_path = dir.join(LOG_FILE_NAME);
        let old_path = dir.join(OLD_LOG_FILE_NAME);

        fs::write(&active_path, b"abcd").unwrap();
        fs::write(&old_path, b"stale").unwrap();

        {
            let mut log = RotatingLogFile::open(&dir, 4).unwrap();
            log.write_record(b"e").unwrap();
            log.close();
        }

        assert_eq!(read_file(&old_path), b"abcd");
        assert_eq!(read_file(&active_path), b"e");

        let _ = fs::remove_dir_all(dir);
    }

    /// Opening an oversized active log rotates it immediately.
    #[test]
    fn oversized_active_log_rotates_on_open() {
        let dir = temp_dir();
        let active_path = dir.join(LOG_FILE_NAME);
        let old_path = dir.join(OLD_LOG_FILE_NAME);

        fs::write(&active_path, b"abcde").unwrap();

        {
            let mut log = RotatingLogFile::open(&dir, 4).unwrap();
            log.close();
        }

        assert_eq!(read_file(&old_path), b"abcde");
        assert!(read_file(&active_path).is_empty());

        let _ = fs::remove_dir_all(dir);
    }

    /// File sink setup failures leave stdout-only logging available.
    #[test]
    fn file_logging_failure_falls_back_to_stdout_only() {
        let dir = temp_dir();
        let file_path = dir.join("not-a-directory");
        fs::write(&file_path, b"x").unwrap();

        assert!(open_file_make_writer(&dir).is_some());
        assert!(open_file_make_writer(&file_path).is_none());

        let _ = fs::remove_dir_all(dir);
    }

    /// A non-terminal stdout sink still allows the log file to be written.
    #[test]
    fn non_tty_stdout_still_writes_to_log_file() {
        let dir = temp_dir();
        let active_path = dir.join(LOG_FILE_NAME);
        let stdout_buffer = Arc::new(Mutex::new(Vec::new()));
        let _stdout = override_stdout(stdout_buffer.clone());

        log_with_layers(open_file_make_writer(&dir));

        let stdout = String::from_utf8(stdout_buffer.lock().unwrap().clone()).unwrap();
        let file = String::from_utf8(read_file(&active_path)).unwrap();

        assert!(stdout.contains("integration log test"));
        assert!(file.contains("integration log test"));

        let _ = fs::remove_dir_all(dir);
    }

    /// A failing stdout sink does not prevent writes from reaching the log file.
    #[test]
    fn failing_stdout_still_writes_to_log_file() {
        let dir = temp_dir();
        let active_path = dir.join(LOG_FILE_NAME);
        let _stdout = fail_stdout();

        log_with_layers(open_file_make_writer(&dir));

        let file = String::from_utf8(read_file(&active_path)).unwrap();
        assert!(file.contains("integration log test"));

        let _ = fs::remove_dir_all(dir);
    }
}
