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

const DEFAULT_LOG_FILTER: &str = "info,symphonia=warn,zbus=warn";
const LOG_FILE_NAME: &str = "hummingbird.log";
const OLD_LOG_FILE_NAME: &str = "hummingbird.log.old";
const MAX_LOG_FILE_SIZE: u64 = 1024 * 1024;

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

/// Flushes stderr and asks the OS to persist the active log file's data.
pub fn flush() {
    let _ = io::stderr().flush();

    if let Some(file) = LOG_FILE.get()
        && let Ok(mut state) = file.lock()
        && let Some(state) = state.as_mut()
    {
        let _ = state.sync();
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
    RotatingLogFile::open(data_dir, MAX_LOG_FILE_SIZE)
        .ok()
        .map(FileMakeWriter::new)
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

        let active_path = active_log_path_in(data_dir);
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
        let file = String::from_utf8(read_file(&active_path)).unwrap();

        assert!(stderr.contains("integration log test"));
        assert!(file.contains("integration log test"));

        let _ = fs::remove_dir_all(dir);
    }

    /// A failing stderr sink does not prevent writes from reaching the log file.
    #[test]
    fn failing_stderr_still_writes_to_log_file() {
        let dir = temp_dir();
        let active_path = dir.join(LOG_FILE_NAME);
        let _stderr = fail_stderr();

        log_with_layers(open_file_make_writer(&dir));

        let file = String::from_utf8(read_file(&active_path)).unwrap();
        assert!(file.contains("integration log test"));

        let _ = fs::remove_dir_all(dir);
    }
}
