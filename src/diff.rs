//! Unified-diff parsing and chunking.

use std::path::Path;
use std::process::Command;

use crate::{Error, Result};

/// The patch for a single file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    pub path: String,
    pub status: Status,
    pub lines: Vec<String>,
    pub binary: bool,
}

/// How the file was changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Added,
    Deleted,
    Renamed,
    Modified,
}

impl FileDiff {
    fn new(path: String, first_line: String) -> Self {
        Self {
            path,
            status: Status::Modified,
            lines: vec![first_line],
            binary: false,
        }
    }

    /// The patch text.
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    /// Added lines, excluding the `+++` header.
    pub fn added(&self) -> usize {
        self.lines
            .iter()
            .filter(|line| line.starts_with('+') && !line.starts_with("+++"))
            .count()
    }

    /// Removed lines, excluding the `---` header.
    pub fn removed(&self) -> usize {
        self.lines
            .iter()
            .filter(|line| line.starts_with('-') && !line.starts_with("---"))
            .count()
    }

    /// Split the patch body into hunks, each prefixed with the file header.
    pub fn hunks(&self) -> Vec<String> {
        let mut header: Vec<&str> = Vec::new();
        let mut chunks: Vec<Vec<&str>> = Vec::new();
        for line in &self.lines {
            if line.starts_with("@@ ") {
                chunks.push(vec![line]);
            } else if let Some(last) = chunks.last_mut() {
                last.push(line);
            } else {
                header.push(line);
            }
        }
        if chunks.is_empty() {
            return vec![self.text()];
        }
        let prefix = header.join("\n");
        chunks
            .into_iter()
            .map(|chunk| {
                format!("{prefix}\n{}", chunk.join("\n"))
                    .trim_matches('\n')
                    .to_string()
            })
            .collect()
    }
}

/// A slice of the diff small enough to send as model state in one request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    pub files: Vec<String>,
    pub text: String,
}

/// Parse a unified diff (`git diff` / GitHub `.diff`) into per-file patches.
pub fn parse(diff_text: &str) -> Vec<FileDiff> {
    let mut files: Vec<FileDiff> = Vec::new();

    for line in diff_text.lines() {
        if let Some(path) = header_path(line) {
            files.push(FileDiff::new(path, line.to_string()));
            continue;
        }
        let Some(current) = files.last_mut() else {
            continue;
        };
        current.lines.push(line.to_string());

        if line.starts_with("new file mode") {
            current.status = Status::Added;
        } else if line.starts_with("deleted file mode") {
            current.status = Status::Deleted;
        } else if let Some(path) = line.strip_prefix("rename to ") {
            current.status = Status::Renamed;
            current.path = path.trim().to_string();
        } else if line.starts_with("Binary files ") || line.starts_with("GIT binary patch") {
            current.binary = true;
        } else if let Some(path) = line.strip_prefix("+++ ") {
            if let Some(path) = strip_prefix_marker(path, "b/") {
                current.path = path;
            }
        } else if current.status == Status::Deleted {
            if let Some(path) = line.strip_prefix("--- ") {
                if let Some(path) = strip_prefix_marker(path, "a/") {
                    current.path = path;
                }
            }
        }
    }

    files.retain(|file| !file.path.is_empty());
    files
}

/// `diff --git a/<path> b/<path>` -> the b-side path.
fn header_path(line: &str) -> Option<String> {
    let rest = line.strip_prefix("diff --git ")?;
    let marker = rest.find(" b/")?;
    Some(rest[marker + 3..].to_string())
}

fn strip_prefix_marker(path: &str, marker: &str) -> Option<String> {
    let path = path.trim();
    if path == "/dev/null" {
        return None;
    }
    Some(path.strip_prefix(marker).unwrap_or(path).to_string())
}

/// Read a diff from a file, stdin (`-`) or `git diff base...head`.
pub fn read(
    diff_file: Option<&str>,
    base: Option<&str>,
    head: Option<&str>,
    repo_dir: &Path,
) -> Result<String> {
    if let Some(diff_file) = diff_file {
        if diff_file == "-" {
            let mut buffer = String::new();
            use std::io::Read;
            std::io::stdin()
                .read_to_string(&mut buffer)
                .map_err(|err| Error::Diff(format!("cannot read stdin: {err}")))?;
            return Ok(buffer);
        }
        let path = Path::new(diff_file);
        if !path.is_file() {
            return Err(Error::Diff(format!("diff file not found: {diff_file}")));
        }
        return std::fs::read_to_string(path)
            .map_err(|err| Error::Diff(format!("cannot read {diff_file}: {err}")));
    }

    let Some(base) = base else {
        return Err(Error::Diff(
            "provide --diff-file or --base (optionally with --head)".to_string(),
        ));
    };
    let revision = match head {
        Some(head) => format!("{base}...{head}"),
        None => base.to_string(),
    };
    let output = Command::new("git")
        .args(["diff", "--no-color", "--find-renames", &revision])
        .current_dir(repo_dir)
        .output()
        .map_err(|err| Error::Diff(format!("cannot run git: {err}")))?;
    if !output.status.success() {
        return Err(Error::Diff(format!(
            "`git diff {revision}` failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Pack file patches into chunks of at most `max_chars` characters.
///
/// A single file larger than the budget is split by hunk; a single hunk that is
/// still too large is truncated, with a marker so the model knows.
pub fn chunk_files(files: &[FileDiff], max_chars: usize) -> Vec<Chunk> {
    let mut chunks: Vec<Chunk> = Vec::new();
    let mut parts: Vec<String> = Vec::new();
    let mut names: Vec<String> = Vec::new();
    let mut length = 0usize;

    for file in files {
        let text = file.text();
        let pieces = if text.len() <= max_chars {
            vec![text]
        } else {
            split_oversized(file, max_chars)
        };

        for piece in pieces {
            if length > 0 && length + piece.len() > max_chars {
                chunks.push(Chunk {
                    files: std::mem::take(&mut names),
                    text: std::mem::take(&mut parts).join("\n"),
                });
                length = 0;
            }
            length += piece.len() + 1;
            parts.push(piece);
            if !names.contains(&file.path) {
                names.push(file.path.clone());
            }
        }
    }

    if !parts.is_empty() {
        chunks.push(Chunk {
            files: names,
            text: parts.join("\n"),
        });
    }
    chunks
}

fn split_oversized(file: &FileDiff, max_chars: usize) -> Vec<String> {
    file.hunks()
        .into_iter()
        .map(|hunk| {
            if hunk.len() <= max_chars {
                return hunk;
            }
            let marker = format!(
                "\n[review-gate: hunk truncated, {} characters omitted]",
                hunk.len() - max_chars
            );
            let keep = max_chars.saturating_sub(marker.len());
            let mut boundary = keep;
            while boundary > 0 && !hunk.is_char_boundary(boundary) {
                boundary -= 1;
            }
            format!("{}{marker}", &hunk[..boundary])
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = include_str!("../tests/fixtures/sample.diff");

    #[test]
    fn parses_files_and_counts() {
        let files = parse(SAMPLE);
        let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "services/orders/handler.py",
                "infra/stack.ts",
                "assets/logo.png"
            ]
        );
        assert_eq!(files[0].added(), 1);
        assert_eq!(files[0].removed(), 2);
        assert!(files[2].binary);
        assert_eq!(files[0].status, Status::Modified);
    }

    #[test]
    fn parses_added_deleted_renamed() {
        let diff = "diff --git a/new.py b/new.py\n\
                    new file mode 100644\n--- /dev/null\n+++ b/new.py\n@@ -0,0 +1 @@\n+x\n\
                    diff --git a/gone.py b/gone.py\n\
                    deleted file mode 100644\n--- a/gone.py\n+++ /dev/null\n@@ -1 +0,0 @@\n-x\n\
                    diff --git a/old.py b/moved.py\nsimilarity index 100%\n\
                    rename from old.py\nrename to moved.py\n";
        let files = parse(diff);
        let seen: Vec<(&str, Status)> = files.iter().map(|f| (f.path.as_str(), f.status)).collect();
        assert_eq!(
            seen,
            [
                ("new.py", Status::Added),
                ("gone.py", Status::Deleted),
                ("moved.py", Status::Renamed),
            ]
        );
    }

    #[test]
    fn ignores_noise() {
        assert!(parse("not a diff at all\n").is_empty());
    }

    #[test]
    fn packs_and_splits_chunks() {
        let files = parse(SAMPLE);
        let packed = chunk_files(&files, 10_000);
        assert_eq!(packed.len(), 1);
        assert_eq!(packed[0].files.len(), 3);

        let small = chunk_files(&files, files[0].text().len() + 1);
        assert!(small.len() > 1);
        assert_eq!(small[0].files, ["services/orders/handler.py"]);
    }

    #[test]
    fn oversized_file_is_split_by_hunk_and_truncated() {
        let body: Vec<String> = (0..500).map(|i| format!("+line {i}")).collect();
        let file = FileDiff {
            path: "big.py".to_string(),
            status: Status::Modified,
            binary: false,
            lines: vec![
                "diff --git a/big.py b/big.py".to_string(),
                "@@ -1 +1 @@".to_string(),
                body.join("\n"),
                "@@ -2 +2 @@".to_string(),
                "+tail".to_string(),
            ],
        };
        let chunks = chunk_files(&[file], 600);
        assert!(chunks.len() >= 2);
        assert!(chunks.iter().any(|chunk| chunk.text.contains("truncated")));
        assert!(chunks.iter().all(|chunk| chunk.text.len() <= 600));
    }

    #[test]
    fn read_errors_are_clear() {
        let dir = Path::new(".");
        let err = read(Some("does-not-exist.diff"), None, None, dir).unwrap_err();
        assert!(err.to_string().contains("diff file not found"));
        let err = read(None, None, None, dir).unwrap_err();
        assert!(err.to_string().contains("--diff-file or --base"));
    }
}
