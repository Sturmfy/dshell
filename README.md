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
  <p><a href="README.md">简体中文</a> · <a href="docs/english.md">English</a></p>
  <p>💙 感谢 <a href="https://github.com/deepseek-ai/deepseek-harness">DeepSeek Harness</a>：
  界面、会话和那只鲸鱼都来自 DSH，dshell 只是给它加了一层窗口。</p>
</div>

---

![新建连接界面](docs/screenshots/new-connection.png)

<p align="center"><sub><b>新建连接</b>：让 dshell 启动本地后端，或接入一台已经在运行的 DSH。</sub></p>

## 为什么

`dsh web` 已经是一个完整的 Web 应用，缺的只是一个像样的桌面窗口。dshell 就做这一层：

- **不重写** DSH 的聊天、会话、审批或终端 —— 选项卡里加载的就是官方前端；
- **不注入**脚本，也不改 DSH 源码，升级 DSH 不需要等 dshell 跟进；
- **不做另一个客户端** —— 没有自己的 agent 逻辑，也不碰模型凭据。

它只负责三件事：**启动或连接 `dsh web`、接住启动 token、把多个实例排成选项卡**。

## 特性

- **一个二进制**：单个 `dshell.exe`（约 4 MB），拷走就能跑，不需要安装器，也不随附 Node。
- **一个窗口，多个会话**：每个选项卡是一个独立的 DSH 实例（本地启动或远程接入），会话、审批、终端互不干扰。关闭选项卡默认结束后台进程，可在高级选项中改。
- **100% 原生界面**：不重写、不注入，选项卡里就是 DSH 本身。
- **凭据不经过 dshell**：启动 token 只在内存里出现一次，随后由 webview 自己兑换并持有 cookie。
- **连接优先**：连接已有或远程实例不需要 Node；会扫描本机正在运行的实例、记住历史连接，并可固定常用目标。
- **像浏览器一样用**：`Ctrl+T` 新建、`Ctrl+W` 关闭、`Ctrl+Tab` 切换、拖拽排序、双击重命名、中键关闭。
- **跟随 DSH 主题**：选项卡栏会随当前 DSH 页面的明暗主题切换。
- **数据在你手里**：dshell 自己的配置、运行时与 webview 存储集中在一个数据根；`DSH_HOME` 默认不动，原有的历史、设置与 API Key 都会照旧。

> 默认情况下，DSH 内容页会忽略 Windows 的“减少动态效果”，让“进行中”圆环等
> 工作动画继续播放（dshell 自己的页面仍跟随系统设置）。可在 **设置 → 偏好 →
> 在 DSH 内容页保持动画播放** 中关闭；新选项卡立即生效，已打开的选项卡需重启
> dshell 才会更新。

## 快速开始

### 直接运行

从 [Releases](https://github.com/Sturmfy/dshell/releases) 获取 `dshell.exe`（或按下方步骤从源码构建），双击即可。
首次启动会打开向导：选择数据位置 → 环境检查 → 新建连接。

需要 Windows 10/11 与 WebView2 Runtime（系统一般已自带）。「连接」不需要 Node，
只有「启动本地」走 npx 时才需要。

### 数据放在哪

dshell 的数据根按四级回落，设置页会显示最终生效的是哪一层：

| 优先级 | 位置 |
|---|---|
| 1 | `--data-root <path>` |
| 2 | `$DSHELL_HOME` |
| 3 | `<exe 所在目录>\dshell-data\` |
| 4 | `%LOCALAPPDATA%\dshell\` |

配置、下载的运行时、每个实例的 webview 存储都在这里。DSH 自己的 `~/.dsh`
（会话、设置、凭据）默认不受影响。

### 从源码构建

需要 Rust ≥ 1.90 + MSVC Build Tools、Node ≥ 20（仅构建时需要）与 WebView2 Runtime。

```powershell
npm install        # esbuild / typescript / @tauri-apps/api
npm run build      # 构建 UI + release 二进制
npm run binary     # 落到 release/dshell.exe，并断言它是单文件
npm run app        # 开发模式运行
npm test           # Rust 单元测试
npm run check      # clippy -D warnings
npm run icons      # logo-tile.svg → PNG → icon.ico（需要本机 Chrome/Edge）
```

### 快捷键

| 快捷键 | 动作 |
|---|---|
| `Ctrl+T` | 新建连接 |
| `Ctrl+W` | 关闭当前选项卡 |
| `Ctrl+Tab` / `Ctrl+Shift+Tab` | 下一个 / 上一个选项卡 |
| `Ctrl+1` … `Ctrl+9` | 跳到第 N 个（`Ctrl+9` 为最后一个） |
| `Ctrl+R` | 重新加载当前选项卡 |
| `Ctrl+,` | 打开设置 |
| 双击标签 | 重命名（dshell 自己的页面标签除外） |
| 中键点击标签 | 关闭 |

### 调试

```powershell
$env:DSHELL_DEBUG='1'
.\dshell.exe --data-root E:\tmp\dshell-data
# → <数据根>\logs\dshell-debug.log
```

## 边界

- 只支持 Windows 10/11；架构不排斥其他平台，但首版只验收 Windows。
- 不随附 Node，也不代管模型凭据；API Key 仍在 DSH 自己的设置里。
- 不读 `~/.dsh/.credentials.yaml` 自行铸造 cookie。
- 「启动本地」首次使用 npx 需要下载官方发行版（约 450 MB），之后走缓存。
- 首版不做自动更新。

## 文档

- [English README](docs/english.md) — 同一份说明的英文版。
- [ARCHITECTURE.md](ARCHITECTURE.md) — 多 webview 的窗口模型、鉴权边界、进程生命周期与失败模式。

## 许可

MIT © 2026 Sturmfy —— 见 [LICENSE](LICENSE)。
