use crate::model::{JobConfig, Progress};
use crate::scan::{self, ScanMode, UniqueFile};
use chrono::Local;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use walkdir::WalkDir;

pub const REPORT_NAME: &str = "归档完整性校验报告.txt";
const LEGACY_REPORT_NAMES: &[&str] = &["复查报告.txt"];
const MANIFEST_NAME: &str = "_archive_dedup_manifest.json";
const PROJECT_URL: &str = "https://github.com/GSJNZH/archive-dedup";

#[derive(Debug, Clone)]
pub struct VerifyResult {
    pub ok: bool,
    pub archived: u64,
    pub unique: u64,
    pub checked: u64,
    pub missing: u64,
    pub mismatched: u64,
    pub extra: u64,
    pub total_bytes: u64,
    pub report: String,
    pub errors: Vec<String>,
    pub project: String,
}

pub fn verify_items(
    output: &Path,
    items: &[UniqueFile],
    mode: ScanMode,
    source: &Path,
    copy_mode: &str,
    archived: u64,
) -> Result<VerifyResult, String> {
    if !output.is_dir() {
        return Err(format!("输出目录不存在：{}", output.display()));
    }

    let project = source
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "未命名项目".to_string());

    let mut expected: HashMap<String, &UniqueFile> = HashMap::new();
    for item in items {
        expected.insert(normalize_relative(&item.relative), item);
    }
    let total_bytes = items.iter().map(|item| item.size).sum();

    let mut checked = 0u64;
    let mut missing = 0u64;
    let mut mismatched = 0u64;
    let mut extra = 0u64;
    let mut errors = Vec::new();
    let mut ok_lines = Vec::new();
    let mut missing_lines = Vec::new();
    let mut mismatch_lines = Vec::new();
    let mut extra_lines = Vec::new();

    for entry in WalkDir::new(output).follow_links(false) {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                errors.push(format!("遍历输出目录失败：{}", err));
                continue;
            }
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let relative = match entry.path().strip_prefix(output) {
            Ok(relative) => normalize_relative(relative),
            Err(_) => continue,
        };
        let name = entry.file_name().to_string_lossy().to_string();
        if name.eq_ignore_ascii_case(REPORT_NAME)
            || name.eq_ignore_ascii_case(MANIFEST_NAME)
            || LEGACY_REPORT_NAMES
                .iter()
                .any(|legacy| name.eq_ignore_ascii_case(legacy))
        {
            continue;
        }

        let Some(item) = expected.remove(&relative) else {
            extra += 1;
            if extra_lines.len() < 500 {
                let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                extra_lines.push(format!("[多余] {} | {}", relative, format_size(size)));
            }
            continue;
        };

        checked += 1;
        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
        match scan::fingerprint_file(entry.path(), size, mode) {
            Ok(actual) if actual == item.key => {
                ok_lines.push(format!(
                    "[OK] {} | {} | {}",
                    relative,
                    format_size(size),
                    scan::fingerprint_label(&actual)
                ));
            }
            Ok(actual) => {
                mismatched += 1;
                mismatch_lines.push(format!(
                    "[不一致] {} | {} | 期望 {} | 实际 {}",
                    relative,
                    format_size(size),
                    scan::fingerprint_label(&item.key),
                    scan::fingerprint_label(&actual)
                ));
            }
            Err(err) => {
                mismatched += 1;
                errors.push(format!("{}: {}", relative, err));
                mismatch_lines.push(format!("[不一致] {} | 无法读取：{}", relative, err));
            }
        }
    }

    let mut missing_items: Vec<&UniqueFile> = expected.values().copied().collect();
    missing_items.sort_by(|a, b| a.relative.cmp(&b.relative));
    for item in missing_items {
        missing += 1;
        missing_lines.push(format!(
            "[缺失] {} | {} | {}",
            normalize_relative(&item.relative),
            format_size(item.size),
            scan::fingerprint_label(&item.key)
        ));
    }

    let ok = missing == 0 && mismatched == 0;
    let mut details = Vec::new();
    details.extend(ok_lines);
    details.extend(missing_lines);
    details.extend(mismatch_lines);
    details.extend(extra_lines);
    if details.is_empty() {
        details.push("[空] 输出目录没有文件。".to_string());
    }
    let details_text = details.join("\n");
    let collection = sha256_hex(details_text.as_bytes());

    let status = if ok { "通过" } else { "未通过" };
    let mode_label = mode.label();
    let copy_label = if copy_mode == "move" {
        "同盘移动"
    } else {
        "复制"
    };
    let report_text = format!(
        "\u{feff}============================================================\n\
素材归档去重工具 · 归档完整性校验报告\n\
============================================================\n\
项目名称: {}\n\
生成时间: {}\n\
软件版本: v{}\n\
项目地址: {}\n\
比对模式: {}\n\
处理方式: {}\n\
原素材目录: {}\n\
输出目录: {}\n\
------------------------------------------------------------\n\
校验结果: {}\n\
归档命中: {}\n\
未归档: {}\n\
校验文件: {}\n\
缺失: {}\n\
不一致: {}\n\
多余: {}\n\
总大小: {}\n\
------------------------------------------------------------\n\
文件明细:\n{}\n\
------------------------------------------------------------\n\
集合 SHA256: {}\n\
报告结束\n",
        project,
        Local::now().format("%Y-%m-%d %H:%M:%S"),
        env!("CARGO_PKG_VERSION"),
        PROJECT_URL,
        mode_label,
        copy_label,
        source.display(),
        output.display(),
        status,
        archived,
        items.len(),
        checked,
        missing,
        mismatched,
        extra,
        format_size(total_bytes),
        details_text,
        collection
    );

    fs::create_dir_all(output).map_err(|err| format!("无法创建输出目录：{}", err))?;
    let report_path = output.join(REPORT_NAME);
    fs::write(&report_path, report_text.as_bytes())
        .map_err(|err| format!("无法写入校验报告：{}", err))?;
    for legacy in LEGACY_REPORT_NAMES {
        let path = output.join(legacy);
        if path.is_file() {
            let _ = fs::remove_file(path);
        }
    }

    Ok(VerifyResult {
        ok,
        archived,
        unique: items.len() as u64,
        checked,
        missing,
        mismatched,
        extra,
        total_bytes,
        report: report_path.display().to_string(),
        errors,
        project,
    })
}

pub fn verify_only(
    config: &JobConfig,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(Progress),
) -> Result<VerifyResult, String> {
    let mode = ScanMode::parse(&config.mode)?;
    let archive = Path::new(config.archive.trim());
    let source = Path::new(config.source.trim());
    let output = Path::new(config.output.trim());
    if archive.as_os_str().is_empty()
        || source.as_os_str().is_empty()
        || output.as_os_str().is_empty()
    {
        return Err("归档目录、原素材目录和输出目录都不能为空。".to_string());
    }

    let mut scan_progress = |event: scan::ScanProgress| {
        progress(Progress {
            stage: event.stage,
            message: event.message,
            percent: event.percent * 0.5,
            archived: event.archived,
            unique: event.unique,
            size_gb: event.size_gb,
            copied: 0,
            failed: 0,
        });
    };
    let summary = scan::scan_dirs(
        archive,
        source,
        Some(output),
        mode,
        cancel,
        &mut scan_progress,
    )?;
    if cancel.load(Ordering::SeqCst) {
        return Err("已取消".to_string());
    }

    progress(Progress {
        stage: "verify".to_string(),
        message: format!("校验输出目录：{} 个文件", summary.unique),
        percent: 55.0,
        archived: summary.archived,
        unique: summary.unique,
        size_gb: summary.size_gb.clone(),
        copied: 0,
        failed: 0,
    });

    let result = verify_items(
        output,
        &summary.unique_items,
        mode,
        source,
        &config.copy_mode,
        summary.archived,
    )?;
    progress(Progress {
        stage: "done".to_string(),
        message: format!(
            "校验{}：缺失 {}，不一致 {}，多余 {}",
            if result.ok { "通过" } else { "未通过" },
            result.missing,
            result.mismatched,
            result.extra
        ),
        percent: 100.0,
        archived: summary.archived,
        unique: summary.unique,
        size_gb: summary.size_gb.clone(),
        copied: 0,
        failed: 0,
    });
    Ok(result)
}

fn normalize_relative(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    if cfg!(windows) {
        text.to_lowercase()
    } else {
        text
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut output = String::with_capacity(digest.len() * 2);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for byte in digest.iter() {
        let value = *byte;
        output.push(HEX[(value >> 4) as usize] as char);
        output.push(HEX[(value & 0x0f) as usize] as char);
    }
    output
}

fn format_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let value = bytes as f64;
    if value >= GB {
        format!("{:.1} GB", value / GB)
    } else if value >= MB {
        format!("{:.1} MB", value / MB)
    } else if value >= KB {
        format!("{:.1} KB", value / KB)
    } else {
        format!("{} B", bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_root(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "archive-dedup-verify-{}-{}-{}",
            name,
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn verify_ok_and_missing() {
        let root = test_root("verify");
        let output = root.join("output");
        fs::create_dir_all(&output).unwrap();
        fs::write(output.join("a.txt"), b"abc").unwrap();
        let key = scan::fingerprint_file(&output.join("a.txt"), 3, ScanMode::Fast).unwrap();
        let items = vec![UniqueFile {
            path: output.join("a.txt"),
            relative: PathBuf::from("a.txt"),
            size: 3,
            key: key.clone(),
        }];
        let result = verify_items(&output, &items, ScanMode::Fast, &root, "copy", 0).unwrap();
        assert!(result.ok);
        assert_eq!(result.checked, 1);
        assert_eq!(result.missing, 0);
        assert!(Path::new(&result.report).is_file());

        let missing_items = vec![UniqueFile {
            path: output.join("missing.txt"),
            relative: PathBuf::from("missing.txt"),
            size: 3,
            key,
        }];
        let result =
            verify_items(&output, &missing_items, ScanMode::Fast, &root, "copy", 0).unwrap();
        assert!(!result.ok);
        assert_eq!(result.missing, 1);
        fs::remove_dir_all(root).unwrap();
    }
}
