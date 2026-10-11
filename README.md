# dshell

DeepSeek Harness 的桌面入口：**一个独立二进制**，在一个窗口里用类似 Windows
Terminal / Chrome 的顶部选项卡承载多个 DSH 会话，内容区 100% 是 DSH 原生界面。

dshell 不重新实现 DSH，也不修改 DSH。它为每个选项卡启动或连接一个 `dsh web`
服务器，把服务器打印的**带 token 的启动 URL** 交给一个独立 webview 加载；之后
DSH 前端与官方后端之间的会话、`/api` RPC、SSE 全部在 webview 内原生完成。
dshell 只负责**进程生命周期 + 端口/token 捕获 + 选项卡布局**。

---

## 1. 为什么必须是「每个选项卡一个 webview」

这不是实现选择，而是唯一可行的方案：

- DSH 的会话 cookie 是 `HttpOnly; SameSite=Strict`，且**绑定 Host authority**；
- DSH 的 `/api` 信任围栏**拒绝 `Origin: null`**，也拒绝任何跨站来源。

因此：`<iframe>` 嵌不了，dshell 自己的前端（`http://tauri.localhost`）也
`fetch` 不了 DSH 的 `/api`。**只有让每个 DSH 页面自己成为顶层文档**，
origin 就是 `http://127.0.0.1:PORT`，一切才成立。

由此得到本项目最重要的不变量：

> **dshell 永远不站在流量的中间。** 凭据只出现一次：Rust 从 stdout 解析出
> URL → 交给 content webview 导航 → webview 自己兑换 cookie 并保存在自己的
> data directory 里。此后凭据（`HttpOnly`）对 dshell 完全不可见，也从不落盘。

---

## 2. 三个面（架构总纲）

| 面 | 承载 | origin | 能访问 DSH `/api` | 有 IPC 权限 |
|---|---|---|---|---|
| **Chrome 面** | dshell 的选项卡条 + 面板 | `http://tauri.localhost` | ❌ 也不需要 | ✅ **只有它们有** |
| **Content 面** | DSH 原生前端，每选项卡一个 | `http://127.0.0.1:PORT` | ✅ 顶层文档，天然同源 | ❌ **一个都不给** |
| **Core 面** | dshell 的 Rust 后端 | 进程内 | 只做无凭据探测（`GET /` 期望 401） | — |

Content webview **不在任何 capability 里**：一旦它导航到 DSH origin，
它就是一个 *remote* 上下文，什么都调不到。这既满足需求，也顺手关掉了
「DSH 前端被替换/被诱导后拿到本地能力」这条路。

### 窗口分区：互不重叠的四个区

```
 y = 0      ┌──────────────────────────────────┐
            │ chrome webview — 选项卡条 38px    │  拖拽区 + 窗口按钮（永远在）
 y = 38     ├──────────────────────────────────┤
            │ panel webview — 新建/设置/首启    │  仅面板打开时（≤380px）
 y = 38+h   ├──────────────────────────────────┤
            │ content webview — 当前 DSH 页面   │
            └──────────────────────────────────┘
```

**这是本应用唯一一段刻意"死板"的布局，因为它是窗口可拖动的前提。**
无边框窗口只能通过 `data-tauri-drag-region` 元素拖动，而那个元素在选项卡条里；
Tauri 子 webview 又没有 z-order API，所以"谁在上面"不能靠猜。
把面板做成**下面独立的一个 webview** 而不是覆盖整窗的浮层，两个问题一起消失：

- 选项卡条永远可见 → 窗口永远能拖、能最小化/最大化/关闭；
- 面板不需要自己的标题栏（选项卡条就是标题栏），也不会盖住拖拽区。

`tabs/layout.rs` 里的单元测试直接断言这条不变量（`nothing_ever_overlaps_the_tab_strip`），
而不是靠算术正确性的人眼复核。

---

## 3. 构建与运行

### 前置

| 依赖 | 版本 | 用途 |
|---|---|---|
| Rust | stable ≥ 1.90 | 构建 dshell 自身 |
| MSVC Build Tools | VS 2022+ | Tauri 只支持 MSVC triple |
| Node.js | ≥ 20（仅构建前端用） | `tools/build-ui.mjs`；**不随附、不打包** |
| WebView2 Runtime | Win11 预装 | 运行期依赖 |

终端用户只需要 **WebView2**：dshell 自己不随附 Node，也从不下载 Node。

### 命令

```powershell
npm install            # 只装 esbuild / typescript / @tauri-apps/api（类型与打包）
npm run build          # 构建 UI + release 二进制
npm run binary         # 把产物落到 release/dshell.exe，并断言它是单文件
npm run app            # 开发运行（debug）
npm run check          # clippy -D warnings
npm test               # Rust 单元测试（cargo test）
```

产物：`release/dshell.exe` —— **单个文件（约 3.7 MB），拷走即可运行**。
前端资源通过 Tauri 的 asset embedding 编进 exe，运行期不读任何磁盘资源目录；
WebView2Loader 走静态导入库，所以没有任何随附 DLL。`npm run binary` 会实际清点
`release/` 目录并在出现多余文件时**失败**，而不是只在文档里声称。

### 上游契约回归

dshell 依赖的上游行为（flag 集合、就绪行、token 兑换、前端标记、401 信号）由
一个无依赖脚本断言，升级 DSH 后先跑它：

```powershell
node tools/dsh-contract-probe.mjs --dsh <dsh>/lib/bin.js
```

### 诊断

```powershell
$env:DSHELL_DEBUG = '1'
.\dshell.exe --data-root E:\tmp\dshell-data
# → 在 <数据根>\logs\dshell-debug.log 里记录
#   能力探测、spawn 命令行、就绪行、就绪校验结果、导航目标、子进程退出与日志尾部
```

`--data-root` 覆盖数据根，便于多实例并行与回归测试。失败详情也可以直接在
选项卡的错误页里看到：可读原因 + 原始证据 + 下一步按钮。

---

## 4. 数据放在哪

两个根，刻意分开：

```
<数据根 = DSHELL_HOME>/
├─ config.json          dshell 的偏好
├─ tabs.json            选项卡列表（★ 不含任何 token）
├─ runtimes/            本地维护的运行时
│  ├─ npm/              npm 安装的 dsh
│  ├─ npx-cache/        npm_config_cache 重定向目标
│  ├─ node/             可选的便携 Node
│  └─ bin/              其他后端二进制
└─ webview-data/        每个实例一份 webview 存储（cookie / localStorage）
```

`DSHELL_HOME` 的**四级回落链**（每层都做可写性探测）：

1. `--data-root <path>`
2. `$DSHELL_HOME` 环境变量（首启向导写入 `HKCU\Environment`，**不用 `setx`**，
   因为后者有 1024 字符截断问题；写完广播 `WM_SETTINGCHANGE`）
3. `<exe 同级>/dshell-data/`（真便携默认）
4. `%LOCALAPPDATA%\dshell\`

设置页**始终显示最终生效的是哪一层**。

### `DSH_HOME` 默认**不设置**

这是本设计最容易踩坑、也最影响用户体验的一条：

| 用户选项 | dshell 的行为 | 后果 |
|---|---|---|
| **留空（默认）** | **完全不在子进程 env 里放 `DSH_HOME`** | DSH 落到 `~/.dsh` → **复用用户已有的会话、设置、API Key** |
| 独立状态 | `<数据根>/dsh-home/<工作目录名>/` | 全新空目录，**不含现有历史**（UI 明说） |
| 自定义绝对路径 | 注入该路径 | 用该目录的状态 |
| 相对路径 / 纯空白 | **拒绝** | DSH 会相对*子进程 cwd* 解析 → 落点漂移，灾难 |

理由很实际：用户机器上通常**已经**有一个 DSH 状态根。替它换根，用户第一眼
看到的是"历史全空了、还要重填 API Key"。`DSH_AGENTS_HOME` 同理默认不设置，
否则用户已在 `~/.agents` 维护的 skills / instructions 会失效。

---

## 5. 启动本地：四种后端，一个接口

`runtime` 模块把四种启动方式抽象成同一个 `LaunchPlan`，因此**进程监管、stdout
解析、就绪判定、错误分类只写一份**：

| 方式 | spawn 形态 |
|---|---|
| npx | `npx --yes @deepseek-ai/dsh@<pin> web --no-open --port 0`（缓存重定向到数据根） |
| 本地 npm 安装（推荐默认） | `<数据根>/runtimes/npm/node_modules/.bin/dsh.cmd web …` |
| 本地维护的目录 | 已安装依赖树 → 直接跑它的入口；源码 checkout → `node <entry> web …` |
| 其他后端二进制 | `<exe> web --no-open --port 0`（**完全不需要 Node**） |

**能力探测而非硬编码**：每次解析运行时前跑一次 `web --help`，只传该版本真的
支持的 flag（0.2.0-rc.2 遇到 `--public-url` 会直接 exit 1），结果按运行时缓存。

**「其他二进制」采用前必须过验证**：`--version` 可解析 → `web --help` 含
`--port` / `--no-open` → 才允许保存。用户完全可能指到一个旧的/魔改的/不相干的
程序，必须带原因拒绝，而不是让他在一个白屏前猜。

---

## 6. 就绪判定：三步，不是一步

```
1. stdout 出现严格匹配 ^dsh web: (https?://\S+)$ 的一行
   → 服务器已绑定端口、/api 已挂载
2. GET <url>（跟随 303 到 /，带 cookie jar）→ 期望 200 且正文含 __DSH_BOOT__
   → 404 就是 FailureCode::FrontendMissing
3. content webview 首次 PageLoadEvent::Finished → Ready
```

第 2 步不能省：**DSH 在前端 dist 缺失时照常打印就绪行、stderr 为空、进程存活，
但页面是 404**。只看第 1 步会把一个坏掉的安装判成"已就绪"。

第 3 步只用**首次**加载完成，因为 DSH 是 SPA：内部切换会话不触发页面导航，
`on_document_title_changed` 才是拿会话标题的唯一可靠途径。

---

## 7. 进程回收：作业对象

DSH 自己会 spawn 子进程（`pwsh` / `rg` / `git` …），而 Windows 的 `.cmd` shim
还会再套一层 `node`。只杀直接子进程会留下孤儿，继续占用工作目录和端口。

因此每次 spawn 都：

1. `CREATE_NO_WINDOW`（release 构建的 dshell 没有控制台，否则子进程会弹窗）；
2. 创建带 `KILL_ON_JOB_CLOSE` 的**作业对象**，把子进程放进去。

作业对象由**内核**在最后一个句柄关闭时（包括进程崩溃）回收整棵树 —— 这是唯一
在"直接强杀 dshell"场景下也成立的机制。

### 可见性策略

非活动选项卡的 content webview**不移出 DOM、不 `hide()`、也不设成 0 尺寸**，
而是**移出可视区域**：隐藏会触发浏览器后台节流，而 DSH 是长连接 agent 会话，
后台 tab 被节流意味着丢事件或审批请求卡住。

代价是无法依赖 z-order（Tauri 子 webview 本来也没有 z-order API），所以布局靠
**固定分区互不重叠**保证：任何时刻只有一个 content webview 在可见区，而
dshell 自己的三个区（选项卡条、面板、内容）由 `tabs/layout.rs` 算出边界，
彼此永不交叠。

---

## 8. 代码地图

```
web/                 dshell 自己的界面（三个独立文档）
  strip.ts           选项卡条 —— 窗口的标题栏，永远在
  panel.ts           面板：新建连接 / 设置 / 首启向导
  store.ts           两个界面共享的唯一数据层（Rust 是唯一权威）
  ipc.ts             invoke 与事件的全部类型化封装（Rust 契约只声明一次）
  shell.ts           content webview 里的加载页 / 失败页
src-tauri/src/
  root/              ★ 数据根与四级回落链（唯一解析点）
  config.rs          config.json / tabs.json 模型与校验
  runtime/           ★ 四种后端的统一抽象 + Node 探测 + --help 能力探测
  dsh/               launch URL 解析（含 token 脱敏）、进程监管、无凭据探测
  tabs/              ★ 唯一允许碰 multi-webview API 的模块
    layout.rs         几何计算：互不重叠的四区（纯函数，有断言测试）
    manager.rs        选项卡管理、webview 编排、事件泵
    state.rs          TabSpec（落盘）/ TabRuntime（仅内存）
  commands.rs        invoke 面（全部 async）
  win.rs             Job object、广播环境变量变更
  env.rs             写 HKCU\Environment、可写性探测
  dialog.rs          原生目录/文件选择器
```

**为什么 strip 与 panel 是两个 webview**：它们共享 `store.ts` 的数据层，但各自渲染。
这样选项卡条在面板打开时仍然存在（可拖动、可关窗），而两个 webview 永远不会重叠 ——
不需要依赖任何 z-order 行为。

### 三条代码级硬规则

1. **任何操作 webview 的 command 必须是 `async fn`。** Windows 上在同步命令或
   事件处理器里创建 webview、读 cookie 都会**死锁**（wry#583）。
2. **只有 `tabs/manager.rs` 可以调用 multi-webview API。** 把 Tauri 的
   `unstable` feature 的爆炸半径限制在一个可评审的文件里，升级或回退都只动这里。
3. **数据根的解析只有一处**（`root/mod.rs`）。其他地方一律从 `AppState` 取值，
   不允许自己拼 `DSHELL_HOME`。

### 职责边界：项目目录归 DSH

**dshell 不选项目目录，也不假装选。** DSH 的进程 `cwd` 只是它的*初始*工作目录，
项目和会话的工作根目录由 DSH 自己的界面决定。所以：

- 首启向导**没有**「选择工作目录」这一步（只有数据位置 + 环境检查）；
- 「启动本地」表单里的目录是**可选的**「起始目录」，留空即使用用户主目录，
  说明文字明确写了「项目目录在 DSH 里选择」；
- 只有用户真的选了目录，才会进「最近」列表 —— 默认值不算用户的选择。

> 这不是小气，而是避免 dshell 制造一个假的必填项，让用户以为这里是决定项目归属的地方。

---

## 9. 安全边界

| 边界 | 规则 |
|---|---|
| 凭据 | 启动 URL 的 token 只在 Core 内存与 content webview 的导航请求里；不落盘、不进日志（日志行写入前 `token=***` 脱敏） |
| tabs.json | 只存 `TabSpec`，结构上不可能含 token（含 token 的 `launch_url` 在另一个类型里，且只在内存） |
| Content 面 | **不授予任何 capability** |
| 明文远程 | 需显式勾选确认，且选项卡上**持续**显示 ⚠ |
| `DSH_HOME` | 只接受绝对路径；相对路径在提交时就被拒绝 |
| `--host 0.0.0.0` | dshell 永不生成（DSH 本身也硬拒绝） |
| 清空实例数据 | 用 `clear_all_browsing_data()`，而不是删目录 |

---

## 10. 已知限制

- **快捷键需要 dshell 自己的界面有焦点**（选项卡条或面板）。焦点在 DSH 内容区时，
  `Ctrl+T` 之类会被 DSH 页面自己接收。设计文档建议的"隐藏窗口菜单加速键"在 Tauri 2
  的 Windows 实现里没有可靠路径，因此本版**不声称**解决了这一点；点击选项卡条即可
  把焦点交回 dshell。
- **面板高度固定**（380px），不随内容自适应：chrome 面拿不到内容高度，
  让 webview 跟着内容长高需要在 Rust 与前端之间来回同步尺寸，收益不值这个复杂度。
  面板内部滚动。
- **源码 checkout 模式**只做到"检测 + 能启动已构建的 checkout"，不做
  `pnpm install` / `pnpm run build` 的编排。DSH 的 workspace 规模让构建编排本身就是
  一个大特性，值得单独做。
- **固定端口以保持登录态**是设置项，但默认关闭：`--port 0` 永不冲突，而 dshell
  每次都会重新捕获 token，所以 `LocalSpawn` 本来就不需要"跨重启保持登录"。
- 只验证了 Windows（架构不排斥其他平台，但 job object 之外的分支没有实测）。

---

## 11. 与设计文档的关系

`../docs/` 是完整调研与设计（含实测证据）。本仓库是对它的实现，其中：

- **保持不变**：三面分离、multi-webview、三步就绪判定、四后端统一抽象、
  `DSH_HOME` 默认不设置、作业对象、token 不落盘、`instance_key` 决定 webview
  存储位置。
- **修正**：设计里「首启选主要工作目录，它就是 DSH 会话的 workspace 根」这条
  **过度承诺**了。DSH 自己就会选项目目录，把进程 cwd 说成"会话的工作根目录"
  会让用户以为项目归属由 dshell 决定。现在目录是可选的「起始目录」，
  首启向导不再问它（见 §8「职责边界」）。
- **简化**：设计里的 `tab-shell.html` / `tab-error.html` 两个资源合并为
  `shell/placeholder.html` / `shell/error.html`，且失败详情由页面**回调**
  Rust 获取（错误页是 dshell 自己的资源，所以它真的能 `invoke`）。
- **修正**：设计里「面板把 chrome webview 长高」会**同时吃掉选项卡条**，
  于是无边框窗口失去唯一的拖拽区、也失去窗口按钮。现在面板是选项卡条
  **下面**独立的一个 webview（见 §2「窗口分区」）。
- **放弃**：隐藏窗口菜单加速键（见上文「已知限制」）。
- **单文件**：设计文档说"没有官方单文件方案"。本实现通过「Tauri 不启用 bundle
  + 前端资源 asset embedding + WebView2Loader 走静态导入库」达到单个 `.exe`。
