mod job;
mod model;
mod scan;

use model::{JobConfig, JobDone, Progress};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tauri::{AppHandle, Emitter, State};

#[derive(Default)]
struct AppState {
    running: Arc<AtomicBool>,
    cancel: Arc<AtomicBool>,
}

#[tauri::command]
fn pick_folder(title: String) -> Option<String> {
    rfd::FileDialog::new()
        .set_title(title)
        .pick_folder()
        .map(|path| path.display().to_string())
}

#[tauri::command]
fn open_path(path: String) -> Result<(), String> {
    if path.trim().is_empty() {
        return Err("路径为空".to_string());
    }

    #[cfg(target_os = "windows")]
    {
        return std::process::Command::new("explorer")
            .arg(&path)
            .spawn()
            .map(|_| ())
            .map_err(|err| err.to_string());
    }

    #[cfg(target_os = "macos")]
    {
        return std::process::Command::new("open")
            .arg(&path)
            .spawn()
            .map(|_| ())
            .map_err(|err| err.to_string());
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        return std::process::Command::new("xdg-open")
            .arg(&path)
            .spawn()
            .map(|_| ())
            .map_err(|err| err.to_string());
    }

    #[allow(unreachable_code)]
    Err("当前平台不支持打开目录".to_string())
}

fn demo_progress(app: &AppHandle, stage: &str, message: impl Into<String>, percent: f64) {
    let _ = app.emit(
        "job-progress",
        Progress {
            stage: stage.to_string(),
            message: message.into(),
            percent,
            archived: 71,
            unique: 71,
            size_gb: "28.1".to_string(),
            copied: 0,
            failed: 0,
        },
    );
}

fn demo_done(app: &AppHandle, ok: bool, message: impl Into<String>, report: String) {
    let _ = app.emit(
        "job-done",
        JobDone {
            ok,
            message: message.into(),
            report,
            archived: 71,
            unique: 71,
            size_gb: "28.1".to_string(),
            copied: 0,
            failed: 0,
        },
    );
}

#[tauri::command]
fn start_job(app: AppHandle, state: State<'_, AppState>, config: JobConfig) -> Result<(), String> {
    if state.running.swap(true, Ordering::SeqCst) {
        return Err("任务正在运行。".to_string());
    }
    state.cancel.store(false, Ordering::SeqCst);

    let running = state.running.clone();
    let cancel = state.cancel.clone();

    std::thread::spawn(move || {
        let progress_app = app.clone();
        let mut on_progress = |event: Progress| {
            let _ = progress_app.emit("job-progress", event);
        };

        let result = job::run_job(&config, &cancel, &mut on_progress);
        running.store(false, Ordering::SeqCst);

        match result {
            Ok(result) => {
                let action = if result.copy_mode == "move" {
                    "移动"
                } else {
                    "复制"
                };
                let mut message = if result.dry_run {
                    format!(
                        "试运行完成：已归档 {}，未归档 {}，{} GB；未复制任何文件",
                        result.archived, result.unique, result.size_gb
                    )
                } else {
                    format!(
                        "{}完成：成功 {}，失败 {}，{} GB",
                        action, result.copied, result.failed, result.size_gb
                    )
                };
                if result.failed > 0 {
                    message.push_str("；详见日志");
                }
                let _ = app.emit(
                    "job-done",
                    JobDone {
                        ok: true,
                        message,
                        report: String::new(),
                        archived: result.archived,
                        unique: result.unique,
                        size_gb: result.size_gb,
                        copied: result.copied,
                        failed: result.failed,
                    },
                );
            }
            Err(err) => {
                let _ = app.emit(
                    "job-done",
                    JobDone {
                        ok: false,
                        message: err,
                        report: String::new(),
                        archived: 0,
                        unique: 0,
                        size_gb: "0.0".to_string(),
                        copied: 0,
                        failed: 0,
                    },
                );
            }
        }
    });

    Ok(())
}

#[tauri::command]
fn scan_library(config: JobConfig) -> Result<scan::ScanSummary, String> {
    let mode = scan::ScanMode::parse(&config.mode)?;
    let output_path = config.output.clone();
    let output = if output_path.trim().is_empty() {
        None
    } else {
        Some(PathBuf::from(output_path))
    };
    let cancel = AtomicBool::new(false);
    let mut noop = |_event: scan::ScanProgress| {};
    scan::scan_dirs(
        Path::new(config.archive.trim()),
        Path::new(config.source.trim()),
        output.as_deref(),
        mode,
        &cancel,
        &mut noop,
    )
}

#[tauri::command]
fn verify_output(
    app: AppHandle,
    state: State<'_, AppState>,
    config: JobConfig,
) -> Result<(), String> {
    if state.running.swap(true, Ordering::SeqCst) {
        return Err("任务正在运行。".to_string());
    }
    state.cancel.store(false, Ordering::SeqCst);

    let running = state.running.clone();
    let cancel = state.cancel.clone();

    std::thread::spawn(move || {
        let total = 5u32;
        for step in 0..=total {
            if cancel.load(Ordering::SeqCst) {
                running.store(false, Ordering::SeqCst);
                demo_done(&app, false, "校验已取消", config.output);
                return;
            }
            let percent = (step as f64 / total as f64) * 100.0;
            demo_progress(
                &app,
                "verify",
                format!("校验骨架 {}/{}", step, total),
                percent,
            );
            std::thread::sleep(std::time::Duration::from_millis(180));
        }
        running.store(false, Ordering::SeqCst);
        demo_done(&app, true, "Rust 校验骨架完成（演示）", config.output);
    });

    Ok(())
}

#[tauri::command]
fn cancel_job(state: State<'_, AppState>) {
    state.cancel.store(true, Ordering::SeqCst);
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            pick_folder,
            open_path,
            start_job,
            scan_library,
            verify_output,
            cancel_job
        ])
        .run(tauri::generate_context!())
        .expect("error while running ArchiveDedup");
}
