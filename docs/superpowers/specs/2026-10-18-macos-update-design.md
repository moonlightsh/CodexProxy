# CodexHelper macOS 内部升级渠道设计

- 日期：2026-10-18
- 状态：已实现（见 docs/superpowers/plans/2026-10-18-macos-update.md）
- 上游设计：`docs/superpowers/specs/2026-09-23-codex-helper-design.md` §15（Windows 升级渠道）
- 取代：`2026-10-14-codex-helper-macos-design.md` 决策 D6（"不接入内部升级渠道"）

## 1. 背景与目标

Windows 版已通过 `tauri-plugin-updater` 接入内网 Nexus 升级渠道（检测 / 下载 / Ed25519
签名校验 / NSIS 静默安装 / 自动重启）。macOS 版（v0.2.0 起）刻意延后了升级支持：界面隐藏
升级区，`check_update` / `install_update` 命令在非 Windows 平台直接拒绝。

本设计把升级渠道扩展到 macOS，**交互与安全模型尽量与 Windows 版一致**，让两个平台的
用户获得相同的"检查更新 → 确认 → 安装 → 自动重启"体验。

### 目标

- macOS 界面显示升级区，支持手动检查、启动静默自检、确认后安装、安装后自动重启。
- 发布链产出 macOS 升级产物（`.app.tar.gz` + `.sig`），`latest.json` 增加 darwin 平台键。
- 临时安装位置（dmg / App Translocation）不破坏升级体验：检测可用，安装被拦截并引导。

### 非目标

- Developer ID 签名与公证（维持 D2 ad-hoc，Ed25519 更新签名独立于 codesign）。
- Sparkle 框架或自研升级器（继续复用 `tauri-plugin-updater`）。
- macOS 自动安装（无确认直接升级）——与 Windows 交互保持一致：必须用户确认。

### 反目标

- 升级流程弄坏现有受管配置或 Keychain 凭据。
- 从 dmg / Translocation 临时路径原地替换导致"升级成功但装错了地方"或只读卷写入失败。
- 破坏 Windows 现有升级链路（同一份 `latest.json`、同一发布脚本必须继续服务 Windows）。

## 2. 已拍板的决策

| 编号 | 决策 |
| --- | --- |
| M1 | 复用 `tauri-plugin-updater` 的 macOS 路径：下载 `.app.tar.gz` → 解压临时目录 → 原地 rename 替换 `.app`；替换后由应用自行 `app.restart()`（插件在 macOS 不自动重启，重启时 Tauri 从原路径读新 `Info.plist` 拉起新版本，已验证 tauri-2.11.6 源码） |
| M2 | universal 产物一份，`latest.json` 写裸键 `darwin-aarch64` 与 `darwin-x86_64` 指向同一 URL（客户端查找顺序 `darwin-<arch>-app` → `darwin-<arch>`，裸键可命中，官方标准做法） |
| M3 | 升级区在 macOS 显示；**检查更新始终可用**（只读无副作用）；安装位置异常时"立即更新"禁用并提示，Rust 侧 `install_update` 同步拦截（纵深防御，与 `manager.rs` 受管开关的临时位置拦截同一思想） |
| M4 | 临时安装位置判定复用 `helper_core::paths::is_transient_install_path`，但基于主程序 `current_exe()`（替换目标就是主 `.app`；与 manager 用的 sidecar 路径同源等价） |
| M5 | `tauri.macos.conf.json` 的 `createUpdaterArtifacts` 改为 `true`；`build-macos.sh` 在未提供 `TAURI_SIGNING_PRIVATE_KEY` 时用临时 `--config` 覆盖为 `false`（本地开发构建不受影响），提供私钥时产出 `.app.tar.gz` + `.sig` 并自检 |
| M6 | `publish-update.sh` 改造为**目录扫描式**：在一个产物目录里自动识别 `*-setup.exe`（windows-x86_64）与 `*.app.tar.gz`（darwin 两键），合并生成单一 `latest.json`；发布者从 GitHub Release 下载两平台产物放入同一目录后在内网机器执行 |
| M7 | `release.yml` 的 macos-release job 注入 `TAURI_SIGNING_PRIVATE_KEY`，构建升级产物并随 dmg 一起上传 GitHub Release |
| M8 | codesign 维持 ad-hoc（`signingIdentity: "-"`）：Ed25519 更新签名保证完整性，Gatekeeper 首次放行体验与现状一致（tar 解包不携带 quarantine，升级后重开不弹"已损坏"） |

## 3. 平台行为差异（Windows 对照）

| 环节 | Windows（现状） | macOS（本设计） |
| --- | --- | --- |
| 检测地址 | `latest.json`（Nexus cypub，明文 HTTP + Ed25519） | 同一份 `latest.json`，新增 darwin 键 |
| 更新产物 | `*-setup.exe`（NSIS 安装器本体） | `*.app.tar.gz`（bundler 产出的 .app 压缩包） |
| 签名校验 | Ed25519 + `requireSignedVersion` | 同一机制，同一公钥 |
| 安装方式 | NSIS `installMode: passive` 静默安装器 | 解压 + 原地 rename 替换 `.app`；`/Applications` 无写权限时插件弹系统管理员授权（AppleScript），可接受 |
| 重启 | 插件经 NSIS 安装器重启 | 应用在 `download_and_install` 成功后自行 `app.restart()`（现有 `install_update` 末尾的 `app.restart()` 不变） |
| 位置约束 | 无 | 必须从正常位置运行（非 dmg / Translocation），否则拦截 |

## 4. 详细设计

### 4.1 core：`update_supported` 放开（`crates/helper-core`）

- `manager.rs`：`update_supported: cfg!(windows)` 改为
  `cfg!(any(windows, target_os = "macos"))`。
- `Status` 字段注释同步更新（"仅 Windows"表述作废）。
- `paths.rs` 不改：`is_transient_install_path` 已存在且已有测试。

### 4.2 Rust 命令层：`update.rs`

- `ensure_update_supported`：`cfg!(windows)` 改为 `cfg!(any(windows, target_os = "macos"))`；
  错误文案不变（只剩 Linux 等开发平台会走到）。
- 新增 `ensure_install_location_ok()`：仅 macOS 生效——
  `std::env::current_exe()` 传给 `helper_core::paths::is_transient_install_path`，
  命中临时路径时返回 `ErrorPayload { code: "update", message: <引导文案> }`。
  引导文案与前端 `INSTALL_LOCATION_MESSAGE` 同义（"请先把 CodexHelper 拖到「应用程序」文件夹，再从那里打开后再更新"），作为 const 写在 `update.rs`。
- `install_update` 在 `ensure_update_supported()` 之后调用 `ensure_install_location_ok()`。
  `check_update` **不调用**（检测无副作用，M3）。
- `app_version` 不变。
- 单元测试：`ensure_update_supported` 断言改为 `cfg!(any(windows, target_os = "macos"))`；
  新增 `is_transient_install_path(current_exe 语义)` 相关用例（构造 `/Volumes/...`、
  `/AppTranslocation/...` 路径断言拦截文案；普通路径断言放行——沿用 paths.rs 已有测试风格）。

### 4.3 前端：升级区与安装位置联动

- `types.ts`：`Status` 无结构变化（`updateSupported` / `installLocationOk` 已存在）。
- `format.ts`：`showUpdatePanel` 不变（macOS 下 `updateSupported` 变 true 后自动显示）。新增纯函数
  `updatePanelViewState(state)`：输入 `Pick<UpdatePanelState, "available"|"installing"|"installLocationOk">`，
  输出 `{ installDisabled: boolean; installHint: string }`（安装位置异常且已有可用更新时
  `installDisabled: true`，hint 为 `发现新版本 v${version}，请先把 CodexHelper 拖到「应用程序」文件夹后再更新`）。
  `updatepanel.ts` 的 `update()` 仅应用该视图状态——前端测试基建是 `node --test` 纯函数
  （无 DOM shim），行为断言落在 `format.test.ts`。
- `updatepanel.ts`：`UpdatePanelState` 新增 `installLocationOk: boolean`；
  `available` 存在且 `installLocationOk === false` 时：
  - "立即更新"按钮 `disabled`；
  - hint 显示：`发现新版本 v${version}，请先把 CodexHelper 拖到「应用程序」文件夹后再更新`。
- `main.ts`：`render` 时把 `status.installLocationOk` 传入 `updatePanel.update`。
- `dev-mock.ts`：macos 场景 `updateSupported: true`；`transient` 场景叠加
  `installLocationOk: false` 继续成立；mock 的 `checkUpdate` / `installUpdate`
  拒绝条件从 `!status.updateSupported` 改为 `status.platform === "other"`（新增最小
  `other` 场景：`platform: "other"`、`updateSupported: false`，覆盖 `UPDATE_UNSUPPORTED`
  错误路径）。
- `format.test.ts`：新增 `updatePanelViewState` 纯函数测试（安装位置异常时禁用与 hint
  文案；位置正常时可用；无可用更新时不受安装位置影响）。

### 4.4 构建配置与脚本

- `tauri.macos.conf.json`：`createUpdaterArtifacts: false` → `true`。
- `build-macos.sh`：
  - 若 `TAURI_SIGNING_PRIVATE_KEY` 为空：打印提示（"本地构建：未提供签名私钥，跳过升级产物"），
    追加 `--config '{"bundle":{"createUpdaterArtifacts":false}}'`（CLI 内联 JSON 与文件
    config 叠加合并，数组整体替换、标量覆盖）；
  - 若提供私钥：正常构建，结束后自检 `bundle/macos/*.app.tar.gz` 与 `.sig` 存在、
    sig 非空，并把 `updater_artifact=<路径>` 追加进 `GITHUB_OUTPUT`（与 dmg 同法）。
  - dmg 自检逻辑保持。
- `publish-update.sh` 改造（保持无参可用，兼容旧行为）：
  - 扫描 `BUNDLE_DIR`（默认不变）：
    - `*-setup.exe` + 同名 `.sig` → 平台键 `windows-x86_64`；
    - `*.app.tar.gz` + 同名 `.sig` → 平台键 `darwin-aarch64` 与 `darwin-x86_64`
      （同一 URL、同一 signature）；
  - 至少识别出一个平台才生成 `latest.json`；两类产物并存时合并进同一 manifest；
  - 上传循环改为"逐产物 + 合并后的 latest.json"；`latest.json` 始终覆盖上传；
  - 保持"未配凭据只生成不上传"。
- `release.yml`：
  - `macos-release` job 的构建步骤注入 `TAURI_SIGNING_PRIVATE_KEY`
    （`env: TAURI_SIGNING_PRIVATE_KEY: ${{ secrets.TAURI_SIGNING_PRIVATE_KEY }}`）；
  - 上传步骤 files 增加 `target/universal-apple-darwin/release/bundle/macos/*.app.tar.gz`
    与 `*.app.tar.gz.sig`（通配两者，与 windows 侧上传 sig 的写法一致）；
  - Release 说明的 macOS 安装段补一句：`已安装旧版的内网用户也可在应用内「检查更新」直接升级`。

### 4.5 发布流程（运维视角，写进本文档供发布者参考）

1. 打 tag → `release.yml` 产出 Windows 安装器 + macOS dmg + 两平台升级产物与 sig，
   全部上传 GitHub Release。
2. 内网机器：从 Release 下载 `*-setup.exe`(+sig) 与 `*.app.tar.gz`(+sig) 放入同一目录。
3. 执行 `BUNDLE_DIR=<目录> bash scripts/publish-update.sh`，生成合并 `latest.json`
   并（配了凭据时）上传 Nexus cypub。
4. 用户端：Windows / macOS 应用内"检查更新"→ 确认 → 安装 → 自动重启。

### 4.6 安全模型（继承 §15.6，macOS 增量）

- Ed25519 + `requireSignedVersion: true` 对 darwin 键同样生效（manifest 级保护）。
- 明文 HTTP 下载 + 签名保证完整性：与 Windows 同一权衡，不新增面。
- ad-hoc codesign 不参与更新链完整性；升级包完整性仅由 Ed25519 签名保证（与 Windows
  NSIS 包同一模型——安装器本体也无 Authenticode）。
- 残余风险：明文 HTTP 中间人仍可隐藏新版本 / 篡改 notes / 回放旧合法包（Windows 侧
  已有 `requireSignedVersion` 缓解降级），macOS 不扩大该面。
- `install_update` 的 expected_version 竞态校验（确认 A 不装 B）对两平台一致，无需改动。

## 5. 兼容性

- 旧版 macOS 客户端（v0.2.0/v0.3.0-rc.1）不检查更新（`update_supported=false`），
  需手动下载 dmg 一次完成"接种"，此后走应用内升级。
- `latest.json` 增加 darwin 键对旧 Windows 客户端透明（多余键被忽略）。
- 本地 `npm run dev` / `cargo test` 在无私钥环境不受影响（M5 降级路径）。
- `check-windows.sh` 门禁（macOS 交叉编译 windows-msvc）不受影响：改动不触碰
  updater 依赖特性。

## 6. 验收

- macOS `cargo test --workspace`：update.rs 新旧用例全绿。
- 前端 `node --test`：updatepanel / format / dev-mock 相关用例全绿。
- 本地无私钥 `bash scripts/build-macos.sh`：产出 dmg、无升级产物、提示出现。
- 本地有私钥构建：产出 dmg + `.app.tar.gz` + `.sig`，自检通过。
- `publish-update.sh` 在只含 Windows 产物的目录：生成单键 `latest.json`（回归不变）；
  混合目录：三键合并；空目录：报错退出。
- 手工冒烟（有真机时）：从 `/Applications` 运行 → 检查更新 → 确认 → 安装 → 自动重启
  且版本号更新；从 dmg 直接运行 → 检查更新可用 → "立即更新"禁用且 hint 引导。

## 7. 交付物清单

| 文件 | 动作 |
| --- | --- |
| `crates/helper-core/src/manager.rs` | 改 `update_supported` |
| `apps/desktop/src-tauri/src/update.rs` | 放开平台 + 安装位置拦截 + 测试 |
| `apps/desktop/src-tauri/tauri.macos.conf.json` | `createUpdaterArtifacts: true` |
| `apps/desktop/src/updatepanel.ts` | `installLocationOk` 联动 |
| `apps/desktop/src/main.ts` | 传参 |
| `apps/desktop/src/dev-mock.ts` | macos 场景放开 |
| `apps/desktop/src/format.test.ts`（或新增 updatepanel.test.ts） | 行为测试 |
| `scripts/build-macos.sh` | 私钥降级 + 产物自检 |
| `scripts/publish-update.sh` | 多平台扫描合并 |
| `.github/workflows/release.yml` | macos job 私钥 + 上传升级产物 |
| 本文档 | 新增 |
