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
| **Chrome 面** | dshell 的选项卡条 + dshell 自己的页面 | `http://tauri.localhost` | ❌ 也不需要 | ✅ **只有它们有** |
| **Content 面** | DSH 原生前端，每选项卡一个 | `http://127.0.0.1:PORT` | ✅ 顶层文档，天然同源 | ❌ **一个都不给** |
| **Core 面** | dshell 的 Rust 后端 | 进程内 | 只做无凭据探测（`GET /` 期望 DSH 的鉴权页，见「发现本机已运行的实例」） | — |

**窗口里每一个页面都是一个选项卡** —— 包括 dshell 自己的页面（首启向导、新建连接、
设置）。它们没有自己的布局区域，就是内容区里的一个选项卡，因此**焦点、快捷键、
激活与关闭只有一套规则**，而不是两套。

IPC 权限由**两条**同时成立的条件决定（`capabilities/dshell-ui.json`）：

1. **webview label**：内容 webview 一律叫 `tab-*`，dshell 自己的页面叫 `page-*`。
   capability 只匹配 `chrome` 与 `page-*`，所以内容 webview **一个权限都没有**；
2. **当前 origin 必须是 dshell 自己的**（`local` 默认为 true）。即使某个页面被导航到
   DSH origin，它也会**立刻失去全部权限**。

第 1 条关掉了「DSH 前端被替换/被诱导后拿到本地能力」这条路；第 2 条是它的纵深防御。
名字在这里是安全边界而不是命名习惯，所以 `tabs/state.rs` 里有单元测试断言
`page-*` 与 `tab-*` 永不可能撞车。

### DSH 内容页的 reduced-motion 例外

Windows 的“动画效果”关闭时，DSH 的 `prefers-reduced-motion` 会把“进行中”圆环
变成一段静态弧。`forceDshMotion`（默认 `true`）让 DSH content webview 带上
WebView2 的 `--force-prefers-no-reduced-motion`，圆环继续旋转。

这里刻意只作用于 `tab-*`：dshell 自己的 `page-*` 与顶部 chrome 仍跟随系统设置。
实现是 WebView2 的 browser argument，不是对 DSH DOM/CSS 的注入，因此
“内容区是 DSH 原生界面”这条边界没有被打破。

代价与边界：

- `--force-prefers-no-reduced-motion` 是 Chromium 内部开关，不是 WebView2 公开
  API；升级 WebView2 后需要回归验证，失效时圆环会回到静态弧。
- browser argument 在 WebView2 环境创建时固定，所以新选项卡读取新值，已打开的
  选项卡不会热更新；要让全部选项卡生效需重启 dshell。
- 默认开启等于替用户覆盖了系统无障碍偏好；设置页提供显式关闭入口，并且只
  影响 DSH 内容页。

### 窗口分区：互不重叠的两个区

```
 y = 0      ┌──────────────────────────────────┐
            │ chrome webview — 选项卡条 38px    │  拖拽区 + 窗口按钮（永远在）
 y = 38     ├──────────────────────────────────┤
            │ content webview — 当前选项卡      │  DSH 页面，或 dshell 自己的页面
            └──────────────────────────────────┘
```

**这是本应用唯一一段刻意"死板"的布局，因为它是窗口可拖动的前提。**
无边框窗口只能通过 `data-tauri-drag-region` 元素拖动，而那个元素在选项卡条里；
Tauri 子 webview 又没有 z-order API，所以"谁在上面"不能靠猜：
选项卡条永远在自己的区域里，任何页面都不可能盖住它。

`tabs/layout.rs` 里的单元测试直接断言这条不变量（`nothing_ever_overlaps_the_tab_strip`、
`there_is_exactly_one_content_region`），而不是靠算术正确性的人眼复核。

> **曾经存在的第三个区（panel）已删除。** 早期版本把「新建/设置/首启」做成选项卡条
> 下面一个 ≤380px 高的独立 webview，于是首启时选项卡条是空的、内容区是空的，
> 用户面对的是一个没有选项卡的窗口。现在这三者都是普通选项卡：
> `PageKind`（`tabs/state.rs`）定义它们，Rust 负责创建与去重（每个页面是单例），
> 页面自己通过 `page-*` 的 IPC 驱动自己的动作。

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
npm run icons          # logo-tile.svg → PNG → icon.ico（需要本机 Chrome/Edge）
```

产物：`release/dshell.exe` —— **单个文件（约 3.7 MB），拷走即可运行**。
前端资源通过 Tauri 的 asset embedding 编进 exe，运行期不读任何磁盘资源目录；
WebView2Loader 走静态导入库，所以没有任何随附 DLL。`npm run binary` 会实际清点
`release/` 目录并在出现多余文件时**失败**，而不是只在文档里声称。

> ⚠️ **改了 UI 一定要重新 build 二进制。** `dist/` 是编进 exe 的输入，而 cargo 默认
> 不知道这件事 —— 只跑 `npm run build:ui` 会得到一个**仍是旧 CSS/JS** 的可执行文件，
> 而 cargo 还打印 "Finished"。`src-tauri/build.rs` 里 `rerun-if-changed=../dist`
> 就是为了堵住这个坑；命令行手敲 `cargo build` 时请确认它真的 `Compiling` 了。
> 另外 `release/dshell.exe` 是 `npm run binary` 拷过去的，`target/release/` 里的那份
> 才是 cargo 刚产出的 —— 两者时间戳不一致时，以上面这条命令为准。

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
├─ config.json          dshell 的偏好（★ 记住的连接里含 token，见 §9）
├─ tabs.json            选项卡列表（不含 token：spawn 的 token 只在内存）
├─ window.json          主窗口几何（物理像素 + 显示器身份，见 §7）
├─ session.lock         上次退出是否干净（见 §7）
├─ runtimes/            本地维护的运行时
│  ├─ deepseek-harness/ 源码 checkout（默认放在 runtimes/ 下；仓库始终克隆进所选父目录的 deepseek-harness 子目录）
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

### `config.json` 的版本与迁移

`config.json` 带 `schemaVersion`，加载时把旧文件迁移到当前默认值并**立刻写回一次**，
所以文件与运行中的行为不会各说各话。目前只有一条迁移（`1 → 2`）：`killOnClose`
从 `false` 变为 `true`。旧版的默认值就是 `false`，而 `update_config` 会把整份配置
写盘 —— 于是任何动过设置的人磁盘上都有一个自己从未选择过的 `false`。迁移只做一次，
之后用户在「高级选项」里取消勾选，`false` 就是用户自己的决定，会被原样保留。

`closeToTray` 是**附加字段**，不参与迁移：旧文件没有它时 serde 取默认值 `true`
（关窗口收进托盘），下一次任何保存会把它写进文件；用户取消勾选后保存的 `false`
此后不会再被覆盖。这与 `forceDshMotion` 采用的先例一致。

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
解析、就绪判定、错误分类只写一份**。「新建连接」页只暴露其中两种（npx / 源码），
另外两种留在设置与 `managed:` / `external` 运行时 id 上：

| 方式 | spawn 形态 |
|---|---|
| npx | `npx --yes @deepseek-ai/dsh@<pin> web --no-open --port <端口>`（缓存重定向到数据根） |
| 源码 checkout | `node --import tsx/esm apps/cli/src/bin.ts web --no-open --port <端口>` |
| 本地 npm 安装 | `<数据根>/runtimes/npm/node_modules/.bin/dsh.cmd web …` |
| 其他后端二进制 | `<exe> web --no-open --port <端口>`（**完全不需要 Node**） |

`<端口>` 由 dshell 自己挑（`runtime/port.rs`），**不再传 `--port 0`**：交给系统
分配意味着从 Windows 动态端口区间（本机是 1024–15000）里随便拿一个，而其中一部分
端口被所有 Chromium 客户端禁止连接（IRC 的 6665–6669、X11 的 6000、Amanda 的
10080 …）。一旦抽中，后端正常服务、页面却只能是 `ERR_UNSAFE_PORT`，看上去像装坏了。
所以 dshell 在 20000–29999（不在常见动态端口区间内）里挑一个**不在 Chromium 受限
端口表里**、并且当场 bind 验证过空闲的端口；真被占用的极端情况仍由既有的
「端口被占用 → 换端口重试」失败分类兜住。就绪行读回来的端口若仍然受限（老版本 DSH
不接受 `--port`），监视线会直接报一条可重试的失败，而不是把打不开的 URL 交给选项卡。

### 启动前的准备（在新建连接页面上完成，不在选项卡里）

「启动」不是一个瞬时动作：两种后端都可能需要先下载/构建。这些工作**在发起它们的
页面上完成**，页面在准备期间**整页禁用**（模态遮罩、无取消按钮），进度通过
`dshell://prepare-progress` 实时上报（阶段 + 子进程输出尾行）：

| 运行时 | 准备流程 | 何时跳过 |
|---|---|---|
| npx | `npx --yes <spec> --version` 把包装进 `<数据根>/runtimes/npx-cache` | 目标版本已在缓存里（`state == ready`） |
| 源码 | 在 `<所选父目录>/deepseek-harness` 里：`git clone --depth 1` → `git fetch` + 比较 → `git reset --hard FETCH_HEAD` → `pnpm install` → `pnpm run build`（前端产物仍缺时再 `pnpm run build:web`）→ 校验入口与 `apps/web/dist/index.html` | **从不跳过更新检查**；只有 `pnpm run build` 在"没拉到新提交且前端已构建"时省略 |

**所选目录只是「父目录」，仓库永远克隆进其中的 `deepseek-harness` 子目录**：用户挑的
往往是 `E:\projects` 这种本来就有别的项目的目录，dshell 不会占用它、不会清空它，
"删掉源码版"也只是删掉那一个子目录。默认父目录是 `<数据根>/runtimes`。

两条保护：**有未提交修改的 checkout 不会被 `reset --hard`**（跳过更新并提示）；
**断网只降级为"按现有提交启动"**，不会把一个能用的 checkout 变得不可用。

### 「已经有实例在跑」的处理

本地启动前若 dshell 正在监管一个本地后端，则默认**拒绝**并列出它（可「切换过去」），
用户勾选确认后才继续（`NewTabRequest::Launch.confirmExistingRunning`）。理由：两个
`dsh web` 共用同一个 `DSH_HOME` 会并发读写 `profiles/` 与 `storages/`，官方未验证。

### 发现本机已运行的实例

「远程」这一步会扫一遍本机端口：`netstat -ano` 取 loopback 监听 → 对每个端口做一次
**无 token** 的 `GET /`，认 DSH 的鉴权页（`401` + `text/plain` + `no-store` + 正文以
`dsh web authentication required` 开头）——只看 401 会把任何要鉴权的服务都算成 DSH，
`403` 也不再认，因为 DSH 的围栏 403 是裸 `forbidden`。找到的实例分三类呈现：
dshell 自己启动的（有选项卡 → 「切换过去」）、有历史记录的同 origin（有 token →
「连接」）、其它（只能「填入地址」，因为 token 只存在于启动它的终端里）。

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

### 干净退出：`session.lock` 的语义

启动时写 `<数据根>/session.lock`，`RunEvent::Exit` 里删除。**真正退出** dshell 的路径
（托盘菜单的「退出」、把「关闭窗口时最小化到托盘」关掉之后的窗口关闭、`app.exit()`）
都会走到 `Exit`；崩溃与强杀不会。默认的「关闭按钮 / Alt+F4」现在只是**隐藏到托盘**，
进程仍在运行，因此不会删标记 —— 这是正确的：它本来就不是一次退出。所以"下次启动
还看到标记"仍然等于"上次没走正常退出"，而不是反过来：**删标记**才是这条提示可信的前提。

标记带 `format` 版本。旧版本只写文件、从不删除，它留下的标记无法区分正常退出与
崩溃，因此不作判断 —— 否则升级后第一次启动会误报，看起来和这个 bug 一模一样。
提示本身是右上角、可手动关闭的 toast，且除关闭按钮外不接收点击，不会挡住页面按钮。

### 可见性策略

非活动选项卡的 content webview**不移出 DOM、不 `hide()`、也不设成 0 尺寸**，
而是**移出可视区域**：隐藏会触发浏览器后台节流，而 DSH 是长连接 agent 会话，
后台 tab 被节流意味着丢事件或审批请求卡住。

代价是无法依赖 z-order（Tauri 子 webview 本来也没有 z-order API），所以布局靠
**固定分区互不重叠**保证：任何时刻只有一个 content webview 在可见区，而
dshell 自己的三个区（选项卡条、面板、内容）由 `tabs/layout.rs` 算出边界，
彼此永不交叠。

### 托盘与关闭行为

托盘是**原生菜单**（`tray.rs`，Tauri 的 `tray-icon` feature），不是选项卡条里那个
`menu.html` 弹窗。原因很直接：它必须在主窗口隐藏时也能右键弹出，而 38px 高的子
webview 连自己的弹窗都画不出去；原生菜单还能把状态圆点作为菜单项图标交给
Windows 按 DPI 绘制。

- **关闭策略**：`CloseRequested` 里若 `closeToTray`（`AppState` 的原子镜像）为真
  且托盘真的存在，就 `prevent_close()` + `hide()`；否则不拦截，走原有的「销毁主
  窗口 → 关闭菜单弹窗 → 事件循环退出」。两个条件缺一不可：偏好为真但托盘创建
  失败时，隐藏窗口会让用户没有入口把它叫回来。
- **菜单内容**：只列连接选项卡（`pageKind == null`），按选项卡条顺序；页面选项卡
  （首次配置 / 新建连接 / 设置）不是连接，不出现。每项前面是状态圆点，颜色与
  `web/strip.ts::dotClass` 使用同一优先级（attention error > needsInput > completed
  > phase）；底部固定「退出」。圆点由运行期生成的 32×32 RGBA 图提供，颜色逐字取自
  `styles.css` 的 DSH 令牌。
- **刷新**：`dshell://tabs-changed` / `dshell://tab-changed` 触发 `tray::refresh`；
  串行重建，并只在 `(id, title, dot)` 投影变化时调用 `set_menu`，所以后台轮询
  报告 `running` 计数不会反复重建原生菜单。菜单 id 只含 tab id，标题来自
  `TabView`，因此和 tabs.json 一样不含 token。
- **点击**：左键/双击恢复并聚焦窗口；连接项先恢复窗口，再校验 tab 仍在才
  `activate`（菜单打开期间 tab 可能已关闭）；「退出」调用 `app.exit(0)`，走
  `RunEvent::Exit` 清理 `session.lock`。
- **未查看提醒**：任一连接选项卡的 `TabView::attention` 非空（error / needsInput /
  completed，优先级与选项卡条一致）时，托盘图标在普通图标与带红/黄/绿角标的图标
  之间每 600ms 交替闪烁；`attention` 清空后恢复普通图标。闪烁循环只启动一个，并由
  `BLINK` 的 generation 串行化「一帧正在写图标」与「停止并恢复普通图标」，避免清空
  提醒后残留一个角标。注意这里的判定只看 `attention`，启动中/就绪但无未读消息的
  选项卡不会让图标闪。
- **标题清洗**：折叠空白、空标题回退为「未命名」、超长截断、`&` 转义为 `&&`
  （Windows 菜单助记符），避免会话标题变成菜单快捷键或撑破菜单。

### 窗口几何：关闭前记录，下次回到同一块屏幕

主窗口的还原矩形、最大化标记和所在显示器记在 **`<数据根>/window.json`**，不在
`config.json` 里。两个原因：

1. **它必须在关闭的瞬间同步写盘。** `CloseRequested` 在主线程上运行，而 `config.json`
   只能经 `async` command 在 tokio 锁里写；为了存一个矩形去等锁可能阻塞事件循环，
   也可能和一个正在等主线程的任务互相等待。`window.json` 只有几百字节，
   `std::fs::write` 一次写完 —— 和 `session.lock` 是同一个先例。
2. **它是状态，不是偏好。** 和 `tabs.json` 一样，「上次窗口在哪」不是用户设置；文件
   写坏了最多让下一次开窗回到默认位置，不会连累数据根、连接历史和 API 设置。
   `config.json` 的 `window` 只剩下**首次启动**的默认尺寸（此时还没有记录可读）。

记录的是**物理像素 + 所在显示器的身份**（`\\.\DISPLAYn` 名字 + 那块屏当时的矩形），
不是逻辑坐标：逻辑坐标离开 DPI 没有意义，而 dshell 经常跑在两块 DPI 不同的屏幕上。
矩形是窗口外框左上角 + 客户区大小（无边框窗口两者只差 DWM 的不可见边框）。

**只记「还原」矩形。** 最大化时 Windows 报告的是最大化矩形，把它当成还原矩形存下来，
下次「还原」就会留下一个整屏大小的窗口。`window_state::Tracker` 在 `Moved` / `Resized` /
`ScaleFactorChanged` 里维护一份内存副本，只用**没最小化、没最大化**时读到的矩形覆盖它，
最大化标记单独记。这里必须问 Win32 的 `IsZoomed` / `IsIconic` 而不是窗口库的缓存标记：
最大化时 `WM_WINDOWPOSCHANGED`（移动事件）先于更新标记的 `WM_SIZE` 到达，而窗口此时
已经有了最大化矩形 —— 相信库的标记就会把那个矩形当成还原矩形记下来。最小化同理，
窗口会被报告到 `-32000`。

**还原顺序**（`window_state::restore`，纯函数，有单测）：

1. 按设备名找同一块屏幕 —— 显示器换位置、换分辨率、重新插拔后名字不变，这是「回到
   正确的那块屏」最可靠的依据；
2. 名字对不上（重装驱动 / 平台没有设备名）就按保存的显示器矩形找重叠最大的那块；
3. 再按窗口中心、窗口与显示器的重叠；
4. 都没有（副屏被拔掉）就落到主显示器。

找到目标屏后，把窗口放在**相对该屏左上角的同一偏移**上。只有在目标屏的矩形与保存时
不一致（换分辨率、旋转、或干脆是另一块屏）、或者记录的矩形让窗口顶部那条拖拽区完全
离开屏幕时才把它夹回工作区（必要时缩到工作区内）—— 否则「窗口上次在右边缘」会在更小
的屏上变成「窗口在屏幕外」，而 `window.json` 是可以手改的。屏幕完全没变时不做任何
「修正」：故意探出边缘的窗口也原样还原。

写入只有两处，都是同步的：`CloseRequested`（关闭按钮 / Alt+F4；即使随后只是收进托盘
也要先记），以及 `RunEvent::Exit`（托盘「退出」、`app.exit()`、托盘没装成功时的关闭
路径）。托盘「退出」不经过 `CloseRequested`，所以 `Exit` 那次不是保险而是必需。
崩溃或强杀不会经过这两处，上一份记录因此保持完整。

窗口以 `visible(false)` 创建，几何在显示之前用**物理坐标**设置好，再 `show()`；否则会
先看到操作系统默认位置，然后跳一下。`tao` 的 `set_size` 对从未显示过的窗口会按它当时
还能看到的框架换算外框尺寸（Windows 11 上多出一整个标题栏的高度），因此 `apply` 会
调用两次：第一次让框架定型，第二次才精确落在请求的客户区大小上。

---

## 8. 代码地图

```
web/                 dshell 自己的界面（三个独立文档）
  strip.ts           选项卡条 —— 窗口的标题栏，永远在
  pages/             dshell 自己的页面，每个都是普通选项卡
    new.ts             新建连接（步骤一：本地/远程 → 步骤二：运行时或地址）
    onboarding.ts      首启向导（唯一不可关闭的选项卡）
    settings.ts        设置
    shared.ts          三个页面共用的小工具 + bootPage 启动契约
  store.ts           所有界面共享的唯一数据层（Rust 是唯一权威）+ PageStore
  ipc.ts             invoke 与事件的全部类型化封装（Rust 契约只声明一次）
  shell.ts           content webview 里的加载页 / 失败页
src-tauri/src/
  root/              ★ 数据根与四级回落链（唯一解析点）
  config.rs          config.json / tabs.json 模型与校验（含记住的连接）
  runtime/           ★ 四种后端的统一抽象
    node.rs            Node / npm / pnpm / git 探测
    npxcache.rs        npx 缓存里已有哪些版本 + npx 预热（下载/升级）
    source.rs          源码 checkout 的检测与 clone/install/build 流水线
    exec.rs            带输出流与超时的子进程执行（准备流水线共用）
  dsh/               launch URL 解析（含 token 脱敏）、进程监管、无凭据探测
    scan.rs            本机已在监听的 DSH 实例发现
  tabs/              ★ 唯一允许碰 multi-webview API 的模块
    layout.rs         几何计算：互不重叠的两区（纯函数，有断言测试）
    manager.rs        选项卡管理、webview 编排、事件泵、页面选项卡的创建与去重
    state.rs          TabSpec（落盘）/ TabRuntime（仅内存）/ PageKind
  commands.rs        invoke 面（全部 async）
  tray.rs            系统托盘：原生菜单、状态圆点、关闭到托盘策略
  win.rs             Job object、广播环境变量变更、窗口最大化/最小化状态
  window_state.rs    ★ 主窗口几何：window.json、物理像素与显示器匹配（纯函数 + 单测）
  env.rs             写 HKCU\Environment、可写性探测
  dialog.rs          原生目录/文件选择器
```

**为什么选项卡条仍是独立 webview**：它是窗口唯一的拖拽区与窗口按钮所在。
内容区可以显示 DSH 页面，也可以显示 dshell 自己的页面，但**永远不可能盖住它** ——
两个区互不重叠，所以不需要依赖任何 z-order 行为。

### 视觉：直接复用 DSH 的设计令牌

`web/styles.css` 里 `--dsw-static-*` 与 `--dsw-alias-*` 的取值**逐字抄自 DSH 的
`dsh-client-ui-theme`**（`design-platform.css` + 其 client bundle 的 alias 层），
浅色与深色两套都抄；dshell 自己的 `--bg` / `--fg` / `--border` / `--radius` 等名字
只是**绑到这些令牌上**，所以这个文件里每一个界面都是一步之遥指向 DSH 的调色板，
而不是手挑的近似色。

早先的版本是手挑的"差不多"灰阶加一个蓝色强调色 —— 和真正的 DSH 页面并排看，
差别一眼可见：dshell 的页面是**另一个产品**的配色。注意 DSH 的强调色**不是蓝色**：
`--dsw-alias-brand-primary` 浅色下近黑、深色下近白；整个体系里唯一的饱和色是
`--dsw-alias-state-business-primary`（DeepSeek 蓝），只用于焦点与选中。

滚动条同样照搬 DSH 的 `scrollbar.css`（5px、透明轨道、`radius: 999px`、
`--dsw-alias-scrollbar-bg-l1` 系列），**包括它那道 `@supports not
selector(::-webkit-scrollbar)` 闸门**：非 `auto` 的 `scrollbar-width` 会让 Chromium
丢掉该元素上所有 `::-webkit-scrollbar*` 规则（连 `:hover` 一起），所以两条路径
必须互斥而不是同时声明。

dshell 无法真的 `@import` 这些样式表：它们加载在另一个 webview、另一个 origin 里，
而 CSS 自定义属性不跨文档。抄值（而不是抄机制）是唯一的共享方式。

### 选项卡名称与会话状态

**用户重命名的是一个连接，而不是 DSH 的当前会话。** 名字同时写进三个地方：

| 位置 | 作用 |
|---|---|
| `tabs.json` 的 `customTitle` | 这个选项卡自己重启后原样恢复 |
| `config.json` 的 `tabNames[instanceKey]` | 同一 workspace/DSH_HOME（或同一 attach/remote origin）的新选项卡、以及从连接历史重连时匹配回来 |
| DSH 页面左上角的品牌文字 | 通过 bridge 注入替换，用户看到的标题与选项卡一致 |

只要 `customTitle` 存在，`on_document_title_changed` 只更新“当前会话标题”的
记录，**不再改选项卡标题** —— 这就是“切会话后名字被换掉”的根因。
重命名还会同步更新 `recentConnections` 里同一 origin 的 label，所以历史记录卡片
也显示新名字。

内联重命名编辑器在打开期间会**暂停选项卡条的整树重绘**：store 的 4 秒刷新、标签
事件、主题事件都不会把输入框从 DOM 里换掉。失焦只有在“整个窗口仍有焦点”时才提交；
弹出菜单窗口造成的失焦会保留编辑器并把焦点交还。

**状态点跟随 DSH 自己的会话行，而不是只看后端进程相位。** 内容页 bridge 读取
DSH 已经渲染的 `data-row-key="session:*"` 与状态点的 `data-state`，把
`error / warning / done / ongoing` 聚合成计数上报。优先级固定为
**出错 > 需要输入 > 已完成**；只有这三种“需要用户确认”的状态会让圆点做间断跳跃。
“已完成”采用 DSH 的 `completionUnread`（用户没看过的完成），另外 Rust 还会把
“后台运行时从 running 变为 0、且该选项卡当时不是活动页”的转换记成未读完成，
并在用户切回该选项卡时清除。

为了让后台选项卡仍然渲染 DSH 的会话行，parked webview 不再缩到 1×1：DSH 在
视口窄于 1024px 时会自动折叠侧栏，折叠后**根本不存在** session 行。parked 尺寸
因此固定为 **1024×1** 并移到屏幕外 —— 宽度刚好保持在断点以上，让 React 树和
会话行挂载；高度只有 1px，后台页面几乎不参与光栅化，而 SSE、审批轮询和状态点
仍然在跑。

### 四条代码级硬规则

1. **任何操作 webview 的 command 必须是 `async fn`。** Windows 上在同步命令或
   事件处理器里创建 webview、读 cookie 都会**死锁**（wry#583）。
2. **只有 `tabs/manager.rs` 可以调用 multi-webview API。** 把 Tauri 的
   `unstable` feature 的爆炸半径限制在一个可评审的文件里，升级或回退都只动这里。
3. **数据根的解析只有一处**（`root/mod.rs`）。其他地方一律从 `AppState` 取值，
   不允许自己拼 `DSHELL_HOME`。
4. **webview label 是安全边界**：内容选项卡 `tab-*`、dshell 页面 `page-*`，
   capability 只匹配后者。`state.rs` 里有测试断言二者不可能混淆。

### 「每个页面都是一个选项卡」的可见后果

| 场景 | 行为 |
|---|---|
| 首启 | 向导作为**选项卡**出现并占据焦点；选项卡条上它有一个 `✦` 图标 |
| 关闭向导（未完成） | **被 Rust 拒绝**（`closable == false`），关闭按钮不渲染，中键与 `Ctrl+W` 同样无效 |
| 完成向导 | 打开「新建连接」选项卡，然后关掉自己 |
| 关掉最后一个选项卡 | 自动开一个「新建连接」选项卡 —— 空内容区不是本应用的状态 |
| `Ctrl+T` / `+` | 打开或聚焦「新建连接」选项卡（单例） |
| `Ctrl+,` / 齿轮 | 打开或聚焦「设置」选项卡（单例） |
| 页面选项卡的重命名 / 重启后端 / 清空站点数据 | 右键菜单里不提供；Rust 也会拒绝重命名 |
| 重启应用后 | 页面选项卡**不落盘**，按配置重建（向导仅在未完成时重建） |

### 启动时到底开哪个选项卡

| 状态 | 打开 |
|---|---|
| 没有可恢复的选项卡，向导未完成 | 向导，**并激活** |
| 有可恢复的选项卡，向导未完成 | 向导，**在后台**（不打断用户已有的选项卡） |
| 没有可恢复的选项卡，向导已完成 | 新建连接，激活 |

### 职责边界：项目目录归 DSH

**dshell 不选项目目录，也不假装选。** DSH 的进程 `cwd` 只是它的*初始*工作目录，
项目和会话的工作根目录由 DSH 自己的界面决定。所以：

- 首启向导**没有**「选择工作目录」这一步（只有数据位置 + 环境检查）；
- 「新建连接 → 本地」**不问目录**：后端在用户主目录启动，说明文字明确写了
  「项目目录在 DSH 里选择」；
- 页面上唯一问路径的地方是**源码 checkout 的父目录**，那是 dshell 自己维护的
  产物位置（仓库落在其中固定的 `deepseek-harness` 子目录），与项目无关。

> 这不是小气，而是避免 dshell 制造一个假的必填项，让用户以为这里是决定项目归属的地方。

---

## 9. 安全边界

| 边界 | 规则 |
|---|---|
| 凭据（spawn 的实例） | 启动 URL 的 token 只在 Core 内存与 content webview 的导航请求里；不落盘、不进日志（日志行写入前 `token=***` 脱敏） |
| 凭据（记住的连接） | **刻意例外**：`config.json` 的 `recentConnections` 保存用户粘贴过的完整 URL（含 token），否则「历史连接」无法一键重连。这与 DSH 自己把签名密钥明文放进 `$DSH_HOME/.credentials.yaml` 属同一威胁类别、同一台机器；条目可以逐条删除 |
| tabs.json | 只存 `TabSpec`，结构上不可能含 spawn 的 token（含 token 的 `launch_url` 在另一个类型里，且只在内存） |
| Content 面 | **不授予任何 capability**：`tab-*` 不匹配任何 capability 的 label，且它导航到的 DSH origin 也不是 local |
| dshell 自己的页面 | 权限由 `page-*` label **与** local origin 两条同时决定；被导航到外部 origin 即失效 |
| 明文远程 | 需显式勾选确认，且选项卡上**持续**显示 ⚠。loopback 目标不算"远程"，也不需要确认 |
| `DSH_HOME` | 只接受绝对路径；相对路径在提交时就被拒绝 |
| `--host 0.0.0.0` | dshell 永不生成（DSH 本身也硬拒绝） |
| 源码 checkout | `git reset --hard` 只对**干净**的 checkout 执行；有本地修改时跳过更新 |
| 清空实例数据 | 用 `clear_all_browsing_data()`，而不是删目录 |

### 内容页桥（不经过 Tauri IPC）

DSH 的 content tab 仍然**没有任何 Tauri capability**。它在两个可选全局上读取 shell 能力：

- `__DSH_DIRECTORY_PICKER__.pick()` —— 打开 dshell 自己拥有的原生文件夹选择器；
- `__DSH_HOST_PATHS__.pathFor(file)` —— 返回拖入/选择文件的磁盘路径。

两者由 `src-tauri/src/bridge/content.js` 在文档开始时注入，经 WebView2 的
`chrome.webview` 消息通道与 `src-tauri/src/bridge/mod.rs` 通信。脚本启动时发
`ready`，宿主回 `hello`；收不到 `hello` 时 pick 在 1.5 秒内失败，而不是让
DSH 无声等待。宿主按每个 tab 的 `display_origin` 校验消息来源；不是当前 DSH
origin 的请求会被拒绝。`hostPaths` 只读取页面已经持有的 `File` 对象对应的
`ICoreWebView2File::Path`，不接受 JSON 里的任意路径，也不读文件内容。没有 Tauri
capability，因此内容页即使被导航到外部 origin，也拿不到 dshell 的命令。

`__DSH_HOST_PATHS__` 是尽力而为：路径尚未就绪时返回空串，DSH 按原逻辑上传；
粘贴文件不经过该桥（仍上传）。

内容页还会周期性报告 DSH 左侧边栏的 `background` / `foreground` / `mode`。宿主
校验 origin 和颜色格式后发 `dshell://tab-theme` 给 chrome；chrome 只把其中的
背景色、前景色和派生边框色应用到**选中** tab，未选中 tab 保持透明。DSH 切换
主题时 active tab 会自动跟随，而不是把颜色写死在 dshell 的 CSS 里。

内容 tab 的浏览器行为由同一个 WebView2 入口收紧：

- 内容 tab **只在可编辑控件上保留 WebView2 右键菜单**（输入框/文本域/
  `contenteditable`）；页面空白、链接、图片等位置由
  `ContextMenuRequested` 直接 `SetHandled(true)`，不弹浏览器菜单。可编辑区域
  的菜单再过滤掉 Back / Forward / Print。
- dshell 自己的 chrome 和 page tab 不需要浏览器菜单，默认右键菜单关闭。
- 选项卡右键改为 dshell 风格的 **Tauri 弹出窗口**（`menu.html`），位置跟随
  鼠标、失焦/点击外部关闭；不再受 chrome webview 38px 高度限制，也跨平台可用。
  这个窗口**只创建一次**：`RunEvent::Ready` 之后在后台任务里预热，之后每次右键
  只做「下发 payload + 定位 + show」，关闭是 `hide` 而不是 `close`。此前是每次
  右键新建一个窗口，等于把一整套 WebView2（浏览器进程、渲染进程、页面加载）放在
  交互路径上，这是菜单"弹得慢"的根因；`CloseRequested` 被拦下改成 hide，避免
  Alt+F4 把预热的窗口销毁。窗口尺寸也是页面自己报的：面板按 `width: max-content`
  排出内容尺寸，渲染后 `tab_menu_measured` 交给 Rust，Rust 先 `set_size` 再
  `show`（窗口此时仍是隐藏的），所以窗口边缘就是面板边缘，不会在下方/右侧留一圈
  透明却挡点击的空白；页面若没来得及上报，300ms 后用上一次测到的尺寸兜底。菜单配色跟随被右键那个 tab 上报的 DSH 侧栏色
  （`dshell://tab-theme` 的同一份数据），由 strip 随打开请求一起传给 Rust，再由
  payload 带到菜单页；跨窗口拿不到页面的 `backdrop-filter`，所以用的是侧栏色的
  实色近似，圆角/描边/悬停由前景色 `color-mix` 派生。
- `NavigationStarting` 读取 `NavigationKind`，取消 `BackOrForward`，覆盖鼠标
  侧键、`Alt+Left/Right` 和任何走浏览器历史的后退/前进。
- `Ctrl+P` 在 `AcceleratorKeyPressed` 中吞掉；**刷新和查找保留**（不关闭
  `AreBrowserAcceleratorKeysEnabled`）。
- 缩放、触摸板滑动、状态栏、release 的 DevTools 仍由 Wry 默认关闭。

---

## 10. 已知限制

- **内容页桥是尽力而为的**：`__DSH_HOST_PATHS__` 依赖 WebView2 把 DOM `File`
  映射回磁盘路径。拖入/选择的文件可用；粘贴文件不经过桥，仍上传；文件夹是否
  可用取决于 WebView2 是否把目录 `File` 当作磁盘项（不保证）。路径没就绪时
  DSH 按原逻辑上传，不会阻塞输入。
- **内容页桥 attach 失败时**只写 debug log：标签页继续工作，`pick` 在 1.5 秒内
  报“bridge unavailable”，`hostPaths` 按无路径上传；不会给 `tab-*` 增加 Tauri
  capability，也不会阻断标签页创建。
- **快捷键需要 dshell 自己的界面有焦点**（选项卡条，或某个 dshell 页面选项卡）。
  焦点在 DSH 内容区时，`Ctrl+T` 之类会被 DSH 页面自己接收。设计文档建议的
  "隐藏窗口菜单加速键"在 Tauri 2 的 Windows 实现里没有可靠路径，因此本版**不声称**
  解决了这一点；点击选项卡条即可把焦点交回 dshell。
- **dshell 页面的明暗跟随操作系统的 `prefers-color-scheme`**，而不是跟随 DSH 内部
  的主题设置。DSH 的默认偏好就是 `system`（读同一个媒体查询），所以默认情况下两侧
  一致；只有当用户在 DSH 里**手动**钉死一个与系统相反的明暗时，选项卡条与 dshell
  页面才会和 DSH 内容不一致。dshell 读不到那个偏好 —— 它在另一个 webview 的页面里。
  若将来要同步，只需在 dshell 页面 `body` 上加 `data-ds-dark-theme`：样式表里已经
  按 DSH 的约定留了这个选择器。
- **dshell 页面的选项卡标题是固定的**（首次配置 / 新建连接 / 设置），不跟随页面内容，
  也不可重命名 —— 重命名一个按类型重建的单例选项卡只会让下次启动时消失。
- **源码 checkout 模式**由 dshell 自己做安装与构建编排（见 §5）：clone / fetch / reset /
  `pnpm install` / `pnpm run build` 全部流式上报，并且**只在用户按下「启动」时**执行 ——
  检测路径（打开页面时）只读文件系统与 `git rev-parse`，不联网、不写盘。代价是这一页
  可能真的等上十几分钟（DSH 约 350 个 workspace），所以它有进度、有输出尾行、并且
  **整页禁用**而不是留一堆按钮让人误点。唯一的例外是**装 pnpm 本身**：环境报告里若探测到了
  `npm` 而缺 `pnpm`，那一行直接带一个「安装 pnpm」按钮（`npm install -g pnpm`，命令原样
  显示在下面）—— 因为它是唯一一条"用户自己也要跑、且一条命令就能修好"的依赖，
  而一个不带出路的 `⚠` 只会让首启的人卡在这里。按钮只在点击时执行，不在任何检测
  路径上偷偷写用户的全局 npm 前缀。
- **端口由 dshell 挑，不交给系统**（见 §5）：`--port 0` 会抽到 Chromium 禁止的端口
  （6666 已经真实发生过），所以固定端口设置项目前仍不接线 —— 端口每次都是合法且空闲的
  新值，而 dshell 每次都会重新捕获 token，`LocalSpawn` 本来就不需要"跨重启保持登录"。
- **shell 页面（加载页 / 失败页）目前没有 IPC**：它们在 `tab-*` 内容 webview 里，
  而那个 label 不在任何 capability 中，所以加载页的进度轮询与失败页的按钮拿不到
  返回值。这是既有的独立问题，与选项卡模型无关，未在本次改动中处理。
- **选项卡条里的弹出菜单画不出来**：右键菜单与溢出菜单（`»`）都是选项卡条自己
  DOM 里的 `position: fixed` 元素，而选项卡条是一个 38px 高的子 webview —— 子
  webview 会把自己的内容裁剪到自己的矩形内，菜单因此被画在可视区之外（`»` 上
  「所有选项卡」的菜单项确实已生成，只是看不见）。这与「页面即选项卡」无关
  （选项卡条一直只有 38px，菜单一直如此），但重构去掉了面板层之后，它再没有
  别的落脚点。修法是**原生弹出菜单**（`tauri::Window::popup_menu`，2.12 已有），
  而不是重新引入第二个布局区域。在那之前，这些操作只能走快捷键（`Ctrl+R` 重载、
  `Ctrl+W` 关闭、双击重命名），而「重启后端进程 / 清空站点数据 / 在系统浏览器中
  打开」暂时没有入口 —— 需要时请按原生菜单的方向补齐，不要再往选项卡条里塞
  DOM 弹层。
- **托盘菜单是原生菜单，圆点没有跳动动画**：颜色与状态优先级和选项卡条一致，
  但菜单项是 Windows 自己绘制的，无法承载 `tab-dot-jump` 那种 CSS 动画。圆点颜色
  也刻意固定为 DSH 语义色，不跟随 DSH 深浅主题（原生菜单自身会跟随系统主题）。
- **关到托盘后 WebView2 可能降低后台优先级**：窗口 `hide()` 后 Chromium 可能节流
  定时器/渲染，但 DSH 后端与 SSE 会话仍在运行；恢复窗口后状态点会按最新上报刷新。
  这与隐藏窗口换取“关闭不打断会话”的取舍是同一件事。
- **托盘创建失败时不会隐藏窗口**：此时关闭窗口仍走原来的退出路径，避免窗口消失后
  没有恢复入口；失败原因只写进 `DSHELL_DEBUG` 日志。
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
  `shell/placeholder.html` / `shell/error.html`。⚠️ 设计说它们能回调 Rust，
  但实际上 `tab-*` 不在任何 capability 里，所以这条并未生效（见「已知限制」）。
- **修正**：设计里「面板把 chrome webview 长高」会**同时吃掉选项卡条**，
  于是无边框窗口失去唯一的拖拽区、也失去窗口按钮。中途的实现改成选项卡条
  **下面**独立的 panel webview；**现在进一步删除该区域** —— dshell 自己的页面
  就是普通选项卡（见 §2、§8）。
- **修正**：设计里「窗口尺寸记忆用 `tauri-plugin-window-state` 或自己存 config」。
  实现是自己写 `window_state.rs`，并把几何放进 `window.json` 而不是 `config.json`
  （见 §7「窗口几何」）：多显示器 + 混合 DPI 必须自己决定匹配哪块屏，而关闭时要
  **同步**写盘，拿不到 config 那条只有 `async` command 才能取的锁。`config.json`
  的 `window` 因此只剩首次启动的默认尺寸（原先预留的 `x` / `y` 已删除）。
- **放弃**：隐藏窗口菜单加速键（见上文「已知限制」）。
- **单文件**：设计文档说"没有官方单文件方案"。本实现通过「Tauri 不启用 bundle
  + 前端资源 asset embedding + WebView2Loader 走静态导入库」达到单个 `.exe`。

---

## 12. 许可

MIT —— 见 [`LICENSE`](LICENSE)。

