# 平台

dshell 的三平台产物、依赖与已知差异。发版前按最后一节的冒烟清单逐项验收。

## 产物

| 平台 | 产物 | 形态 |
|---|---|---|
| Windows 10/11 | `dshell-<ver>-windows-x64.exe` | 单文件，无 DLL / 无安装器 |
| Linux x86_64 | `dshell-<ver>-linux-x86_64.AppImage` | 单文件 AppImage |
| macOS 11+ | `dshell-<ver>-macos-universal.dmg` | `.app`（arm64 + x86_64），不是单文件 |

Windows 的「单文件」承诺只适用于 Windows：macOS 的 `.app` 必须是一个目录，
Linux 的 AppImage 是自挂载的单文件镜像。

## 依赖

### Windows

- Windows 10/11 + WebView2 Runtime（系统一般已自带）。
- 可选：Node.js（`^22.19 || >=24`），仅「启动本地」的 npx 后端需要。

### Linux

- glibc ≥ 2.35（在 Ubuntu 22.04 上构建，覆盖 22.04/24.04 及更新的发行版）。
- X11 或 Wayland，GTK 3。
- WebKitGTK 4.1（`libwebkit2gtk-4.1-0`）。
- 托盘需要 `libayatana-appindicator3`；缺失时只是没有托盘图标，关闭窗口仍会退出。
- 构建依赖（Ubuntu）：

  ```sh
  sudo apt install libwebkit2gtk-4.1-dev libgtk-3-dev \
    libayatana-appindicator3-dev librsvg2-dev patchelf libfuse2
  ```

### macOS

- macOS 11 或更新。
- 无额外运行时（WKWebView 随系统提供）。
- 构建需要 Xcode Command Line Tools。通用包需要两个 Rust target：
  `rustup target add aarch64-apple-darwin x86_64-apple-darwin`。

## 已知差异

| 能力 | Windows | Linux | macOS |
|---|---|---|---|
| 原生目录选择器（DSH 页面内） | dshell 托管（WebView2 bridge） | DSH 自己的选择器 | DSH 自己的选择器 |
| 拖入文件的真实路径（`@文件` chip） | 支持 | 回退为浏览器上传 | 回退为浏览器上传 |
| 内容页右键菜单 | 过滤后退 / 前进 / 打印 | 原生 WebKit 菜单 | 原生 WebKit 菜单 |
| 「在 DSH 内容页保持动画播放」 | 生效（Chromium 开关） | 设置项不显示 | 设置项不显示 |
| 关闭到托盘 | 支持 | 支持（需要 appindicator） | 支持（菜单栏图标） |
| 数据根 | `%LOCALAPPDATA%\dshell` | `$XDG_DATA_HOME/dshell` | `~/Library/Application Support/dshell` |
| 便携形态 | exe 同级 `dshell-data/` | exe 同级 `dshell-data/` | `.app` 同级 `dshell-data/` |

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

### Linux

- [ ] `chmod +x dshell-*.AppImage && ./dshell-*.AppImage` 能启动
- [ ] 新建连接 / 本地 npx 启动（有 Node 时）/ 连接已有实例
- [ ] 选项卡模型正常（多 webview 的坐标、缩放、DPI）
- [ ] 托盘（appindicator 已装）；关闭到托盘
- [ ] 文件选择器打开；`--data-root` 生效
- [ ] 没有 Node 时「连接」仍可用，错误提示指向 Node 而不是 WebView2

### macOS

- [ ] dmg 安装到 `/Applications` 能启动（arm64 与 Intel／Rosetta 各一次）
- [ ] `lipo -info dshell.app/Contents/MacOS/dshell` 含两种架构
- [ ] 选项卡、`⌘W` / `⌘T`、菜单栏图标
- [ ] 文件选择器；`~/Library/Application Support/dshell` 数据根
- [ ] 未签名包按上面的 Gatekeeper 步骤可用

## 最近验证版本

发版后在这里记录实际验收过的系统版本（没有记录 = 未验收）。

| 平台 | 版本 | 日期 |
|---|---|---|
| Windows 10/11 | 待首次三平台发布 | — |
| Ubuntu 22.04 / 24.04 | 待首次三平台发布 | — |
| macOS 13 / 14（arm64、Intel） | 待首次三平台发布 | — |
