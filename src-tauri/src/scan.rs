use md5::{Digest, Md5};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
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
pub struct ArchiveRecord {
    pub relative: String,
    pub size: u64,
    pub key: String,
}

#[derive(Debug, Clone)]
pub struct SourceRecord {
    pub source_root: PathBuf,
    pub source_relative: String,
    pub output_relative: String,
    pub size: u64,
    pub key: String,
    pub archived: bool,
}

#[derive(Debug, Clone)]
pub struct UniqueFile {
    pub path: PathBuf,
    /// 输出目录中的相对路径
    pub relative: PathBuf,
    pub source_root: PathBuf,
    pub source_relative: PathBuf,
    pub size: u64,
    pub key: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScanSummary {
    pub project: String,
    pub archived: u64,
    pub unique: u64,
    pub total: u64,
    pub source_bytes: u64,
    pub archived_bytes: u64,
    pub unique_bytes: u64,
    pub size_gb: String,
    pub unique_files: Vec<String>,
    #[serde(skip)]
    pub unique_items: Vec<UniqueFile>,
    #[serde(skip)]
    pub archive_records: Vec<ArchiveRecord>,
    #[serde(skip)]
    pub source_records: Vec<SourceRecord>,
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
    sources: &[PathBuf],
    output: Option<&Path>,
    mode: ScanMode,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(ScanProgress),
) -> Result<ScanSummary, String> {
    let started = Instant::now();
    let archive = canonical_or_absolute(archive);
    let skip = output.map(canonical_or_absolute);

    if !archive.is_dir() {
        return Err(format!("归档文件夹不存在：{}", archive.display()));
    }
    if sources.is_empty() {
        return Err("至少需要一个原素材文件夹。".to_string());
    }

    let mut source_roots = Vec::new();
    for source in sources {
        let root = canonical_or_absolute(source);
        if !root.is_dir() {
            return Err(format!("原素材文件夹不存在：{}", root.display()));
        }
        source_roots.push(root);
    }

    let mut base_counts: HashMap<String, u32> = HashMap::new();
    let mut source_prefixes = Vec::new();
    for root in &source_roots {
        let base = root
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| "source".to_string());
        let count = base_counts.entry(base.clone()).or_insert(0);
        *count += 1;
        source_prefixes.push(if *count > 1 {
            format!("{}_{}", base, *count)
        } else {
            base
        });
    }

    let multiple_sources = source_roots.len() > 1;
    let project = if source_roots.len() == 1 {
        source_roots[0]
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| "未命名项目".to_string())
    } else {
        archive
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| "多素材项目".to_string())
    };

    let mut errors = Vec::new();
    let archive_files = collect_files(&archive, skip.as_deref(), &mut errors);
    let archive_total = archive_files.len() as u64;
    let mut archive_keys: HashSet<String> = HashSet::new();
    let mut archive_records = Vec::new();

    for (index, entry) in archive_files.iter().enumerate() {
        if cancel.load(Ordering::SeqCst) {
            return Err("已取消".to_string());
        }
        match fingerprint(entry, mode) {
            Ok(key) => {
                archive_keys.insert(key.clone());
                archive_records.push(ArchiveRecord {
                    relative: relative_text(&archive, &entry.path),
                    size: entry.size,
                    key,
                });
            }
            Err(err) => push_error(&mut errors, format!("{}: {}", entry.path.display(), err)),
        }
        if index % 10 == 0 || index + 1 == archive_files.len() {
            let percent = if archive_total == 0 {
                30.0
            } else {
                ((index as f64 + 1.0) / archive_total as f64) * 30.0
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

    let mut source_entries: Vec<(usize, FileEntry)> = Vec::new();
    for (index, root) in source_roots.iter().enumerate() {
        for entry in collect_files(root, skip.as_deref(), &mut errors) {
            source_entries.push((index, entry));
        }
    }

    let source_total = source_entries.len() as u64;
    let source_bytes: u64 = source_entries.iter().map(|(_, entry)| entry.size).sum();
    let mut archived = 0u64;
    let mut unique = 0u64;
    let mut archived_bytes = 0u64;
    let mut unique_bytes = 0u64;
    let mut unique_files = Vec::new();
    let mut unique_items = Vec::new();
    let mut source_records = Vec::new();

    for (index, (source_index, entry)) in source_entries.iter().enumerate() {
        if cancel.load(Ordering::SeqCst) {
            return Err("已取消".to_string());
        }
        let source_root = &source_roots[*source_index];
        match fingerprint(entry, mode) {
            Ok(key) => {
                let source_relative = entry
                    .path
                    .strip_prefix(source_root)
                    .unwrap_or(&entry.path)
                    .to_path_buf();
                let output_relative = if multiple_sources {
                    PathBuf::from(&source_prefixes[*source_index]).join(&source_relative)
                } else {
                    source_relative.clone()
                };
                let is_archived = archive_keys.contains(&key);
                source_records.push(SourceRecord {
                    source_root: source_root.clone(),
                    source_relative: source_relative.to_string_lossy().to_string(),
                    output_relative: output_relative.to_string_lossy().to_string(),
                    size: entry.size,
                    key: key.clone(),
                    archived: is_archived,
                });
                if is_archived {
                    archived += 1;
                    archived_bytes += entry.size;
                } else {
                    unique += 1;
                    unique_bytes += entry.size;
                    unique_files.push(output_relative.to_string_lossy().to_string());
                    unique_items.push(UniqueFile {
                        path: entry.path.clone(),
                        relative: output_relative,
                        source_root: source_root.clone(),
                        source_relative,
                        size: entry.size,
                        key,
                    });
                }
            }
            Err(err) => push_error(&mut errors, format!("{}: {}", entry.path.display(), err)),
        }
        if index % 10 == 0 || index + 1 == source_entries.len() {
            let fraction = if source_total == 0 {
                1.0
            } else {
                (index as f64 + 1.0) / source_total as f64
            };
            progress(ScanProgress {
                stage: "scan-source".to_string(),
                message: format!("比对原素材 {}/{}", index + 1, source_total),
                percent: 30.0 + fraction * 70.0,
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
        project,
        archived,
        unique,
        total: source_total,
        source_bytes,
        archived_bytes,
        unique_bytes,
        size_gb: gb(unique_bytes),
        unique_files,
        unique_items,
        archive_records,
        source_records,
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
    fingerprint_file(&entry.path, entry.size, mode)
}

pub fn fingerprint_file(path: &Path, size: u64, mode: ScanMode) -> Result<String, String> {
    match mode {
        ScanMode::Turbo => {
            let mtime = fs::metadata(path)
                .and_then(|metadata| metadata.modified())
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map(|duration| duration.as_nanos())
                .unwrap_or(0);
            Ok(format!("t:{}:{}", size, mtime))
        }
        ScanMode::Fast => {
            let mut file = File::open(path).map_err(|err| err.to_string())?;
            let head_len = size.min(SAMPLE_SIZE);
            let tail_len = size.min(SAMPLE_SIZE);
            let head = hash_range(&mut file, 0, head_len).map_err(|err| err.to_string())?;
            let tail_start = size.saturating_sub(tail_len);
            let tail =
                hash_range(&mut file, tail_start, tail_len).map_err(|err| err.to_string())?;
            Ok(format!("f:{}:{}:{}", size, head, tail))
        }
        ScanMode::Exact => {
            let mut file = File::open(path).map_err(|err| err.to_string())?;
            let hash = hash_stream(&mut file).map_err(|err| err.to_string())?;
            Ok(format!("e:{}:{}", size, hash))
        }
    }
}

pub fn fingerprint_label(key: &str) -> String {
    if let Some(rest) = key.strip_prefix("f:") {
        let mut parts = rest.splitn(3, ':');
        let size = parts.next().unwrap_or("0");
        let head = parts.next().unwrap_or("");
        let tail = parts.next().unwrap_or("");
        format!("fp=MD5({}|{}|{})", size, head, tail)
    } else if let Some(rest) = key.strip_prefix("e:") {
        let mut parts = rest.splitn(2, ':');
        let _size = parts.next().unwrap_or("0");
        let hash = parts.next().unwrap_or("");
        format!("md5={}", hash)
    } else if let Some(rest) = key.strip_prefix("t:") {
        let mut parts = rest.splitn(2, ':');
        let size = parts.next().unwrap_or("0");
        let mtime = parts.next().unwrap_or("0");
        format!("size={} mtime={}", size, mtime)
    } else {
        key.to_string()
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

fn relative_text(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string()
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
            let summary = scan_dirs(
                &archive,
                std::slice::from_ref(&source),
                Some(&output),
                mode,
                &cancel,
                &mut noop,
            )
            .unwrap();
            assert_eq!(summary.archived, 1);
            assert_eq!(summary.unique, 1);
            assert_eq!(summary.unique_files, vec!["u.bin".to_string()]);
            assert_eq!(summary.total, 2);
        }

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn multiple_sources_are_scanned() {
        let root = test_root("multi");
        let archive = root.join("archive");
        let source1 = root.join("CardA");
        let source2 = root.join("CardB");
        let output = root.join("output");
        fs::create_dir_all(&archive).unwrap();
        fs::create_dir_all(&source1).unwrap();
        fs::create_dir_all(&source2).unwrap();
        fs::create_dir_all(&output).unwrap();
        fs::write(archive.join("archived.bin"), b"archived").unwrap();
        fs::write(source1.join("archived.bin"), b"archived").unwrap();
        fs::write(source2.join("unique-b.bin"), b"unique-b").unwrap();

        let cancel = AtomicBool::new(false);
        let mut noop = |_event: ScanProgress| {};
        let summary = scan_dirs(
            &archive,
            &[source1.clone(), source2.clone()],
            Some(&output),
            ScanMode::Fast,
            &cancel,
            &mut noop,
        )
        .unwrap();

        assert_eq!(summary.total, 2);
        assert_eq!(summary.archived, 1);
        assert_eq!(summary.unique, 1);
        assert_eq!(summary.source_records.len(), 2);
        assert!(summary
            .unique_files
            .iter()
            .any(|path| path.ends_with("unique-b.bin")));

        fs::remove_dir_all(root).unwrap();
    }
}
