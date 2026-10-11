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
| **Chrome 面** | dshell 自己的选项卡条与面板 | `http://tauri.localhost` | ❌ 也不需要 | ✅ **只有它有** |
| **Content 面** | DSH 原生前端，每选项卡一个 | `http://127.0.0.1:PORT` | ✅ 顶层文档，天然同源 | ❌ **一个都不给** |
| **Core 面** | dshell 的 Rust 后端 | 进程内 | 只做无凭据探测（`GET /` 期望 401） | — |

Content webview **不在任何 capability 里**：一旦它导航到 DSH origin，
它就是一个 *remote* 上下文，什么都调不到。这既满足需求，也顺手关掉了
「DSH 前端被替换/被诱导后拿到本地能力」这条路。

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
npm run app            # 开发运行（debug）
npm run check          # clippy -D warnings
```

产物：`src-tauri/target/release/dshell.exe` —— **单个文件，拷走即可运行**。
前端资源通过 Tauri 的 asset embedding 编进 exe，运行期不读任何磁盘资源目录。

调试用的额外参数：

```powershell
.\dshell.exe --data-root E:\tmp\dshell-data   # 覆盖数据根，便于多实例/回归测试
```

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

代价是无法依赖 z-order（Tauri 子 webview 本来也没有 z-order API），所以
**任何时刻只有一个 content webview 位于可见区域**，dshell 自己的面板也是
「把 chrome webview 长高」而不是叠一层新 webview。

---

## 8. 代码地图

```
web/                 chrome UI（选项卡条 + 面板 + 启动/错误页）
  app.ts             单状态对象 + 全量重渲染；所有变更都走 invoke
  shell.ts           content webview 里的加载页 / 失败页
  ipc.ts             invoke 与事件的全部类型化封装（Rust 契约只声明一次）
src-tauri/src/
  root/              ★ 数据根与四级回落链（唯一解析点）
  config.rs          config.json / tabs.json 模型与校验
  runtime/           ★ 四种后端的统一抽象 + Node 探测 + --help 能力探测
  dsh/               launch URL 解析（含 token 脱敏）、进程监管、无凭据探测
  tabs/              ★ 唯一允许碰 multi-webview API 的模块
    manager.rs        选项卡管理、webview 编排、事件泵
    layout.rs         几何计算（纯函数，有测试）
    state.rs          TabSpec（落盘）/ TabRuntime（仅内存）
  commands.rs        invoke 面（全部 async）
  win.rs             Job object、广播环境变量变更
  env.rs             写 HKCU\Environment、可写性探测
  dialog.rs          原生目录/文件选择器
```

### 三条代码级硬规则

1. **任何操作 webview 的 command 必须是 `async fn`。** Windows 上在同步命令或
   事件处理器里创建 webview、读 cookie 都会**死锁**（wry#583）。
2. **只有 `tabs/manager.rs` 可以调用 multi-webview API。** 把 Tauri 的
   `unstable` feature 的爆炸半径限制在一个可评审的文件里，升级或回退都只动这里。
3. **数据根的解析只有一处**（`root/mod.rs`）。其他地方一律从 `AppState` 取值，
   不允许自己拼 `DSHELL_HOME`。

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

- **快捷键需要 chrome webview 有焦点**。焦点在 DSH 内容区时，`Ctrl+T` 之类会
  被 DSH 页面自己接收。设计文档建议的"隐藏窗口菜单加速键"在 Tauri 2 的 Windows
  实现里没有可靠路径，因此本版**不声称**解决了这一点；点击选项卡条即可把焦点
  交回 dshell。这一点在 `docs/` 的设计里是被标为待验证的 S8。
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
- **简化**：设计里的 `tab-shell.html` / `tab-error.html` 两个资源合并为
  `shell/placeholder.html` / `shell/error.html`，且失败详情由页面**回调**
  Rust 获取（错误页是 dshell 自己的资源，所以它真的能 `invoke`）。
- **放弃**：隐藏窗口菜单加速键（见上文「已知限制」）。
- **单文件**：设计文档说"没有官方单文件方案"。本实现通过「Tauri 不启用 bundle
  + 前端资源 asset embedding + WebView2Loader 走静态导入库」达到单个 `.exe`。
