# 发版

dshell 的版本只有一个真源：`package.json`。另外三处（`src-tauri/tauri.conf.json`、
`src-tauri/Cargo.toml`、`src-tauri/Cargo.lock`）由脚本同步，**不要手改**。

```
npm version <patch|minor|major> -m "release: v%s"
git push --follow-tags
```

`npm version` 会依次：跑 `tools/set-version.mjs`（同步四处）→ 由 version 脚本把
`tauri.conf.json` / `Cargo.toml` / `Cargo.lock` `git add` 进来 → 提交 → 打 `vX.Y.Z`
tag。注意 npm 只自动提交 `package.json` 与 `package-lock.json`，所以那三个文件必须
在 version 脚本里显式 stage，否则它们会留在工作区、tag 指向一个版本不一致的提交。
推送 tag 后，GitHub Actions 的 `release` 工作流构建 Windows/macOS 产物并创建 Release。

## 版本号规则

SemVer 2.0.0，0.x 阶段：

| 改动 | 升哪一位 | 例子 |
|---|---|---|
| 只修 bug | patch | 0.1.0 → 0.1.1 |
| 新功能 / 有感知的行为变化 | minor | 0.1.1 → 0.2.0 |
| 不兼容改动（0.x 允许放进 minor） | minor | 0.2.0 → 0.3.0 |
| 承诺兼容之后才有 | major | 1.x → 2.0.0 |
| 测试版 | 预发布后缀 | 0.2.0-beta.1、0.2.0-rc.1 |

- tag 一律带 `v` 前缀：`v0.2.0`。
- 已发布的 tag **不可移动、不可复用**；发错了就发下一个 patch。
- 0.2.0 之后，`config.json` 的 `schemaVersion`、数据根查找规则、`--data-root`
  这类参数变化都算“对外契约变化”，至少升 minor，并写进 Release notes。
- 不使用日期版本、四段版本、`+build` 元数据。预发布 tag 里的版本号要原样写进
  四处文件；macOS 打包会自动取其中的数字核心（`0.2.0`），因为
  `CFBundleShortVersionString` 只接受数字。

## 本地校验

```
npm run version:check            # 四处是否一致
npm run version:check -- v0.2.0  # 再加“tag 必须等于这个版本”
npm test                         # 含 crate 版本与 tauri.conf 的漂移测试
npm run test:version             # 版本脚本自身的单测
```

## CI 行为

- `.github/workflows/ci.yml`：push/PR 到 `main`，Windows/macOS 编译 + `cargo test` +
  clippy + bridge 测试。移植期它就是红灯。
- `.github/workflows/release.yml`：
  - `push` 一个 `v*` tag → 校验版本 → Windows/macOS 构建 → 创建 Release（默认 draft）。
  - `workflow_dispatch` 可以只构建不发布（`publish=false`），用于移植期自测：
    产物在 Actions 的 workflow artifacts 里，不会碰 Releases。
  - tag 含 `-`（如 `v0.2.0-beta.1`）自动标为 prerelease。
  - Release 附件：`dshell-<ver>-windows-x64.exe`、
    `dshell-<ver>-macos-universal.dmg`、
    `SHA256SUMS.txt`。

## 发布前检查单

1. `npm run version:check` 通过；`npm test`、`npm run check` 通过。
2. Windows 与 macOS 冒烟（见 `docs/platforms.md`）全绿；任一失败就不要打 tag。
3. `npm version ...` → `git push --follow-tags` → 等 Actions。
4. 打开 draft Release，核对附件名、大小、`SHA256SUMS.txt` 与 notes，再点发布。
5. 发布后把 README 的下载段与 `docs/platforms.md` 的“最近验证版本”更新（如有变化）。

## 回滚

- 已发布的 Release 有问题：不要删 tag。发新的 patch，并在 notes 里写明
  “0.2.0 的某个问题已修复”。
- Release 附件上传错（tag 还没发布、仍是 draft）：删掉 draft 重跑工作流即可。
