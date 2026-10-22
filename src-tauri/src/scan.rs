use md5::{Digest, Md5};
use serde::Serialize;
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Instant, UNIX_EPOCH};
use walkdir::WalkDir;

const SAMPLE_SIZE: u64 = 64 * 1024;
const READ_BUFFER: usize = 1024 * 1024;
const IGNORED_NAMES: &[&str] = &[
    "_archive_dedup_manifest.json",
    "归档完整性校验报告.txt",
    "复查报告.txt",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanMode {
    Turbo,
    Fast,
    Exact,
}

impl ScanMode {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value.to_ascii_lowercase().as_str() {
            "turbo" => Ok(Self::Turbo),
            "fast" => Ok(Self::Fast),
            "exact" => Ok(Self::Exact),
            other => Err(format!("未知比对模式：{}", other)),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Turbo => "极速",
            Self::Fast => "快速",
            Self::Exact => "精确",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ScanProgress {
    pub stage: String,
    pub message: String,
    pub percent: f64,
    pub archived: u64,
    pub unique: u64,
    pub size_gb: String,
    pub current: u64,
    pub total: u64,
}

#[derive(Debug, Clone)]
pub struct UniqueFile {
    pub path: PathBuf,
    pub relative: PathBuf,
    pub size: u64,
    pub key: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScanSummary {
    pub archived: u64,
    pub unique: u64,
    pub total: u64,
    pub archived_bytes: u64,
    pub unique_bytes: u64,
    pub size_gb: String,
    pub unique_files: Vec<String>,
    #[serde(skip)]
    pub unique_items: Vec<UniqueFile>,
    pub errors: Vec<String>,
    pub elapsed_ms: u64,
    pub mode: String,
}

#[derive(Debug, Clone)]
struct FileEntry {
    path: PathBuf,
    size: u64,
}

pub fn scan_dirs(
    archive: &Path,
    source: &Path,
    output: Option<&Path>,
    mode: ScanMode,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(ScanProgress),
) -> Result<ScanSummary, String> {
    let started = Instant::now();
    let archive = canonical_or_absolute(archive);
    let source = canonical_or_absolute(source);
    let skip = output.map(canonical_or_absolute);

    if !archive.is_dir() {
        return Err(format!("归档文件夹不存在：{}", archive.display()));
    }
    if !source.is_dir() {
        return Err(format!("原素材文件夹不存在：{}", source.display()));
    }

    let mut errors = Vec::new();
    let archive_files = collect_files(&archive, skip.as_deref(), &mut errors);
    let archive_total = archive_files.len() as u64;
    let mut archive_keys: HashSet<String> = HashSet::new();

    for (index, entry) in archive_files.iter().enumerate() {
        if cancel.load(Ordering::SeqCst) {
            return Err("已取消".to_string());
        }
        match fingerprint(entry, mode) {
            Ok(key) => {
                archive_keys.insert(key);
            }
            Err(err) => push_error(&mut errors, format!("{}: {}", entry.path.display(), err)),
        }
        if index % 10 == 0 || index + 1 == archive_files.len() {
            let percent = if archive_total == 0 {
                40.0
            } else {
                ((index as f64 + 1.0) / archive_total as f64) * 40.0
            };
            progress(ScanProgress {
                stage: "scan-archive".to_string(),
                message: format!("扫描归档 {}/{}", index + 1, archive_total),
                percent,
                archived: 0,
                unique: 0,
                size_gb: "0.0".to_string(),
                current: index as u64 + 1,
                total: archive_total,
            });
        }
    }

    let source_files = collect_files(&source, skip.as_deref(), &mut errors);
    let source_total = source_files.len() as u64;
    let mut archived = 0u64;
    let mut unique = 0u64;
    let mut archived_bytes = 0u64;
    let mut unique_bytes = 0u64;
    let mut unique_files = Vec::new();
    let mut unique_items = Vec::new();

    for (index, entry) in source_files.iter().enumerate() {
        if cancel.load(Ordering::SeqCst) {
            return Err("已取消".to_string());
        }
        match fingerprint(entry, mode) {
            Ok(key) => {
                if archive_keys.contains(&key) {
                    archived += 1;
                    archived_bytes += entry.size;
                } else {
                    unique += 1;
                    unique_bytes += entry.size;
                    let relative = entry
                        .path
                        .strip_prefix(&source)
                        .unwrap_or(&entry.path)
                        .to_path_buf();
                    unique_files.push(relative.to_string_lossy().to_string());
                    unique_items.push(UniqueFile {
                        path: entry.path.clone(),
                        relative,
                        size: entry.size,
                        key,
                    });
                }
            }
            Err(err) => push_error(&mut errors, format!("{}: {}", entry.path.display(), err)),
        }
        if index % 10 == 0 || index + 1 == source_files.len() {
            let fraction = if source_total == 0 {
                1.0
            } else {
                (index as f64 + 1.0) / source_total as f64
            };
            progress(ScanProgress {
                stage: "scan-source".to_string(),
                message: format!("比对原素材 {}/{}", index + 1, source_total),
                percent: 40.0 + fraction * 60.0,
                archived,
                unique,
                size_gb: gb(unique_bytes),
                current: index as u64 + 1,
                total: source_total,
            });
        }
    }

    if cancel.load(Ordering::SeqCst) {
        return Err("已取消".to_string());
    }

    unique_files.sort();
    let summary = ScanSummary {
        archived,
        unique,
        total: source_total,
        archived_bytes,
        unique_bytes,
        size_gb: gb(unique_bytes),
        unique_files,
        unique_items,
        errors,
        elapsed_ms: started.elapsed().as_millis() as u64,
        mode: mode.label().to_string(),
    };

    progress(ScanProgress {
        stage: "done".to_string(),
        message: format!(
            "扫描完成：已归档 {}，未归档 {}，{} GB",
            summary.archived, summary.unique, summary.size_gb
        ),
        percent: 100.0,
        archived: summary.archived,
        unique: summary.unique,
        size_gb: summary.size_gb.clone(),
        current: summary.total,
        total: summary.total,
    });

    Ok(summary)
}

fn collect_files(root: &Path, skip: Option<&Path>, errors: &mut Vec<String>) -> Vec<FileEntry> {
    let mut files = Vec::new();
    let walker = WalkDir::new(root).follow_links(false).into_iter();

    for entry in walker.filter_entry(|entry| match skip {
        Some(skip) => !is_under(entry.path(), skip),
        None => true,
    }) {
        match entry {
            Ok(entry) => {
                if !entry.file_type().is_file() {
                    continue;
                }
                let name = entry.file_name().to_string_lossy();
                if IGNORED_NAMES
                    .iter()
                    .any(|ignored| ignored.eq_ignore_ascii_case(name.as_ref()))
                {
                    continue;
                }
                match entry.metadata() {
                    Ok(metadata) => files.push(FileEntry {
                        path: entry.path().to_path_buf(),
                        size: metadata.len(),
                    }),
                    Err(err) => push_error(errors, format!("{}: {}", entry.path().display(), err)),
                }
            }
            Err(err) => push_error(errors, format!("遍历失败: {}", err)),
        }
    }

    files
}

fn fingerprint(entry: &FileEntry, mode: ScanMode) -> Result<String, String> {
    match mode {
        ScanMode::Turbo => {
            let mtime = fs::metadata(&entry.path)
                .and_then(|metadata| metadata.modified())
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map(|duration| duration.as_nanos())
                .unwrap_or(0);
            Ok(format!("t:{}:{}", entry.size, mtime))
        }
        ScanMode::Fast => {
            let mut file = File::open(&entry.path).map_err(|err| err.to_string())?;
            let head_len = entry.size.min(SAMPLE_SIZE);
            let tail_len = entry.size.min(SAMPLE_SIZE);
            let head = hash_range(&mut file, 0, head_len).map_err(|err| err.to_string())?;
            let tail_start = entry.size.saturating_sub(tail_len);
            let tail =
                hash_range(&mut file, tail_start, tail_len).map_err(|err| err.to_string())?;
            Ok(format!("f:{}:{}:{}", entry.size, head, tail))
        }
        ScanMode::Exact => {
            let mut file = File::open(&entry.path).map_err(|err| err.to_string())?;
            let hash = hash_stream(&mut file).map_err(|err| err.to_string())?;
            Ok(format!("e:{}:{}", entry.size, hash))
        }
    }
}

fn hash_range(file: &mut File, offset: u64, length: u64) -> io::Result<String> {
    file.seek(SeekFrom::Start(offset))?;
    let mut hasher = Md5::new();
    let mut buffer = vec![0u8; READ_BUFFER];
    let mut remaining = length;

    while remaining > 0 {
        let take = remaining.min(buffer.len() as u64) as usize;
        let read = file.read(&mut buffer[..take])?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        remaining -= read as u64;
    }

    Ok(hex_digest(hasher.finalize().as_slice()))
}

fn hash_stream(file: &mut File) -> io::Result<String> {
    let mut hasher = Md5::new();
    let mut buffer = vec![0u8; READ_BUFFER];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex_digest(hasher.finalize().as_slice()))
}

fn hex_digest(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let value = *byte;
        output.push(HEX[(value >> 4) as usize] as char);
        output.push(HEX[(value & 0x0f) as usize] as char);
    }
    output
}

fn canonical_or_absolute(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| absolute_path(path))
}

fn absolute_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|current| current.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    }
}

fn normalized(path: &Path) -> String {
    let raw = path.to_string_lossy().replace('\\', "/");
    let text = raw.strip_prefix("//?/").unwrap_or(&raw).to_string();
    if cfg!(windows) {
        text.to_lowercase()
    } else {
        text
    }
}

fn is_under(path: &Path, root: &Path) -> bool {
    let path = normalized(path);
    let root = normalized(root);
    let root = root.trim_end_matches('/');
    let prefix = format!("{}/", root);
    path.as_str() == root || path.starts_with(prefix.as_str())
}

fn gb(bytes: u64) -> String {
    format!("{:.1}", bytes as f64 / 1024.0 / 1024.0 / 1024.0)
}

fn push_error(errors: &mut Vec<String>, message: String) {
    if errors.len() < 50 {
        errors.push(message);
    }
}

#[cfg(test)]
mod tests {
    use super::{scan_dirs, ScanMode, ScanProgress};
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicBool;
    use std::time::UNIX_EPOCH;

    fn test_root(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "archive-dedup-{}-{}-{}",
            name,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn mode_parse() {
        assert_eq!(ScanMode::parse("fast").unwrap(), ScanMode::Fast);
        assert_eq!(ScanMode::parse("TURBO").unwrap(), ScanMode::Turbo);
        assert_eq!(ScanMode::parse("exact").unwrap(), ScanMode::Exact);
        assert!(ScanMode::parse("unknown").is_err());
    }

    #[test]
    fn fast_and_exact_scan() {
        let root = test_root("scan");
        let archive = root.join("archive");
        let source = root.join("source");
        let output = source.join("09_04-未使用画面");
        fs::create_dir_all(&archive).unwrap();
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&output).unwrap();

        fs::write(archive.join("a.bin"), b"same").unwrap();
        fs::write(source.join("a.bin"), b"same").unwrap();
        fs::write(source.join("u.bin"), b"unique").unwrap();
        fs::write(output.join("ignored.bin"), b"ignored").unwrap();

        for mode in [ScanMode::Fast, ScanMode::Exact] {
            let cancel = AtomicBool::new(false);
            let mut noop = |_event: ScanProgress| {};
            let summary =
                scan_dirs(&archive, &source, Some(&output), mode, &cancel, &mut noop).unwrap();
            assert_eq!(summary.archived, 1);
            assert_eq!(summary.unique, 1);
            assert_eq!(summary.unique_files, vec!["u.bin".to_string()]);
            assert_eq!(summary.total, 2);
        }

        fs::remove_dir_all(root).unwrap();
    }
}
