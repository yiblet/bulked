//! Physical filesystem implementation
//!
//! This module provides PhysicalFS, which uses the real OS filesystem.
//! This is the production adapter used by the CLI.

use super::FileSystem;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;

/// Physical filesystem adapter
///
/// This adapter uses std::fs to interact with the real filesystem.
/// It's used in production but never in tests (tests use MemoryFS).
#[derive(Debug, Clone, Copy, Default)]
pub struct PhysicalFS;

impl PhysicalFS {
    /// Create a new PhysicalFS instance
    pub fn new() -> Self {
        Self
    }
}

impl FileSystem for PhysicalFS {
    fn read_to_string(&self, path: &Path) -> Result<String, String> {
        fs::read_to_string(path).map_err(|e| format!("Failed to read {}: {}", path.display(), e))
    }

    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn is_file(&self, path: &Path) -> bool {
        path.is_file()
    }

    fn is_binary(&self, path: &Path) -> bool {
        // Read first 8KB and check for null bytes or invalid UTF-8
        // This matches common approaches used by grep tools
        match fs::read(path) {
            Ok(bytes) => {
                let check_len = bytes.len().min(8192);
                let sample = &bytes[..check_len];

                // Check for null bytes
                if sample.contains(&0) {
                    return true;
                }

                // Check if valid UTF-8
                String::from_utf8(sample.to_vec()).is_err()
            }
            Err(_) => false, // If can't read, assume not binary
        }
    }

    fn read_line_at(&self, path: &Path, line_number: usize) -> Result<String, String> {
        if line_number == 0 {
            return Err("Line numbers are 1-indexed".to_string());
        }

        let file =
            fs::File::open(path).map_err(|e| format!("Failed to open {}: {}", path.display(), e))?;
        let reader = BufReader::new(file);

        let mut lines = reader.lines();
        for _ in 1..line_number {
            if lines.next().is_none() {
                return Err(format!("Line {} out of range", line_number));
            }
        }

        lines
            .next()
            .ok_or_else(|| format!("Line {} out of range", line_number))?
            .map_err(|e| format!("Failed to read line: {}", e))
    }

    fn read_line_range(
        &self,
        path: &Path,
        start_line: usize,
        end_line: usize,
    ) -> Result<Vec<String>, String> {
        if start_line == 0 || end_line == 0 {
            return Err("Line numbers are 1-indexed".to_string());
        }

        if start_line > end_line {
            return Err(format!(
                "Start line {} is greater than end line {}",
                start_line, end_line
            ));
        }

        let file =
            fs::File::open(path).map_err(|e| format!("Failed to open {}: {}", path.display(), e))?;
        let reader = BufReader::new(file);

        let mut result = Vec::new();
        for (idx, line_result) in reader.lines().enumerate() {
            let line_num = idx + 1;

            if line_num < start_line {
                continue;
            }

            if line_num > end_line {
                break;
            }

            let line = line_result.map_err(|e| format!("Failed to read line {}: {}", line_num, e))?;
            result.push(line);
        }

        if result.is_empty() && start_line > 1 {
            return Err(format!("Start line {} out of range", start_line));
        }

        Ok(result)
    }
}

// Note: We don't add #[cfg(test)] tests for PhysicalFS here because
// testing it would require touching the real filesystem, which violates
// our hermetic testing principle. PhysicalFS is simple enough that we
// trust std::fs, and we test the FileSystem trait contract with MemoryFS.
