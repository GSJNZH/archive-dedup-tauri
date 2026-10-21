(() => {
  "use strict";

  const $ = (id) => document.getElementById(id);
  const isTauri = Boolean(window.__TAURI__ && window.__TAURI__.core);
  const state = {
    mode: "fast",
    copyMode: "move",
    running: false,
    timer: null,
    demo: !isTauri,
    milestones: new Set(),
  };

  const demoPaths = {
    archive: "D:\\Projects\\外国语学校\\09_03-项目打包",
    source: "D:\\Footage\\外国语学校-6.11",
    output: "D:\\Footage\\外国语学校-6.11\\09_04-未使用画面",
  };

  function now() {
    const d = new Date();
    return [d.getHours(), d.getMinutes(), d.getSeconds()]
      .map((v) => String(v).padStart(2, "0"))
      .join(":");
  }

  function log(message, tone = "") {
    const line = document.createElement("div");
    line.className = "log-line" + (tone ? " " + tone : "");
    const time = document.createElement("span");
    time.className = "log-time";
    time.textContent = now();
    const text = document.createElement("span");
    text.className = "log-text";
    text.textContent = message;
    line.append(time, text);
    const box = $("log");
    box.appendChild(line);
    while (box.children.length > 600) box.removeChild(box.firstChild);
    box.scrollTop = box.scrollHeight;
  }

  function setStatus(text, percent) {
    $("statusText").textContent = text;
    if (typeof percent === "number") {
      const p = Math.max(0, Math.min(100, percent));
      $("progressBar").style.width = p + "%";
      $("percentText").textContent = Math.round(p) + "%";
    }
  }

  function setRunning(running) {
    state.running = running;
    $("startBtn").disabled = running;
    $("verifyBtn").disabled = running;
    $("cancelBtn").disabled = !running;
    $("runDot").className = "dot" + (running ? " running" : "");
    $("summaryHint").textContent = running ? "正在处理素材…" : "等待开始";
  }

  function setStats(archived, unique, sizeGB) {
    $("statArchived").textContent = archived;
    $("statUnique").textContent = unique;
    $("statSize").textContent = sizeGB;
  }

  function currentModeLabel() {
    return { fast: "快速(采样哈希)", turbo: "极速(大小+修改时间)", exact: "精确(全量MD5)" }[state.mode];
  }

  function currentCopyLabel() {
    return state.copyMode === "move" ? "同盘移动" : "复制";
  }

  function gatherConfig() {
    return {
      archive: $("archivePath").value.trim(),
      source: $("sourcePath").value.trim(),
      output: $("outputPath").value.trim(),
      mode: state.mode,
      copy_mode: state.copyMode,
      workers: Number($("workers").value),
      dry_run: $("dryRun").checked,
      auto_verify: $("autoVerify").checked,
    };
  }

  function validate(config) {
    if (!config.archive || !config.source || !config.output) {
      log("请先选择归档文件夹、原素材文件夹和输出文件夹。", "warn");
      return false;
    }
    return true;
  }

  async function pick(kind) {
    if (state.running) return;
    const titles = { archive: "选择归档文件夹", source: "选择原素材文件夹", output: "选择输出文件夹" };
    if (isTauri) {
      try {
        const path = await window.__TAURI__.core.invoke("pick_folder", { title: titles[kind] });
        if (path) {
          $(kind + "Path").value = path;
          if (kind === "source" && !$("outputPath").value.trim()) {
            $("outputPath").value = path + "\\09_04-未使用画面";
          }
          log("已选择 " + titles[kind] + "：" + path, "muted");
        }
      } catch (err) {
        log("选择目录失败：" + err, "err");
      }
    } else {
      $(kind + "Path").value = demoPaths[kind];
      if (kind === "source") $("outputPath").value = demoPaths.output;
      log("演示模式：已填入示例路径。", "muted");
    }
  }

  async function openPath(kind) {
    const path = $(kind + "Path").value.trim();
    if (!path) {
      log("路径为空，无法打开。", "warn");
      return;
    }
    if (isTauri) {
      try {
        await window.__TAURI__.core.invoke("open_path", { path });
      } catch (err) {
        log("打开目录失败：" + err, "err");
      }
    } else {
      log("演示模式：将打开 " + path, "muted");
    }
  }

  function chooseSegment(id, key) {
    $(id).querySelectorAll("button[data-value]").forEach((button) => {
      button.addEventListener("click", () => {
        if (state.running) return;
        $(id).querySelectorAll("button").forEach((b) => b.classList.remove("active"));
        button.classList.add("active");
        state[key] = button.dataset.value;
        if (key === "copyMode") {
          const warning = document.querySelector(".warning");
          warning.style.display = state.copyMode === "move" ? "" : "none";
        }
      });
    });
  }

  function resetProgress() {
    $("progressBar").style.width = "0%";
    $("percentText").textContent = "0%";
    $("statusText").textContent = "就绪";
    $("runDot").className = "dot";
  }

  async function startJob() {
    if (state.running) return;
    const config = gatherConfig();
    if (!validate(config)) return;

    resetProgress();
    state.milestones.clear();
    setRunning(true);
    setStatus("正在扫描归档文件夹…", 2);
    log("开始任务：" + currentModeLabel() + " / " + currentCopyLabel() + " / " + config.workers + " 线程", "accent");

    if (isTauri) {
      try {
        await window.__TAURI__.core.invoke("start_job", { config });
      } catch (err) {
        log("启动失败：" + err, "err");
        setRunning(false);
        $("runDot").className = "dot error";
      }
      return;
    }

    runDemo(config);
  }

  function runDemo(config) {
    let pct = 0;
    const total = 71;
    const started = Date.now();

    const tick = () => {
      pct = Math.min(100, pct + 2.5 + Math.random() * 6.5);
      setStatus(pct < 72 ? "正在复制未归档文件…" : "正在校验输出…", pct);

      if (pct >= 12 && !state.milestones.has(12)) {
        state.milestones.add(12);
        log("归档: 71 个文件 / 10.9 GB", "muted");
      }
      if (pct >= 32 && !state.milestones.has(32)) {
        state.milestones.add(32);
        log("已归档(跳过): 33 个, 未归档(需上传): 71 个, 共 28.1 GB", "muted");
      }
      if (pct >= 55 && !state.milestones.has(55)) {
        state.milestones.add(55);
        log("正在写入: 09_04-未使用画面\\A001_C012_0611.mov", "muted");
      }
      if (pct >= 78 && !state.milestones.has(78)) {
        state.milestones.add(78);
        log("复制完成: 71 成功, 0 失败", "ok");
        log("开始校验输出目录…", "muted");
      }
      if (pct >= 97 && !state.milestones.has(97)) {
        state.milestones.add(97);
        log("校验结果: 通过 · 缺失 0 / 不一致 0 / 多余 0", "ok");
      }
      setStats(71, 71, "28.1");

      if (pct >= 100) {
        clearInterval(state.timer);
        state.timer = null;
        const elapsed = ((Date.now() - started) / 1000).toFixed(1);
        setStatus("校验通过 · 71 个文件已核对", 100);
        setRunning(false);
        $("runDot").className = "dot done";
        $("summaryHint").textContent = "完成 · 用时 " + elapsed + " 秒";
        log("任务完成。用时 " + elapsed + " 秒。", "ok");
        log("校验报告: " + (config.output || demoPaths.output) + "\\归档完整性校验报告.txt", "muted");
      }
    };

    state.timer = setInterval(tick, 190);
  }

  async function cancelJob() {
    if (!state.running) return;
    if (state.timer) {
      clearInterval(state.timer);
      state.timer = null;
    }
    if (isTauri) {
      try {
        await window.__TAURI__.core.invoke("cancel_job");
      } catch (err) {
        log("取消失败：" + err, "err");
      }
    }
    setRunning(false);
    setStatus("已取消", 0);
    $("runDot").className = "dot";
    log("任务已取消。", "warn");
  }

  async function verifyOutput() {
    if (state.running) return;
    const config = gatherConfig();
    if (!config.output) {
      log("请先选择输出文件夹。", "warn");
      return;
    }
    resetProgress();
    setRunning(true);
    setStatus("正在校验输出目录…", 8);
    log("开始校验输出目录：" + config.output, "accent");

    if (isTauri) {
      try {
        await window.__TAURI__.core.invoke("verify_output", { config });
      } catch (err) {
        log("校验失败：" + err, "err");
        setRunning(false);
        $("runDot").className = "dot error";
      }
      return;
    }

    setTimeout(() => {
      setStatus("校验通过 · 缺失 0 / 不一致 0 / 多余 0", 100);
      setRunning(false);
      log("校验结果: 通过 · 缺失 0 / 不一致 0 / 多余 0", "ok");
      log("校验报告: " + config.output + "\\归档完整性校验报告.txt", "muted");
      $("summaryHint").textContent = "校验完成";
      $("runDot").className = "dot done";
    }, 1300);
  }

  function wireTauriEvents() {
    if (!isTauri) return;
    window.__TAURI__.event.listen("job-progress", (event) => {
      const p = event.payload || {};
      if (typeof p.percent === "number") setStatus(p.message || "处理中…", p.percent);
      if (p.message) log(p.message, p.stage === "error" ? "err" : "muted");
      if (typeof p.archived === "number" || typeof p.unique === "number") {
        setStats(p.archived || 0, p.unique || 0, p.size_gb || "0");
      }
    });
    window.__TAURI__.event.listen("job-done", (event) => {
      const p = event.payload || {};
      setRunning(false);
      if (p.ok === false) {
        $("runDot").className = "dot error";
        setStatus(p.message || "任务失败", 100);
        log(p.message || "任务失败", "err");
        return;
      }
      setStatus(p.message || "任务完成", 100);
      $("summaryHint").textContent = p.message || "任务完成";
      $("runDot").className = "dot done";
      if (p.report) log("校验报告: " + p.report, "muted");
    });
  }

  function init() {
    $("envBadge").textContent = isTauri ? "Tauri 已连接" : "UI 预览";
    $("footerState").textContent = isTauri ? "本地运行" : "浏览器预览 · 演示数据";

    if (!isTauri) {
      $("archivePath").value = demoPaths.archive;
      $("sourcePath").value = demoPaths.source;
      $("outputPath").value = demoPaths.output;
      log("素材归档去重工具 v1.3.0 已就绪。", "accent");
      log("当前是浏览器预览，使用演示数据；打包后会调用 Rust 核心。", "muted");
      log("默认：同盘移动 / 8 线程（SSD）。移动会移除原素材中的未归档文件。", "warn");
    } else {
      log("素材归档去重工具 v1.3.0 已就绪。", "accent");
      log("Rust 核心已连接。", "muted");
    }

    document.querySelectorAll("[data-pick]").forEach((button) => {
      button.addEventListener("click", () => pick(button.dataset.pick));
    });
    document.querySelectorAll("[data-open]").forEach((button) => {
      button.addEventListener("click", () => openPath(button.dataset.open));
    });

    chooseSegment("modeSeg", "mode");
    chooseSegment("copySeg", "copyMode");

    $("startBtn").addEventListener("click", startJob);
    $("cancelBtn").addEventListener("click", cancelJob);
    $("verifyBtn").addEventListener("click", verifyOutput);
    $("clearLog").addEventListener("click", () => { $("log").innerHTML = ""; });

    wireTauriEvents();
  }

  document.addEventListener("DOMContentLoaded", init);
})();
