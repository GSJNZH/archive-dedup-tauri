use crate::manifest;
use crate::model::{JobConfig, Progress};
use crate::scan::{self, ScanMode, ScanProgress, UniqueFile};
use crate::verify;
use filetime::{set_file_mtime, FileTime};
use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct JobResult {
    pub archived: u64,
    pub unique: u64,
    pub copied: u64,
    pub failed: u64,
    pub size_gb: String,
    pub output: String,
    pub copy_mode: String,
    pub elapsed_ms: u64,
    pub dry_run: bool,
    pub errors: Vec<String>,
    pub report: String,
    pub verified: bool,
    pub verify_ok: bool,
    pub missing: u64,
    pub mismatched: u64,
    pub extra: u64,
}

struct WorkerMessage {
    ok: bool,
    skipped: bool,
    relative: String,
    size: u64,
    error: String,
}

pub fn run_job(
    config: &JobConfig,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(Progress),
) -> Result<JobResult, String> {
    let started = Instant::now();
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
    if !archive.is_dir() {
        return Err(format!("归档文件夹不存在：{}", archive.display()));
    }
    if !source.is_dir() {
        return Err(format!("原素材文件夹不存在：{}", source.display()));
    }

    let mut scan_progress = |event: ScanProgress| {
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

    if config.dry_run {
        let result = JobResult {
            archived: summary.archived,
            unique: summary.unique,
            copied: 0,
            failed: 0,
            size_gb: summary.size_gb.clone(),
            output: output.display().to_string(),
            copy_mode: config.copy_mode.clone(),
            elapsed_ms: started.elapsed().as_millis() as u64,
            dry_run: true,
            errors: summary.errors.clone(),
            report: String::new(),
            verified: false,
            verify_ok: false,
            missing: 0,
            mismatched: 0,
            extra: 0,
        };
        progress(Progress {
            stage: "done".to_string(),
            message: format!(
                "试运行完成：已归档 {}，未归档 {}，{} GB",
                result.archived, result.unique, result.size_gb
            ),
            percent: 100.0,
            archived: result.archived,
            unique: result.unique,
            size_gb: result.size_gb.clone(),
            copied: 0,
            failed: 0,
        });
        return Ok(result);
    }

    fs::create_dir_all(output)
        .map_err(|err| format!("无法创建输出目录 {}：{}", output.display(), err))?;

    let requested_move = config.copy_mode == "move";
    let move_files = requested_move && same_volume(source, output);
    if requested_move && !move_files {
        progress(Progress {
            stage: "notice".to_string(),
            message: "输出目录与原素材不在同一磁盘，已自动退化为复制。".to_string(),
            percent: 50.0,
            archived: summary.archived,
            unique: summary.unique,
            size_gb: summary.size_gb.clone(),
            copied: 0,
            failed: 0,
        });
    }

    let total = summary.unique_items.len();
    if total == 0 {
        let result = JobResult {
            archived: summary.archived,
            unique: 0,
            copied: 0,
            failed: 0,
            size_gb: summary.size_gb.clone(),
            output: output.display().to_string(),
            copy_mode: if move_files { "move" } else { "copy" }.to_string(),
            elapsed_ms: started.elapsed().as_millis() as u64,
            dry_run: false,
            errors: summary.errors.clone(),
            report: String::new(),
            verified: false,
            verify_ok: false,
            missing: 0,
            mismatched: 0,
            extra: 0,
        };
        progress(Progress {
            stage: "done".to_string(),
            message: "没有需要处理的未归档文件。".to_string(),
            percent: 100.0,
            archived: result.archived,
            unique: 0,
            size_gb: result.size_gb.clone(),
            copied: 0,
            failed: 0,
        });
        return Ok(result);
    }

    let workers = config.workers.clamp(1, 32).min(total);
    let queue = Arc::new(Mutex::new(VecDeque::from(summary.unique_items.clone())));
    let output_path = output.to_path_buf();
    let (tx, rx) = mpsc::channel::<WorkerMessage>();
    let mut copied = 0u64;
    let mut failed = 0u64;
    let mut bytes_done = 0u64;
    let mut done = 0u64;
    let mut errors = Vec::new();
    let action = if move_files { "移动" } else { "复制" };
    let copy_mode = if move_files { "move" } else { "copy" };
    let copy_ceiling = if config.auto_verify { 90.0 } else { 100.0 };
    let mut last_emit = Instant::now();

    if !move_files {
        if let Ok(available) = fs2::available_space(output) {
            let required = summary.unique_bytes.saturating_add(64 * 1024 * 1024);
            if available < required {
                return Err(format!(
                    "磁盘空间不足：需要 {}，可用 {}",
                    format_gb(required),
                    format_gb(available)
                ));
            }
        }
    }

    match manifest::write_manifest(output, source, archive, mode, copy_mode, &summary) {
        Ok(path) => progress(Progress {
            stage: "manifest".to_string(),
            message: format!("复查清单: {}", path),
            percent: 50.0,
            archived: summary.archived,
            unique: summary.unique,
            size_gb: summary.size_gb.clone(),
            copied: 0,
            failed: 0,
        }),
        Err(err) => {
            errors.push(format!("清单写入失败：{}", err));
            progress(Progress {
                stage: "notice".to_string(),
                message: format!("警告: {}", err),
                percent: 50.0,
                archived: summary.archived,
                unique: summary.unique,
                size_gb: summary.size_gb.clone(),
                copied: 0,
                failed: 0,
            });
        }
    }

    std::thread::scope(|scope| {
        for _ in 0..workers {
            let tx = tx.clone();
            let queue = Arc::clone(&queue);
            let output = output_path.clone();
            scope.spawn(move || loop {
                let item = match queue.lock() {
                    Ok(mut queue) => queue.pop_front(),
                    Err(_) => None,
                };
                let Some(item) = item else {
                    break;
                };
                if cancel.load(Ordering::SeqCst) {
                    let _ = tx.send(WorkerMessage {
                        ok: false,
                        skipped: true,
                        relative: item.relative.to_string_lossy().to_string(),
                        size: item.size,
                        error: String::new(),
                    });
                    continue;
                }
                let relative = item.relative.to_string_lossy().to_string();
                let result = transfer(&item, &output, move_files);
                let _ = match result {
                    Ok(()) => tx.send(WorkerMessage {
                        ok: true,
                        skipped: false,
                        relative,
                        size: item.size,
                        error: String::new(),
                    }),
                    Err(err) => tx.send(WorkerMessage {
                        ok: false,
                        skipped: false,
                        relative: relative.clone(),
                        size: item.size,
                        error: format!("{}: {}", relative, err),
                    }),
                };
            });
        }
        drop(tx);

        for message in rx {
            done += 1;
            if message.ok {
                copied += 1;
                bytes_done += message.size;
            } else if !message.skipped {
                failed += 1;
                if errors.len() < 100 {
                    errors.push(message.error);
                }
            }

            if last_emit.elapsed() >= Duration::from_millis(120) || done == total as u64 {
                last_emit = Instant::now();
                let percent = 50.0 + (done as f64 / total as f64) * (copy_ceiling - 50.0);
                progress(Progress {
                    stage: "copy".to_string(),
                    message: format!(
                        "{} {}/{} · {} / {} GB",
                        action,
                        done,
                        total,
                        format_gb(bytes_done),
                        summary.size_gb
                    ),
                    percent,
                    archived: summary.archived,
                    unique: summary.unique,
                    size_gb: format_gb(bytes_done),
                    copied,
                    failed,
                });
            }
        }
    });

    if cancel.load(Ordering::SeqCst) {
        return Err("已取消".to_string());
    }

    let mut report = String::new();
    let mut verified = false;
    let mut verify_ok = false;
    let mut missing = 0u64;
    let mut mismatched = 0u64;
    let mut extra = 0u64;

    if config.auto_verify {
        progress(Progress {
            stage: "verify".to_string(),
            message: "正在校验输出目录…".to_string(),
            percent: 92.0,
            archived: summary.archived,
            unique: summary.unique,
            size_gb: summary.size_gb.clone(),
            copied,
            failed,
        });
        match verify::verify_items(
            output,
            &summary.unique_items,
            mode,
            source,
            copy_mode,
            summary.archived,
        ) {
            Ok(result) => {
                report = result.report;
                verified = true;
                verify_ok = result.ok;
                missing = result.missing;
                mismatched = result.mismatched;
                extra = result.extra;
                if !result.errors.is_empty() {
                    errors.extend(result.errors);
                }
            }
            Err(err) => errors.push(format!("校验失败：{}", err)),
        }
    }

    let result = JobResult {
        archived: summary.archived,
        unique: summary.unique,
        copied,
        failed,
        size_gb: summary.size_gb.clone(),
        output: output.display().to_string(),
        copy_mode: copy_mode.to_string(),
        elapsed_ms: started.elapsed().as_millis() as u64,
        dry_run: false,
        errors,
        report,
        verified,
        verify_ok,
        missing,
        mismatched,
        extra,
    };

    let mut message = format!(
        "{}完成：成功 {}，失败 {}，{}",
        action, result.copied, result.failed, result.size_gb
    );
    if result.verified {
        message.push_str(if result.verify_ok {
            "；校验通过"
        } else {
            "；校验未通过"
        });
    }
    progress(Progress {
        stage: "done".to_string(),
        message,
        percent: 100.0,
        archived: result.archived,
        unique: result.unique,
        size_gb: result.size_gb.clone(),
        copied: result.copied,
        failed: result.failed,
    });

    Ok(result)
}

fn transfer(item: &UniqueFile, output: &Path, move_files: bool) -> Result<(), String> {
    let destination = output.join(&item.relative);
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| format!("无法创建目录 {}：{}", parent.display(), err))?;
    }

    if move_files {
        if destination.exists() {
            return Err("目标已存在".to_string());
        }
        match fs::rename(&item.path, &destination) {
            Ok(()) => return Ok(()),
            Err(_) => {
                copy_file(&item.path, &destination)?;
                fs::remove_file(&item.path).map_err(|err| format!("删除源文件失败：{}", err))?;
                return Ok(());
            }
        }
    }

    copy_file(&item.path, &destination)
}

fn copy_file(source: &Path, destination: &Path) -> Result<(), String> {
    let part = PathBuf::from(format!("{}.part", destination.display()));
    let _ = fs::remove_file(&part);

    let result = (|| -> Result<(), String> {
        fs::copy(source, &part).map_err(|err| format!("复制失败：{}", err))?;
        if let Ok(metadata) = fs::metadata(source) {
            if let Ok(modified) = metadata.modified() {
                let _ = set_file_mtime(&part, FileTime::from_system_time(modified));
            }
        }
        if destination.exists() {
            fs::remove_file(destination).map_err(|err| format!("覆盖目标失败：{}", err))?;
        }
        fs::rename(&part, destination).map_err(|err| format!("写入目标失败：{}", err))?;
        Ok(())
    })();

    if result.is_err() {
        let _ = fs::remove_file(&part);
    }
    result
}

#[cfg(windows)]
fn same_volume(a: &Path, b: &Path) -> bool {
    match (drive_letter(a), drive_letter(b)) {
        (Some(a), Some(b)) => a.eq_ignore_ascii_case(&b),
        _ => false,
    }
}

#[cfg(windows)]
fn drive_letter(path: &Path) -> Option<String> {
    let text = path.to_string_lossy();
    if text.len() >= 2 && text.as_bytes()[1] == b':' {
        Some(text[..2].to_string())
    } else {
        None
    }
}

#[cfg(not(windows))]
fn same_volume(_a: &Path, _b: &Path) -> bool {
    true
}

fn format_gb(bytes: u64) -> String {
    format!("{:.1}", bytes as f64 / 1024.0 / 1024.0 / 1024.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_root(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "archive-dedup-job-{}-{}-{}",
            name,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn copy_and_move_transfer() {
        let root = test_root("transfer");
        let source = root.join("source");
        let output = root.join("output");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("copy.txt"), b"copy-content").unwrap();
        fs::write(source.join("move.txt"), b"move-content").unwrap();

        let copy_item = UniqueFile {
            path: source.join("copy.txt"),
            relative: PathBuf::from("copy.txt"),
            size: 12,
            key: "k1".to_string(),
        };
        transfer(&copy_item, &output, false).unwrap();
        assert!(source.join("copy.txt").exists());
        assert_eq!(fs::read(output.join("copy.txt")).unwrap(), b"copy-content");

        let move_item = UniqueFile {
            path: source.join("move.txt"),
            relative: PathBuf::from("move.txt"),
            size: 12,
            key: "k2".to_string(),
        };
        transfer(&move_item, &output, true).unwrap();
        assert!(!source.join("move.txt").exists());
        assert_eq!(fs::read(output.join("move.txt")).unwrap(), b"move-content");

        fs::remove_dir_all(root).unwrap();
    }
}
