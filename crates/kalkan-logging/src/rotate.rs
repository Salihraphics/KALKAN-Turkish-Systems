//! Size-based log rotation.
//!
//! `kalkan.log` is the active file. Before a write would push it past
//! `max_bytes`, it is closed and renamed to `kalkan.1.log`; existing archives
//! shift up (`kalkan.1.log` → `kalkan.2.log` …) and anything beyond `keep`
//! archives is deleted. Only files named `<stem>.<number>.<ext>` are ever
//! deleted — never the active log.
//!
//! Concurrency: a [`RotatingFile`] is used behind one `Mutex` (see
//! [`SharedFile`]). Every formatted log line is written with a single
//! `write_all` while the lock is held, so a rotation can never split a line
//! or interleave with another thread's write.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RotationPolicy {
    /// Rotate before the active file would exceed this many bytes.
    pub max_bytes: u64,
    /// Number of archives to keep (`<stem>.1.<ext>` … `<stem>.<keep>.<ext>`).
    pub keep: u16,
}

impl RotationPolicy {
    pub fn from_mb(max_mb: u32, keep: u16) -> Self {
        RotationPolicy {
            max_bytes: max_mb.max(1) as u64 * 1024 * 1024,
            keep: keep.max(1),
        }
    }
}

/// What the GUI shows about the log file. Cheap to produce (no disk I/O).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileStatus {
    pub path: PathBuf,
    pub size: u64,
    pub archives: u16,
    pub policy: RotationPolicy,
    pub rotations: u64,
    /// Last I/O error (rotation, write, prune), if any. Cleared by a later
    /// successful rotation.
    pub last_error: Option<String>,
    /// Writes discarded because the file was closed (shutdown) or failed.
    pub dropped_writes: u64,
}

/// `kalkan.log` + 3 → `kalkan.3.log`.
pub fn archive_path(active: &Path, n: u16) -> PathBuf {
    let stem = active
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let name = match active.extension() {
        Some(ext) => format!("{stem}.{n}.{}", ext.to_string_lossy()),
        None => format!("{stem}.{n}"),
    };
    active.with_file_name(name)
}

/// Archive numbers present next to `active` (parsed strictly from names).
pub fn existing_archives(active: &Path) -> Vec<u16> {
    let Some(dir) = active.parent() else {
        return Vec::new();
    };
    let stem = active
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = active
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    let prefix = format!("{stem}.");
    let mut out: Vec<u16> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let mid = name.strip_prefix(&prefix)?.strip_suffix(ext.as_str())?;
            // Only plain decimal numbers: never matches the active file.
            if mid.is_empty() || !mid.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            mid.parse::<u16>().ok().filter(|&n| n >= 1)
        })
        .collect();
    out.sort_unstable();
    out
}

#[derive(Debug)]
pub struct RotatingFile {
    path: PathBuf,
    policy: RotationPolicy,
    file: Option<File>,
    size: u64,
    archives: u16,
    rotations: u64,
    last_error: Option<String>,
    dropped_writes: u64,
    closed: bool,
    /// After a failed rotation, do not retry before the file grows this big
    /// (avoids hammering the disk on every write).
    retry_at: u64,
}

impl RotatingFile {
    /// Open (append) the active file, continuing an existing one after a
    /// restart. Rotates immediately if it is already over the limit.
    pub fn open(path: &Path, policy: RotationPolicy) -> io::Result<Self> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        let size = file.metadata()?.len();
        let mut rf = RotatingFile {
            path: path.to_path_buf(),
            policy,
            file: Some(file),
            size,
            archives: 0,
            rotations: 0,
            last_error: None,
            dropped_writes: 0,
            closed: false,
            retry_at: 0,
        };
        rf.prune();
        if rf.size >= rf.policy.max_bytes {
            rf.rotate();
        }
        Ok(rf)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Size of the active file when it was opened by [`RotatingFile::open`]
    /// is not tracked here; see `LoggingHandle` for the history boundary.
    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn status(&self) -> FileStatus {
        FileStatus {
            path: self.path.clone(),
            size: self.size,
            archives: self.archives,
            policy: self.policy,
            rotations: self.rotations,
            last_error: self.last_error.clone(),
            dropped_writes: self.dropped_writes,
        }
    }

    /// Apply a new policy now: prune archives beyond the new `keep`; a smaller
    /// size limit takes effect at the next write.
    pub fn set_policy(&mut self, policy: RotationPolicy) {
        self.policy = policy;
        self.retry_at = 0;
        self.prune();
    }

    /// Delete archives numbered above `keep`. Never touches the active file.
    fn prune(&mut self) {
        let keep = self.policy.keep;
        for n in existing_archives(&self.path) {
            if n > keep {
                if let Err(e) = fs::remove_file(archive_path(&self.path, n)) {
                    self.last_error = Some(format!("could not delete old log archive {n}: {e}"));
                }
            }
        }
        self.archives = existing_archives(&self.path)
            .iter()
            .filter(|&&n| n <= keep)
            .count() as u16;
    }

    /// Rotate now. On failure the error is recorded and logging continues
    /// into the active file.
    fn rotate(&mut self) {
        match self.try_rotate() {
            Ok(()) => {
                self.rotations += 1;
                self.last_error = None;
                self.retry_at = 0;
            }
            Err(e) => {
                self.last_error = Some(format!("log rotation failed: {e}"));
                self.retry_at = self.size + (self.policy.max_bytes / 4).max(4096);
                if self.file.is_none() {
                    // Keep logging into the (still existing) active file.
                    self.file = OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&self.path)
                        .ok();
                }
            }
        }
        self.archives = existing_archives(&self.path)
            .iter()
            .filter(|&&n| n <= self.policy.keep)
            .count() as u16;
    }

    fn try_rotate(&mut self) -> io::Result<()> {
        if let Some(mut f) = self.file.take() {
            f.flush()?;
            // Dropped here: Windows cannot rename an open file.
        }
        let keep = self.policy.keep.max(1);
        let oldest = archive_path(&self.path, keep);
        match fs::remove_file(&oldest) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        for n in (1..keep).rev() {
            let from = archive_path(&self.path, n);
            if from.exists() {
                fs::rename(&from, archive_path(&self.path, n + 1))?;
            }
        }
        fs::rename(&self.path, archive_path(&self.path, 1))?;
        self.file = Some(
            OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&self.path)?,
        );
        self.size = 0;
        Ok(())
    }

    /// Flush and close. Later writes are counted as dropped (shutdown).
    pub fn close(&mut self) {
        if let Some(mut f) = self.file.take() {
            let _ = f.flush();
        }
        self.closed = true;
    }
}

impl Write for RotatingFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.closed {
            self.dropped_writes += 1;
            return Ok(buf.len());
        }
        let len = buf.len() as u64;
        if self.size > 0 && self.size + len > self.policy.max_bytes && self.size >= self.retry_at {
            self.rotate();
        }
        let Some(f) = self.file.as_mut() else {
            self.dropped_writes += 1;
            return Err(io::Error::other("log file is not open"));
        };
        match f.write_all(buf) {
            Ok(()) => {
                self.size += len;
                Ok(buf.len())
            }
            Err(e) => {
                self.dropped_writes += 1;
                self.last_error = Some(format!("log write failed: {e}"));
                Err(e)
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self.file.as_mut() {
            Some(f) => f.flush(),
            None => Ok(()),
        }
    }
}

/// A [`RotatingFile`] shared between the logging subscriber (writer) and the
/// GUI (status, policy changes).
#[derive(Debug, Clone)]
pub struct SharedFile(pub Arc<Mutex<RotatingFile>>);

impl SharedFile {
    pub fn new(f: RotatingFile) -> Self {
        SharedFile(Arc::new(Mutex::new(f)))
    }
    /// Lock, recovering from a poisoned lock (a panic while logging must not
    /// disable logging for the rest of the run).
    pub fn lock(&self) -> MutexGuard<'_, RotatingFile> {
        self.0.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// Writer handed to `tracing-subscriber`: one lock per formatted event.
#[derive(Debug)]
pub struct FileGuard<'a>(MutexGuard<'a, RotatingFile>);

impl Write for FileGuard<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }
    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        self.0.write(buf).map(|_| ())
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for SharedFile {
    type Writer = FileGuard<'a>;
    fn make_writer(&'a self) -> Self::Writer {
        FileGuard(self.lock())
    }
}

#[cfg(test)]
#[path = "rotate_tests.rs"]
mod tests;
