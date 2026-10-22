use crate::scan::{ScanMode, ScanSummary};
use chrono::Local;
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

pub const MANIFEST_NAME: &str = "_archive_dedup_manifest.json";

pub fn write_manifest(
    output: &Path,
    sources: &[PathBuf],
    archive: &Path,
    mode: ScanMode,
    copy_mode: &str,
    summary: &ScanSummary,
) -> Result<String, String> {
    let records: Vec<Value> = summary
        .unique_items
        .iter()
        .map(|item| {
            json!({
                "rel": item.source_relative.to_string_lossy().to_string(),
                "source": clean_path(&item.source_root),
                "out_rel": item.relative.to_string_lossy().to_string(),
                "size": item.size,
                "key": key_to_json(&item.key),
            })
        })
        .collect();

    let source_list: Vec<String> = sources.iter().map(|source| clean_path(source)).collect();
    let first_source = sources
        .first()
        .map(|source| clean_path(source))
        .unwrap_or_default();
    let payload = json!({
        "version": 1,
        "created": Local::now().format("%Y-%m-%dT%H:%M:%S").to_string(),
        "mode": mode_label(mode),
        "copy_mode": copy_label(copy_mode),
        "source_count": summary.total,
        "source_bytes": summary.source_bytes,
        "source": first_source,
        "sources": source_list,
        "archive": clean_path(archive),
        "files": records,
    });

    let path = output.join(MANIFEST_NAME);
    let part = output.join(format!("{}.part", MANIFEST_NAME));
    let text =
        serde_json::to_string_pretty(&payload).map_err(|err| format!("生成清单失败：{}", err))?;
    let _ = fs::remove_file(&part);
    fs::write(&part, text.as_bytes()).map_err(|err| format!("写入清单失败：{}", err))?;
    if path.exists() {
        let _ = fs::remove_file(&path);
    }
    fs::rename(&part, &path).map_err(|err| format!("替换清单失败：{}", err))?;
    Ok(path.display().to_string())
}

fn clean_path(path: &Path) -> String {
    let text = path.display().to_string();
    text.strip_prefix(r"\\?\").unwrap_or(&text).to_string()
}

fn key_to_json(key: &str) -> Value {
    if let Some(rest) = key.strip_prefix("f:") {
        let mut parts = rest.splitn(3, ':');
        let size = parts
            .next()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        let head = parts.next().unwrap_or("");
        let tail = parts.next().unwrap_or("");
        json!([size, head, tail])
    } else if let Some(rest) = key.strip_prefix("e:") {
        let mut parts = rest.splitn(2, ':');
        let size = parts
            .next()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        let hash = parts.next().unwrap_or("");
        json!([size, hash])
    } else if let Some(rest) = key.strip_prefix("t:") {
        let mut parts = rest.splitn(2, ':');
        let size = parts
            .next()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        let mtime = parts
            .next()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        json!([size, mtime])
    } else {
        json!(key)
    }
}

fn mode_label(mode: ScanMode) -> &'static str {
    match mode {
        ScanMode::Turbo => "极速(大小+修改时间,不读内容)",
        ScanMode::Fast => "快速(采样哈希,推荐)",
        ScanMode::Exact => "精确(全量MD5)",
    }
}

fn copy_label(copy_mode: &str) -> &'static str {
    if copy_mode == "move" {
        "同盘移动(瞬间,会移除原素材中的文件)"
    } else {
        "复制(安全,保留原素材)"
    }
}
