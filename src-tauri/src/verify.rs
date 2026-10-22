use crate::model::{JobConfig, Progress};
use crate::scan::{self, ArchiveRecord, ScanMode, SourceRecord, UniqueFile};
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
    archive_records: &[ArchiveRecord],
    source_records: &[SourceRecord],
    sources: &[PathBuf],
    project: &str,
    mode: ScanMode,
    copy_mode: &str,
    archived: u64,
) -> Result<VerifyResult, String> {
    if !output.is_dir() {
        return Err(format!("输出目录不存在：{}", output.display()));
    }

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
    let mut ok_blocks = Vec::new();
    let mut missing_blocks = Vec::new();
    let mut mismatch_blocks = Vec::new();
    let mut extra_blocks = Vec::new();
    let mut output_dir_lines = Vec::new();

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
            if extra_blocks.len() < 500 {
                let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                let label = match scan::fingerprint_file(entry.path(), size, mode) {
                    Ok(key) => scan::fingerprint_label(&key),
                    Err(err) => format!("读取失败：{}", err),
                };
                extra_blocks.push(format!(
                    "[多余] {}\n       输出    {}  {}\n       说明    输出目录中存在，但本次扫描没有对应的源文件",
                    relative,
                    format_size(size),
                    label
                ));
                output_dir_lines.push(format!(
                    "[多余] {} | {} | {}",
                    relative,
                    format_size(size),
                    label
                ));
            }
            continue;
        };

        checked += 1;
        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
        let source_label = scan::fingerprint_label(&item.key);
        match scan::fingerprint_file(entry.path(), size, mode) {
            Ok(actual) if actual == item.key => {
                let actual_label = scan::fingerprint_label(&actual);
                ok_blocks.push(format!(
                    "[一致] {}\n       源文件  {}  {}\n       输出    {}  {}",
                    relative,
                    format_size(item.size),
                    source_label,
                    format_size(size),
                    actual_label
                ));
                output_dir_lines.push(format!(
                    "[一致] {} | {} | {}",
                    relative,
                    format_size(size),
                    actual_label
                ));
            }
            Ok(actual) => {
                mismatched += 1;
                let actual_label = scan::fingerprint_label(&actual);
                mismatch_blocks.push(format!(
                    "[不一致] {}\n       源文件  {}  {}\n       输出    {}  {}\n       说明    输出文件内容与扫描时的源文件不一致",
                    relative,
                    format_size(item.size),
                    source_label,
                    format_size(size),
                    actual_label
                ));
                output_dir_lines.push(format!(
                    "[不一致] {} | {} | {}",
                    relative,
                    format_size(size),
                    actual_label
                ));
            }
            Err(err) => {
                mismatched += 1;
                errors.push(format!("{}: {}", relative, err));
                mismatch_blocks.push(format!(
                    "[不一致] {}\n       源文件  {}  {}\n       输出    无法读取：{}",
                    relative,
                    format_size(item.size),
                    source_label,
                    err
                ));
                output_dir_lines.push(format!(
                    "[不一致] {} | {} | 无法读取：{}",
                    relative,
                    format_size(size),
                    err
                ));
            }
        }
    }

    let mut missing_items: Vec<&UniqueFile> = expected.values().copied().collect();
    missing_items.sort_by(|a, b| a.relative.cmp(&b.relative));
    for item in missing_items {
        missing += 1;
        missing_blocks.push(format!(
            "[缺失] {}\n       源文件  {}  {}\n       输出    未找到该文件",
            normalize_relative(&item.relative),
            format_size(item.size),
            scan::fingerprint_label(&item.key)
        ));
    }

    let ok = missing == 0 && mismatched == 0;
    let passed = checked.saturating_sub(mismatched);
    let rate = if items.is_empty() {
        100.0
    } else {
        passed as f64 / items.len() as f64 * 100.0
    };

    let mut blocks = Vec::new();
    if !missing_blocks.is_empty() || !mismatch_blocks.is_empty() || !extra_blocks.is_empty() {
        blocks.push("异常明细".to_string());
        blocks.extend(missing_blocks);
        blocks.extend(mismatch_blocks);
        blocks.extend(extra_blocks);
    }
    if !ok_blocks.is_empty() {
        blocks.push("通过明细".to_string());
        blocks.extend(ok_blocks);
    }
    if blocks.is_empty() {
        blocks.push("没有文件需要校验。".to_string());
    }
    let details_text = blocks.join("\n\n");

    let archive_values = if archive_records.is_empty() {
        "未发现归档文件。".to_string()
    } else {
        archive_records
            .iter()
            .map(|record| {
                format!(
                    "[归档] {} | {} | {}",
                    record.relative,
                    format_size(record.size),
                    scan::fingerprint_label(&record.key)
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let source_values = if source_records.is_empty() {
        "未发现原素材文件。".to_string()
    } else {
        source_records
            .iter()
            .map(|record| {
                format!(
                    "[{}] {}\\{} | {} | {}",
                    if record.archived {
                        "已归档"
                    } else {
                        "未归档"
                    },
                    record.source_root.display(),
                    record.source_relative,
                    format_size(record.size),
                    scan::fingerprint_label(&record.key)
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let output_values = if output_dir_lines.is_empty() {
        "未发现输出文件。".to_string()
    } else {
        output_dir_lines.join("\n")
    };
    let collection_payload = format!(
        "{}\n\n[归档目录]\n{}\n\n[原素材目录]\n{}\n\n[输出目录]\n{}",
        details_text, archive_values, source_values, output_values
    );
    let collection = sha256_hex(collection_payload.as_bytes());

    let (status, conclusion) = if ok {
        if extra == 0 {
            (
                "通过",
                "输出文件与源文件逐项对应，未发现缺失或内容差异，复制结果完整。",
            )
        } else {
            (
                "通过",
                "输出文件与源文件逐项对应，未发现缺失或内容差异；输出目录另有无关文件，不影响本次复制完整性。",
            )
        }
    } else {
        ("未通过", "发现缺失或内容不一致，请查看下方异常明细。")
    };
    let mode_label = mode.label();
    let copy_label = if copy_mode == "move" {
        "同盘移动"
    } else {
        "复制"
    };
    let method = match mode {
        ScanMode::Turbo => "文件大小 + 修改时间",
        ScanMode::Fast => "文件大小 + 头 64KB MD5 + 尾 64KB MD5",
        ScanMode::Exact => "文件大小 + 全量 MD5",
    };
    let source_list_text = if sources.is_empty() {
        "未设置".to_string()
    } else {
        sources
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join("\n            ")
    };

    let mut report = String::new();
    report.push_str("======================================================================\n");
    report.push_str("                  素材归档去重工具 · 归档完整性校验报告\n");
    report.push_str("======================================================================\n");
    report.push_str(&format!("结论: {}\n", status));
    report.push_str(&format!("说明: {}\n", conclusion));
    report.push('\n');
    report.push_str("一、任务信息\n");
    report.push_str("----------------------------------------------------------------------\n");
    report.push_str(&format!("项目名称    {}\n", project));
    report.push_str(&format!(
        "生成时间    {}\n",
        Local::now().format("%Y-%m-%d %H:%M:%S")
    ));
    report.push_str(&format!("软件版本    v{}\n", env!("CARGO_PKG_VERSION")));
    report.push_str(&format!("项目地址    {}\n", PROJECT_URL));
    report.push_str(&format!("比对模式    {}\n", mode_label));
    report.push_str(&format!("处理方式    {}\n", copy_label));
    report.push_str(&format!("原素材目录  {}\n", source_list_text));
    report.push_str(&format!("输出目录    {}\n", output.display()));
    report.push('\n');
    report.push_str("二、校验结果\n");
    report.push_str("----------------------------------------------------------------------\n");
    report.push_str(&format!("归档命中        {}\n", archived));
    report.push_str(&format!("未归档文件      {}\n", items.len()));
    report.push_str(&format!("应校验文件      {}\n", items.len()));
    report.push_str(&format!("校验通过        {}\n", passed));
    report.push_str(&format!("缺失            {}\n", missing));
    report.push_str(&format!("内容不一致      {}\n", mismatched));
    report.push_str(&format!("输出多余        {}\n", extra));
    report.push_str(&format!("通过率          {:.1}%\n", rate));
    report.push_str(&format!("应校验总大小    {}\n", format_size(total_bytes)));
    report.push('\n');
    report.push_str("三、复制对账（未归档文件 → 输出）\n");
    report.push_str("----------------------------------------------------------------------\n");
    report.push_str(&details_text);
    report.push('\n');
    report.push_str("四、归档目录文件校验值\n");
    report.push_str("----------------------------------------------------------------------\n");
    report.push_str(&archive_values);
    report.push('\n');
    report.push_str("五、原素材目录文件校验值\n");
    report.push_str("----------------------------------------------------------------------\n");
    report.push_str(&source_values);
    report.push('\n');
    report.push_str("六、输出目录文件校验值\n");
    report.push_str("----------------------------------------------------------------------\n");
    report.push_str(&output_values);
    report.push('\n');
    report.push_str("----------------------------------------------------------------------\n");
    report.push_str("七、校验说明\n");
    report.push_str("----------------------------------------------------------------------\n");
    report.push_str(&format!("本次使用 {} 比对。\n", method));
    report.push_str("“一致”表示输出文件的大小和指纹与扫描时记录的源文件完全相同。\n");
    report.push_str("报告只读取文件，不修改输出目录中的任何内容。\n");
    report.push('\n');
    report.push_str(&format!("集合 SHA256    {}\n", collection));
    report.push_str("报告结束\n");
    report.push_str("======================================================================\n");

    let report_text = format!("\u{feff}{}", report);

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
        project: project.to_string(),
    })
}

pub fn verify_only(
    config: &JobConfig,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(Progress),
) -> Result<VerifyResult, String> {
    let mode = ScanMode::parse(&config.mode)?;
    let archive = Path::new(config.archive.trim());
    let source_paths: Vec<PathBuf> = config
        .source_paths()
        .into_iter()
        .map(PathBuf::from)
        .collect();
    let output = Path::new(config.output.trim());
    if archive.as_os_str().is_empty() || source_paths.is_empty() || output.as_os_str().is_empty() {
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
        &source_paths,
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
        &summary.archive_records,
        &summary.source_records,
        &source_paths,
        &summary.project,
        mode,
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
            source_root: root.clone(),
            source_relative: PathBuf::from("a.txt"),
            size: 3,
            key: key.clone(),
        }];
        let sources = vec![root.clone()];
        let result = verify_items(
            &output,
            &items,
            &[],
            &[],
            &sources,
            "test",
            ScanMode::Fast,
            "copy",
            0,
        )
        .unwrap();
        assert!(result.ok);
        assert_eq!(result.checked, 1);
        assert_eq!(result.missing, 0);
        assert!(Path::new(&result.report).is_file());
        let report = fs::read_to_string(&result.report).unwrap();
        assert!(report.contains("结论: 通过"));
        assert!(report.contains("[一致] a.txt"));
        assert!(report.contains("源文件"));
        assert!(report.contains("输出"));

        let missing_items = vec![UniqueFile {
            path: output.join("missing.txt"),
            relative: PathBuf::from("missing.txt"),
            source_root: root.clone(),
            source_relative: PathBuf::from("missing.txt"),
            size: 3,
            key,
        }];
        let result = verify_items(
            &output,
            &missing_items,
            &[],
            &[],
            &sources,
            "test",
            ScanMode::Fast,
            "copy",
            0,
        )
        .unwrap();
        assert!(!result.ok);
        assert_eq!(result.missing, 1);
        fs::remove_dir_all(root).unwrap();
    }
}
