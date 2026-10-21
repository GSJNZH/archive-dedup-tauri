mod model;

use model::{JobConfig, JobDone, Progress};
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
async fn pick_folder(title: String) -> Option<String> {
    tauri::async_runtime::spawn_blocking(move || {
        rfd::FileDialog::new()
            .set_title(title)
            .pick_folder()
            .map(|path| path.display().to_string())
    })
    .await
    .ok()
    .flatten()
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
        },
    );
}

#[tauri::command]
fn start_job(
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
        // Phase 0：先用可取消的演示任务跑通前后端进度链路。
        // Phase 1 会把这里替换为真正的 Rust 扫描/复制实现。
        let total = 8u32;
        for step in 0..=total {
            if cancel.load(Ordering::SeqCst) {
                running.store(false, Ordering::SeqCst);
                demo_done(&app, false, "任务已取消", config.output);
                return;
            }

            let percent = (step as f64 / total as f64) * 100.0;
            let message = if step == 0 {
                "Rust 核心骨架启动（Phase 0）".to_string()
            } else {
                format!("演示进度 {}/{}", step, total)
            };
            demo_progress(&app, "demo", message, percent);
            std::thread::sleep(std::time::Duration::from_millis(240));
        }

        running.store(false, Ordering::SeqCst);
        demo_done(
            &app,
            true,
            format!(
                "Rust 骨架演示完成：{} / {}",
                config.mode, config.copy_mode
            ),
            config.output,
        );
    });

    Ok(())
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
            demo_progress(&app, "verify", format!("校验骨架 {}/{}", step, total), percent);
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
            verify_output,
            cancel_job
        ])
        .run(tauri::generate_context!())
        .expect("error while running ArchiveDedup");
}
