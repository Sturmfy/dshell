<div align="center">
  <img src="src-tauri/icons/icon.png" width="120" alt="dshell">
  <h1>dshell</h1>
  <p><b>DeepSeek Harness 的桌面外壳</b><br>
  一个二进制，一个窗口，多个原生 DSH 选项卡。</p>
  <p>
    <a href="LICENSE"><img alt="License: MIT" src="https://img.shields.io/badge/license-MIT-2f6fed"></a>
    <img alt="Platform: Windows" src="https://img.shields.io/badge/platform-Windows%2010%2F11-0078D6">
    <img alt="Tauri 2" src="https://img.shields.io/badge/Tauri-2-24C8DB">
    <img alt="Rust 1.90+" src="https://img.shields.io/badge/Rust-1.90%2B-000000">
  </p>
  <p>💙 感谢 <a href="https://github.com/deepseek-ai/deepseek-harness">DeepSeek Harness</a>：
  界面、会话和那只鲸鱼都来自 DSH，dshell 只是给它加了一层窗口。</p>
</div>

---

## 🎯 目标

dshell 不是另一个 AI 客户端，而是 DSH 的桌面入口。它只做五件事，并且尽量做好：

- **一个二进制** —— 单个 `dshell.exe`（约 4 MB），拷走就能跑。不随附 Node，不需要安装器。
- **一个窗口，多个会话** —— 每个选项卡是一个独立的 `dsh web`，会话、审批、终端互不干扰。
- **100% 原生界面** —— 不重写、不注入、不魔改 DSH；选项卡里就是 DSH 本身。
- **凭据不经过 dshell** —— 启动 URL 的 token 只在内存里出现一次，随后由 webview 自己兑换并持有。
- **多后端，一个接口** —— npx / 本地 npm / 源码 checkout / 任意 `dsh` 二进制，行为一致。

## 📦 使用

### 直接运行

下载 `dshell.exe`，双击。首次启动会打开向导：选择数据位置 → 环境检查 → 新建连接。

数据根按四级回落，设置页会显示最终生效的是哪一层：

1. `--data-root <path>`
2. `$DSHELL_HOME`
3. `<exe 同级>/dshell-data/`
4. `%LOCALAPPDATA%\dshell\`

### 从源码构建

需要 Rust ≥ 1.90 + MSVC Build Tools、Node ≥ 20（仅构建 UI）、WebView2 Runtime。

```powershell
npm install        # esbuild / typescript / @tauri-apps/api
npm run build      # 构建 UI + release 二进制
npm run binary     # 落到 release/dshell.exe，并断言它是单文件
npm run app        # 开发模式运行
npm test           # Rust 单元测试
npm run check      # clippy -D warnings
npm run icons      # logo-tile.svg → PNG → icon.ico（需要本机 Chrome/Edge）
```

### 调试

```powershell
$env:DSHELL_DEBUG='1'
.\dshell.exe --data-root E:\tmp\dshell-data
# → <数据根>\logs\dshell-debug.log
```

架构、边界条件和已知限制见 [ARCHITECTURE.md](ARCHITECTURE.md)。

## 📄 许可

MIT © 2026 Sturmfy —— 见 [LICENSE](LICENSE)。
