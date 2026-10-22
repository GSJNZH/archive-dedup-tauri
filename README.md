# ArchiveDedup · Tauri 2

素材归档去重工具的 **Tauri 2 + Rust** 重构版。

![Tauri UI](docs/tauri-ui.png)

当前是 **Phase 2：扫描 + 复制/移动**：

- 界面：纯 HTML / CSS / JavaScript，深色苹果风，响应式布局
- 后端：Rust（Tauri 2），已接好目录选择、打开目录、进度事件、取消
- Phase 1：真实目录扫描、极速/快速/精确三种指纹、未归档清单
- Phase 2：并发复制、同盘移动、跨盘退化为复制、保留相对目录和 mtime
- 最终校验报告：Phase 3 接入（当前“完成后自动校验”默认关闭）
- 旧 Python 版暂时保留在 `../archive-dedup`，作为行为对照

## 直接预览 UI

`ui/index.html` 可以直接用浏览器打开。没有 Tauri 环境时会自动进入演示模式，使用示例路径和模拟进度。

## 本地开发

需要：

- Rust stable
- Visual Studio C++ Build Tools（MSVC）
- WebView2 Runtime（Windows 11 自带）
- Node.js 20+
- Tauri CLI 2

```bat
cd archive-dedup-tauri
npm install -g @tauri-apps/cli@^2
tauri dev
```

构建：

```bat
tauri build
```

产物：

```text
src-tauri\target\release\bundle\nsis\ArchiveDedup_1.3.0_x64-setup.exe
src-tauri\target\release\archive-dedup-tauri.exe
```

## GitHub Actions

`.github/workflows/build-tauri.yml` 使用 `windows-latest` 构建：

- 先运行 `cargo test`（扫描 + 复制/移动单元测试）
- 再执行 `tauri build`
- 手动运行 `build-tauri`
- 或推送 `v1.3.0-tauri` 这类标签

构建完成后在 Actions 的 Artifacts 里下载 `ArchiveDedup-Tauri-windows`。

## 目录

```text
ui/                 HTML / CSS / JS
src-tauri/          Tauri + Rust
  src/lib.rs        命令注册、进度事件
  src/model.rs      前后端数据结构
  src/scan.rs       目录扫描、三种指纹、未归档清单
  src/job.rs        并发复制/移动、mtime、进度
  tauri.conf.json   窗口与打包配置
  icons/            应用图标
.github/workflows/  Windows 构建
```

## 迁移路线

- [x] Phase 0：Tauri 骨架 + 深色响应式 UI + 演示进度
- [x] Phase 1：Rust 目录扫描、三种指纹、未归档清单（代码已接入，待 Windows 构建验证）
- [x] Phase 2：复制 / 同盘移动 / 并发 / 取消
- [ ] Phase 3：输出校验、`归档完整性校验报告.txt`
- [ ] Phase 4：安装包、Release、与 Python 版结果对照

## Phase 1 已实现

```text
archive / source / output
        ↓ walkdir
文件列表（跳过输出目录和清单/报告）
        ↓
极速：size + mtime
快速：size + head64KB MD5 + tail64KB MD5
精确：size + 全量 MD5
        ↓
归档指纹集合 vs 原素材
        ↓
ScanSummary：archived / unique / unique_bytes / unique_files
```

## Phase 2 已实现

```text
ScanSummary.unique_items
        ↓
输出目录 + 相对路径
        ↓
复制：.part 临时文件 → 替换目标
移动：同盘 rename；失败时复制后删源
        ↓
并发线程池（1–32，默认 8）
        ↓
保留 mtime、进度、取消、失败汇总
```

复制到输出目录时使用 `.part` 临时文件，避免中断留下半个文件；同盘移动优先 `rename`，跨盘自动退化为复制。

## 说明

浏览器预览的“开始处理 / 仅校验”是演示进度；Tauri 版“开始处理”会执行真实扫描、复制/移动，Phase 3 接入校验报告。
