# 平台

当前发布 Windows 与 macOS 两个平台。Linux 的代码路径已经就绪，但**暂不发布、
不跑 CI、不提供产物**（见最后一节）。发版前按冒烟清单在真实设备上验收。

## 产物

| 平台 | 产物 | 形态 |
|---|---|---|
| Windows 10/11 | `dshell-<ver>-windows-x64.exe` | 单文件，无 DLL / 无安装器 |
| macOS 11+ | `dshell-<ver>-macos-universal.dmg` | `.app`（arm64 + x86_64），不是单文件 |

Windows 的「单文件」承诺只适用于 Windows：macOS 的 `.app` 必须是一个目录，
因此打成 dmg 分发。

## 依赖

### Windows

- Windows 10/11 + WebView2 Runtime（系统一般已自带）。
- 可选：Node.js（`^22.19 || >=24`），仅「启动本地」的 npx 后端需要。

### macOS

- macOS 11 或更新。
- 无额外运行时（WKWebView 随系统提供）。
- 从源码构建需要 Xcode Command Line Tools；通用包需要两个 Rust target：
  `rustup target add aarch64-apple-darwin x86_64-apple-darwin`。

## 已知差异

| 能力 | Windows | macOS |
|---|---|---|
| 原生目录选择器（DSH 页面内） | dshell 托管（WebView2 bridge） | DSH 自己的选择器 |
| 拖入文件的真实路径（`@文件` chip） | 支持 | 回退为浏览器上传 |
| 内容页右键菜单 | 过滤后退 / 前进 / 打印 | 原生 WebKit 菜单 |
| 关闭到托盘 | 支持 | 支持（菜单栏图标） |
| 「在 DSH 内容页保持动画播放」 | 生效（Chromium 开关） | 设置项不显示 |
| 数据根（第 4 级回退） | `%LOCALAPPDATA%\dshell` | `~/Library/Application Support/dshell` |
| 便携形态 | exe 同级 `dshell-data/` | `.app` 同级 `dshell-data/` |

macOS 从网上下载的未签名包会被 Gatekeeper 拦截：右键「打开」，或

```sh
xattr -dr com.apple.quarantine /Applications/dshell.app
```

## 发布前冒烟清单

每个平台都要在**真实设备**上跑一遍；CI 只验证编译、单测与打包。

### Windows

- [ ] 双击 exe 能启动；无控制台窗口闪现
- [ ] 新建连接 / 本地 npx 启动 / 连接已有实例
- [ ] 选项卡新建、切换、关闭、重命名；`Ctrl+W` / `Ctrl+T`
- [ ] 托盘图标与菜单；关闭到托盘；退出
- [ ] 原生目录选择器；拖入文件得到真实路径
- [ ] `--data-root` 与 `%LOCALAPPDATA%` 回退

### macOS

- [ ] dmg 安装到 `/Applications` 能启动（arm64 与 Intel／Rosetta 各一次）
- [ ] `lipo -info dshell.app/Contents/MacOS/dshell` 含两种架构
- [ ] 选项卡、`⌘W` / `⌘T`、菜单栏图标
- [ ] 文件选择器；`~/Library/Application Support/dshell` 数据根
- [ ] 未签名包按上面的 Gatekeeper 步骤可用

## Linux：暂不发布

Linux 的代码路径（WebKitGTK、XDG 数据根、`/proc/net/tcp` 扫描、AppImage
打包配置）已经存在，但当前**没有 CI、没有产物、没有验收**：

- 从 `ci.yml` / `release.yml` 的矩阵里移除，避免出现“红了但先不管”的流水线；
- 恢复发布前，必须先有真实的 Linux 冒烟结果，再同时加回 CI 与 release 矩阵；
- 在那之前，`docs/` 与 README 不承诺 Linux 可用。

## 最近验证版本

发版后在这里记录实际验收过的系统版本（没有记录 = 未验收）。

| 平台 | 版本 | 日期 |
|---|---|---|
| Windows 10/11 | 待首次双平台发布 | — |
| macOS（arm64、Intel） | 待首次双平台发布 | — |
