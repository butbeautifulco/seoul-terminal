use std::collections::HashMap;

use crate::git::types::{ChangedFile, CommitInfo, FileStatus};

/// Parse `git status --porcelain=v2 -z` output.
///
/// Returns (staged, unstaged, untracked) file lists and current branch name.
pub fn parse_porcelain_v2(
    output: &[u8],
) -> (String, Vec<ChangedFile>, Vec<ChangedFile>, Vec<ChangedFile>) {
    let mut staged = Vec::new();
    let mut unstaged = Vec::new();
    let mut untracked = Vec::new();
    let mut branch = String::from("HEAD");

    // Split on NUL bytes. The -z flag makes entries NUL-terminated.
    let entries: Vec<&[u8]> = split_nul(output);
    let mut i = 0;

    while i < entries.len() {
        let entry = entries[i];
        let line = String::from_utf8_lossy(entry);

        if let Some(rest) = line.strip_prefix("# branch.head ") {
            branch = rest.to_string();
            i += 1;
            continue;
        }

        // Skip other header lines
        if line.starts_with('#') {
            i += 1;
            continue;
        }

        // Untracked: "? <path>"
        if let Some(rest) = line.strip_prefix("? ") {
            let path = rest.to_string();
            untracked.push(ChangedFile {
                path,
                old_path: None,
                status: FileStatus::Untracked,
                additions: 0,
                deletions: 0,
            });
            i += 1;
            continue;
        }

        // Ordinary changed entry: "1 XY ..."
        if line.starts_with("1 ") {
            if let Some((xy, path)) = parse_ordinary_entry(&line) {
                add_staged_unstaged(xy, &path, None, &mut staged, &mut unstaged);
            }
            i += 1;
            continue;
        }

        // Renamed/copied entry: "2 XY ..." followed by the original path
        if line.starts_with("2 ") {
            if let Some((xy, path)) = parse_rename_entry(&line) {
                // Next NUL-separated entry is the original path
                let old_path = if i + 1 < entries.len() {
                    let op = String::from_utf8_lossy(entries[i + 1]).to_string();
                    i += 1; // consume the extra entry
                    Some(op)
                } else {
                    None
                };
                add_staged_unstaged(xy, &path, old_path, &mut staged, &mut unstaged);
            }
            i += 1;
            continue;
        }

        // Unmerged entry: "u XY ..."
        if line.starts_with("u ") {
            if let Some((xy, path)) = parse_ordinary_entry(&line) {
                add_staged_unstaged(xy, &path, None, &mut staged, &mut unstaged);
            }
            i += 1;
            continue;
        }

        i += 1;
    }

    (branch, staged, unstaged, untracked)
}

fn split_nul(data: &[u8]) -> Vec<&[u8]> {
    let mut result = Vec::new();
    let mut start = 0;
    for (i, &b) in data.iter().enumerate() {
        if b == 0 {
            if start < i {
                result.push(&data[start..i]);
            }
            start = i + 1;
        }
    }
    if start < data.len() {
        result.push(&data[start..]);
    }
    result
}

/// Parse ordinary entry (type "1" or "u"): "1 XY sub mH mI mW hH hI path"
fn parse_ordinary_entry(line: &str) -> Option<([u8; 2], String)> {
    let parts: Vec<&str> = line.splitn(9, ' ').collect();
    if parts.len() < 9 {
        return None;
    }
    let xy = parts[1].as_bytes();
    if xy.len() < 2 {
        return None;
    }
    Some(([xy[0], xy[1]], parts[8].to_string()))
}

/// Parse rename/copy entry (type "2"): "2 XY sub mH mI mW hH hI Xscore path"
fn parse_rename_entry(line: &str) -> Option<([u8; 2], String)> {
    let parts: Vec<&str> = line.splitn(10, ' ').collect();
    if parts.len() < 10 {
        return None;
    }
    let xy = parts[1].as_bytes();
    if xy.len() < 2 {
        return None;
    }
    Some(([xy[0], xy[1]], parts[9].to_string()))
}

fn add_staged_unstaged(
    xy: [u8; 2],
    path: &str,
    old_path: Option<String>,
    staged: &mut Vec<ChangedFile>,
    unstaged: &mut Vec<ChangedFile>,
) {
    let index = xy[0];
    let worktree = xy[1];

    // Index (staged) changes
    if index != b'.' && index != b'?' {
        staged.push(ChangedFile {
            path: path.to_string(),
            old_path,
            status: map_status_code(index),
            additions: 0,
            deletions: 0,
        });
    }

    // Worktree (unstaged) changes
    if worktree != b'.' && worktree != b'?' {
        unstaged.push(ChangedFile {
            path: path.to_string(),
            old_path: None,
            status: map_status_code(worktree),
            additions: 0,
            deletions: 0,
        });
    }
}

fn map_status_code(code: u8) -> FileStatus {
    match code {
        b'A' => FileStatus::Added,
        b'D' => FileStatus::Deleted,
        b'R' => FileStatus::Renamed,
        b'C' => FileStatus::Copied,
        b'?' => FileStatus::Untracked,
        _ => FileStatus::Modified, // M, T, U, etc.
    }
}

/// Parse `git log --format=%H|%h|%s|%an|%aI` output.
pub fn parse_log_output(output: &str) -> Vec<CommitInfo> {
    if output.trim().is_empty() {
        return Vec::new();
    }

    let mut commits = Vec::new();

    for line in output.trim().lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        // Format: hash|shortHash|message|author|date
        // Message may contain '|', so split carefully.
        let parts: Vec<&str> = line.splitn(5, '|').collect();
        if parts.len() < 5 {
            continue;
        }

        let hash = parts[0].to_string();
        let short_hash = parts[1].to_string();
        // parts[2] could contain '|' if the message does, but since we splitn(5),
        // we need to re-parse: message is everything between part[1] and part[-2]
        // Actually with splitn(5), parts[2] is message, parts[3] is author, parts[4] is date
        let message = parts[2].to_string();
        let author = parts[3].to_string();
        let date = parts[4].to_string();

        commits.push(CommitInfo {
            hash,
            short_hash,
            message,
            author,
            date,
            files: Vec::new(),
        });
    }

    commits
}

/// Parse `git diff --numstat` output into a map of path -> (additions, deletions).
pub fn parse_numstat(output: &str) -> HashMap<String, (u32, u32)> {
    let mut stats = HashMap::new();

    for line in output.trim().lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        // Format: additions\tdeletions\tfilepath
        // For renames: additions\tdeletions\toldpath => newpath
        // For binary: -\t-\tfilepath
        let parts: Vec<&str> = line.splitn(3, '\t').collect();
        if parts.len() < 3 {
            continue;
        }

        let additions = parts[0].parse::<u32>().unwrap_or(0); // '-' for binary -> 0
        let deletions = parts[1].parse::<u32>().unwrap_or(0);
        let raw_path = parts[2];

        // Handle rename format: "oldpath => newpath" or "{prefix/oldname => newname}"
        if let Some(idx) = raw_path.find(" => ") {
            let old_path = &raw_path[..idx];
            let new_path = &raw_path[idx + 4..];
            stats.insert(new_path.to_string(), (additions, deletions));
            stats.insert(old_path.to_string(), (additions, deletions));
        } else {
            stats.insert(raw_path.to_string(), (additions, deletions));
        }
    }

    stats
}

/// Parse `git diff --name-status` output.
pub fn parse_name_status(output: &str) -> Vec<ChangedFile> {
    let mut files = Vec::new();

    for line in output.trim().lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        // Format: status\tfilepath (or status\toldpath\tnewpath for renames)
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.is_empty() {
            continue;
        }

        let status_code = parts[0];
        if status_code.is_empty() {
            continue;
        }

        let first_char = status_code.as_bytes()[0];
        let is_rename_or_copy = first_char == b'R' || first_char == b'C';

        let path = if is_rename_or_copy {
            parts.get(2).copied()
        } else {
            parts.get(1).copied()
        };
        let old_path = if is_rename_or_copy {
            parts.get(1).map(|s| s.to_string())
        } else {
            None
        };

        let Some(path) = path else { continue };

        let status = match first_char {
            b'A' => FileStatus::Added,
            b'D' => FileStatus::Deleted,
            b'R' => FileStatus::Renamed,
            b'C' => FileStatus::Copied,
            _ => FileStatus::Modified,
        };

        files.push(ChangedFile {
            path: path.to_string(),
            old_path,
            status,
            additions: 0,
            deletions: 0,
        });
    }

    files
}

/// Apply numstat data to a list of changed files.
pub fn apply_numstat(files: &mut [ChangedFile], numstat: &HashMap<String, (u32, u32)>) {
    for file in files.iter_mut() {
        if let Some(&(additions, deletions)) = numstat.get(&file.path) {
            file.additions = additions;
            file.deletions = deletions;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_porcelain_v2_basic() {
        // Simulate: one staged modified, one unstaged modified, one untracked
        let output = b"# branch.head main\0\
            1 M. N... 100644 100644 100644 abc123 def456 staged.rs\0\
            1 .M N... 100644 100644 100644 abc123 def456 unstaged.rs\0\
            ? untracked.txt\0";

        let (branch, staged, unstaged, untracked) = parse_porcelain_v2(output);
        assert_eq!(branch, "main");
        assert_eq!(staged.len(), 1);
        assert_eq!(staged[0].path, "staged.rs");
        assert_eq!(staged[0].status, FileStatus::Modified);
        assert_eq!(unstaged.len(), 1);
        assert_eq!(unstaged[0].path, "unstaged.rs");
        assert_eq!(untracked.len(), 1);
        assert_eq!(untracked[0].path, "untracked.txt");
    }

    #[test]
    fn test_parse_porcelain_v2_added() {
        let output = b"# branch.head feature\0\
            1 A. N... 000000 100644 100644 0000000 abc1234 new_file.rs\0";

        let (branch, staged, unstaged, untracked) = parse_porcelain_v2(output);
        assert_eq!(branch, "feature");
        assert_eq!(staged.len(), 1);
        assert_eq!(staged[0].status, FileStatus::Added);
        assert!(unstaged.is_empty());
        assert!(untracked.is_empty());
    }

    #[test]
    fn test_parse_log_output() {
        let output = "abc1234|abc1234|Initial commit|Author|2024-01-01T00:00:00+00:00\n\
                       def5678|def5678|Fix bug|Author2|2024-01-02T00:00:00+00:00\n";

        let commits = parse_log_output(output);
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].hash, "abc1234");
        assert_eq!(commits[0].message, "Initial commit");
        assert_eq!(commits[1].message, "Fix bug");
    }

    #[test]
    fn test_parse_log_output_empty() {
        assert!(parse_log_output("").is_empty());
        assert!(parse_log_output("  \n  ").is_empty());
    }

    #[test]
    fn test_parse_numstat() {
        let output = "10\t5\tsrc/main.rs\n\
                       3\t0\tREADME.md\n\
                       -\t-\tbinary.png\n";

        let stats = parse_numstat(output);
        assert_eq!(stats.get("src/main.rs"), Some(&(10, 5)));
        assert_eq!(stats.get("README.md"), Some(&(3, 0)));
        assert_eq!(stats.get("binary.png"), Some(&(0, 0)));
    }

    #[test]
    fn test_parse_numstat_rename() {
        let output = "5\t2\told.rs => new.rs\n";
        let stats = parse_numstat(output);
        assert_eq!(stats.get("new.rs"), Some(&(5, 2)));
        assert_eq!(stats.get("old.rs"), Some(&(5, 2)));
    }

    #[test]
    fn test_parse_name_status() {
        let output = "M\tsrc/main.rs\n\
                       A\tnew_file.rs\n\
                       D\tremoved.rs\n\
                       R100\told_name.rs\tnew_name.rs\n";

        let files = parse_name_status(output);
        assert_eq!(files.len(), 4);
        assert_eq!(files[0].status, FileStatus::Modified);
        assert_eq!(files[0].path, "src/main.rs");
        assert_eq!(files[1].status, FileStatus::Added);
        assert_eq!(files[2].status, FileStatus::Deleted);
        assert_eq!(files[3].status, FileStatus::Renamed);
        assert_eq!(files[3].path, "new_name.rs");
        assert_eq!(files[3].old_path.as_deref(), Some("old_name.rs"));
    }

    #[test]
    fn test_apply_numstat() {
        let numstat = HashMap::from([
            ("src/main.rs".to_string(), (10, 5)),
            ("lib.rs".to_string(), (3, 1)),
        ]);
        let mut files = vec![
            ChangedFile {
                path: "src/main.rs".to_string(),
                old_path: None,
                status: FileStatus::Modified,
                additions: 0,
                deletions: 0,
            },
            ChangedFile {
                path: "unknown.rs".to_string(),
                old_path: None,
                status: FileStatus::Added,
                additions: 0,
                deletions: 0,
            },
        ];

        apply_numstat(&mut files, &numstat);
        assert_eq!(files[0].additions, 10);
        assert_eq!(files[0].deletions, 5);
        assert_eq!(files[1].additions, 0); // not in numstat
    }
}
