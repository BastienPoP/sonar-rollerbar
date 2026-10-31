//! A small log file that can't grow: it is rotated at `MAX_BYTES`, keeping one previous file.
//!
//! It records program events only. The hook has no access to it, so it never holds key data.
//! Tests run anywhere with `rustc --edition 2024 --test src/logfile.rs`.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

const MAX_BYTES: u64 = 512 * 1024;
const FILE_NAME: &str = "sonar-rollerbar.log";
const PREVIOUS_FILE_NAME: &str = "sonar-rollerbar.log.1";

pub fn append(directory: &Path, line: &str) -> io::Result<()> {
    fs::create_dir_all(directory)?;
    let path = directory.join(FILE_NAME);
    if fs::metadata(&path).is_ok_and(|file| file.len() >= MAX_BYTES) {
        fs::rename(&path, directory.join(PREVIOUS_FILE_NAME))?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(file, "{line}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stays_bounded_and_keeps_one_previous_file() {
        let directory = std::env::temp_dir().join(format!("logfile-{}", std::process::id()));
        let line = "x".repeat(1024);
        for _ in 0..5000 {
            append(&directory, &line).unwrap();
        }

        let files: Vec<_> = fs::read_dir(&directory).unwrap().map(Result::unwrap).collect();
        let total: u64 = files.iter().map(|file| file.metadata().unwrap().len()).sum();
        fs::remove_dir_all(&directory).unwrap();

        assert_eq!(files.len(), 2);
        assert!(total <= 2 * MAX_BYTES + 2048, "{total} bytes");
    }
}
