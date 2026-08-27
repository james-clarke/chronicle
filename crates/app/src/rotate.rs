use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Rotate the active log at this size; exactly one rotated file (`<name>.1`)
/// is kept — 5 MB active + 5 MB rotated is plenty for an activity tracker.
const MAX_BYTES: u64 = 5 * 1024 * 1024;

/// Size-based rotation `tracing-appender` doesn't offer. No locking: the
/// `non_blocking` wrapper funnels all writes through one worker thread.
pub(crate) struct SizeRotatingWriter {
    path: PathBuf,
    rotated_path: PathBuf,
    file: File,
    size: u64,
    max_bytes: u64,
}

impl SizeRotatingWriter {
    pub(crate) fn open(dir: &Path, name: &str) -> io::Result<Self> {
        Self::open_with_max(dir, name, MAX_BYTES)
    }

    fn open_with_max(dir: &Path, name: &str, max_bytes: u64) -> io::Result<Self> {
        fs::create_dir_all(dir)?;
        let path = dir.join(name);
        let rotated_path = dir.join(format!("{name}.1"));
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        // Seed from the on-disk size so restarts keep rotating correctly.
        let size = file.metadata()?.len();
        Ok(Self {
            path,
            rotated_path,
            file,
            size,
            max_bytes,
        })
    }

    fn rotate(&mut self) -> io::Result<()> {
        let _ = fs::remove_file(&self.rotated_path);
        fs::rename(&self.path, &self.rotated_path)?;
        self.file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&self.path)?;
        self.size = 0;
        Ok(())
    }
}

impl Write for SizeRotatingWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.size >= self.max_bytes {
            self.rotate()?;
        }
        let n = self.file.write(buf)?;
        self.size += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "chronicle-rotate-test-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn rotates_past_threshold_and_keeps_one_backup() {
        let dir = test_dir("rotate");
        let mut w = SizeRotatingWriter::open_with_max(&dir, "chronicle.log", 16).unwrap();
        w.write_all(b"0123456789").unwrap();
        assert!(!dir.join("chronicle.log.1").exists());
        w.write_all(b"0123456789").unwrap(); // 20 ≥ 16 on next write → rotates
        w.write_all(b"abcd").unwrap();
        assert_eq!(
            fs::read(dir.join("chronicle.log.1")).unwrap(),
            b"01234567890123456789"
        );
        assert_eq!(fs::read(dir.join("chronicle.log")).unwrap(), b"abcd");
        // Another rotation replaces `.1`, never accumulates a `.2`.
        w.write_all(&[b'x'; 16]).unwrap();
        w.write_all(b"tail").unwrap();
        assert_eq!(
            fs::read(dir.join("chronicle.log.1")).unwrap(),
            [b"abcd".as_slice(), &[b'x'; 16]].concat()
        );
        assert_eq!(fs::read(dir.join("chronicle.log")).unwrap(), b"tail");
        assert!(!dir.join("chronicle.log.2").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn reopen_resumes_size_from_disk() {
        let dir = test_dir("resume");
        {
            let mut w = SizeRotatingWriter::open_with_max(&dir, "chronicle.log", 16).unwrap();
            w.write_all(&[b'a'; 16]).unwrap();
        }
        let mut w = SizeRotatingWriter::open_with_max(&dir, "chronicle.log", 16).unwrap();
        w.write_all(b"new").unwrap(); // over threshold from disk → rotates first
        assert_eq!(
            fs::read(dir.join("chronicle.log.1")).unwrap(),
            [b'a'; 16].to_vec()
        );
        assert_eq!(fs::read(dir.join("chronicle.log")).unwrap(), b"new");
        let _ = fs::remove_dir_all(&dir);
    }
}
