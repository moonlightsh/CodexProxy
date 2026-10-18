# macOS 内部升级渠道 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把 Windows 已有的内部升级渠道（检测 / 下载 / Ed25519 校验 / 安装 / 自动重启）扩展到 macOS，含发布链产物与 latest.json 多平台键。

**Architecture:** 三层改动——core 状态字段放开平台；Rust 命令层加安装位置拦截；构建/发布链产出 `.app.tar.gz` 升级产物并合并进 latest.json。前端复用现有升级区组件，仅联动安装位置状态。

**Tech Stack:** Rust (tauri 2.11 / tauri-plugin-updater 2.12)、TypeScript（node --test 纯函数测试）、Bash、GitHub Actions。

**Spec:** `docs/superpowers/specs/2026-10-18-macos-update-design.md`（本计划的依据，实现者须同时阅读）

## Global Constraints

- 所有界面文案、代码注释、提交信息用中文；提交信息尾部带 `Co-Authored-By: Pi`；分支名以 `pi/` 开头（本计划在 `pi/macos-update` 分支上执行，第一条命令创建）。
- 不 push、不建 MR，除非用户明确要求。
- 平台判定统一用 `cfg!(any(windows, target_os = "macos"))`（core 与 update.rs 两处同义）。
- darwin 平台键用裸键：`darwin-aarch64`、`darwin-x86_64`（不带 `-app` 后缀）。
- 前端测试只能跑纯函数：`npm --prefix apps/desktop test`（node --test --experimental-strip-types src/*.test.ts），无 DOM shim。
- Rust 门禁：`cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace` 必须全绿。
- capabilities 不授 `updater:default`（维持现状，不改动 capabilities）。
- 签名私钥绝不入仓库；本地无 `TAURI_SIGNING_PRIVATE_KEY` 时构建必须走降级路径成功。

## Review Focus

1. **临时安装位置只拦安装、不拦检测**：dmg 里运行时"检查更新"仍可用——`install_update` 拦截而 `check_update` 不拦截（Task 3 测试固定）。
2. **Windows 回归**：`update_supported` 放开后 Windows 行为逐字节不变；`publish-update.sh` 在只含 Windows 产物的目录生成与旧脚本完全一致的单键 latest.json（Task 6 回归测试固定）。
3. **无可用更新时安装位置不影响 UI**：`available === null` 时 hint 为空、无禁用逻辑误伤"检查更新"按钮（Task 4 纯函数测试固定）。
4. **无签名私钥的本地构建不失败**：`build-macos.sh` 降级路径打印提示并产出 dmg（Task 7 shell 语法自检 + 人工核验）。
5. **latest.json 合并幂等**：混合目录重复执行脚本得到相同 manifest；空目录报错退出而非生成空 platforms（Task 6 测试固定）。

---

### Task 1: core 放开 update_supported

**Files:**
- Modify: `crates/helper-core/src/manager.rs:1307`
- Modify: `crates/helper-core/src/types.rs:96-100`

**Interfaces:**
- Produces: `Status.update_supported` 在 macOS 上为 `true`（下游 update.rs 与前端依赖此语义）。

- [x] **Step 1: 写失败测试**

`crates/helper-core/src/types.rs` 现有 serde 测试只断言字段存在。在 `manager.rs` 的测试模块（或相邻合适位置，跟随现有测试组织方式）加断言 macOS 语义的测试不可行——`cfg!` 是编译期，单平台上无法同时测两值。改为：找到现有断言 `update_supported` 语义的测试并更新期望；若无此测试，则在 types.rs 测试中仅保留 serde 断言不变。核心验证落在 Step 3 的编译期事实上：`grep -n "update_supported" crates/helper-core/src/manager.rs` 确认只有一处赋值。

- [x] **Step 2: 跑测试确认现状**

Run: `cargo test -p helper-core`
Expected: PASS（现状基线，无失败）

- [x] **Step 3: 修改赋值与注释**

`manager.rs:1307`：`update_supported: cfg!(windows),` → `update_supported: cfg!(any(windows, target_os = "macos")),`
`types.rs:98` 注释：`/// 是否支持内部升级（仅 Windows 为 true；为 false 时界面隐藏升级区域）` → `/// 是否支持内部升级（Windows 与 macOS 为 true；为 false 时界面隐藏升级区域）`

- [x] **Step 4: 跑测试与门禁**

Run: `cargo test -p helper-core && cargo fmt --all --check && cargo clippy -p helper-core --all-targets -- -D warnings`
Expected: 全部 PASS

- [x] **Step 5: Commit**

```bash
git add crates/helper-core/src/manager.rs crates/helper-core/src/types.rs
git commit -m "feat(core): macOS 纳入内部升级支持

Co-Authored-By: Pi"
```

### Task 2: update.rs 安装位置拦截（Rust）

**Files:**
- Modify: `apps/desktop/src-tauri/src/update.rs`

**Interfaces:**
- Consumes: `helper_core::paths::is_transient_install_path(&Path) -> bool`（已存在，勿改）
- Produces: `pub const TRANSIENT_INSTALL_UPDATE_MESSAGE: &str`（文案：`请先把 CodexHelper 拖到「应用程序」文件夹，再从那里打开后再更新`）；内部函数 `fn ensure_install_location_ok() -> Result<(), ErrorPayload>`，仅 macOS 生效。

- [x] **Step 1: 写失败测试**

在 update.rs 测试模块新增（沿用现有 `#[test]` 风格，路径判定直接测 `is_transient_install_path` 组合出的分支逻辑；`ensure_install_location_ok` 依赖 `current_exe`，用与 manager 相同的单测策略：对函数内的判定核心抽 `fn transient_run_location() -> bool`（`std::env::current_exe().map(|p| is_transient_install_path(&p)).unwrap_or(false)`），测试覆盖 `transient_run_location` 与文案常量）：

```rust
#[test]
fn install_location_message_matches_design() {
    assert_eq!(
        TRANSIENT_INSTALL_UPDATE_MESSAGE,
        "请先把 CodexHelper 拖到「应用程序」文件夹，再从那里打开后再更新"
    );
}

#[test]
#[cfg(target_os = "macos")]
fn transient_run_location_false_for_normal_path() {
    // 正常 /Applications 安装下 current_exe 不含 /Volumes/ 或 /AppTranslocation/
    assert!(!transient_run_location());
}
```

另更新现有 `update_commands_only_supported_on_windows` 测试名与断言：`assert_eq!(result.is_ok(), cfg!(any(windows, target_os = "macos")));`，测试改名 `update_commands_supported_on_windows_and_macos`。

- [x] **Step 2: 跑测试确认失败**

Run: `cargo test -p codex-helper-desktop --lib`（若 crate 名不同，以 `cargo test --workspace update` 为准）
Expected: FAIL，`TRANSIENT_INSTALL_UPDATE_MESSAGE` 未定义

- [x] **Step 3: 实现**

- `ensure_update_supported`：`cfg!(windows)` → `cfg!(any(windows, target_os = "macos"))`。
- 新增常量与两个函数（`transient_run_location`、`ensure_install_location_ok`，后者命中临时路径时返回 `ErrorPayload { code: "update", message: TRANSIENT_INSTALL_UPDATE_MESSAGE.to_string() }`，仅 `#[cfg(target_os = "macos")]` 判定生效、其他平台直接 `Ok(())`——用 `cfg!` 而非 `#[cfg]` 保持两平台都能编译测试）。
- `install_update`：在 `ensure_update_supported()?;` 之后插入 `ensure_install_location_ok()?;`。`check_update` 不加。

- [x] **Step 4: 跑测试与门禁**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`
Expected: 全 PASS

- [x] **Step 5: Commit**

```bash
git add apps/desktop/src-tauri/src/update.rs
git commit -m "feat(desktop): macOS 升级安装拦截临时安装位置

Co-Authored-By: Pi"
```

### Task 3: 前端纯函数 updatePanelViewState

**Files:**
- Modify: `apps/desktop/src/format.ts`
- Test: `apps/desktop/src/format.test.ts`

**Interfaces:**
- Produces: `export function updatePanelViewState(state: Pick<UpdatePanelState, "available" | "installing" | "installLocationOk">): { installDisabled: boolean; hint: string }`；`UpdatePanelState` 类型从 `./updatepanel` 导入（若无导出需在 updatepanel.ts 补 `export`，类型本身已导出）。

- [x] **Step 1: 写失败测试**

在 `format.test.ts` 追加（沿用 `node:test` + `node:assert/strict` 现有风格）：

```ts
test("updatePanelViewState：安装位置异常且有可用更新时禁用安装并给出引导", () => {
  const view = updatePanelViewState({ available: fakeUpdate("0.4.0"), installing: false, installLocationOk: false });
  assert.equal(view.installDisabled, true);
  assert.match(view.hint, /请先把 CodexHelper 拖到「应用程序」文件夹/);
});

test("updatePanelViewState：安装位置正常时安装可用", () => {
  const view = updatePanelViewState({ available: fakeUpdate("0.4.0"), installing: false, installLocationOk: true });
  assert.equal(view.installDisabled, false);
});

test("updatePanelViewState：无可用更新时 hint 为空且不误伤", () => {
  const view = updatePanelViewState({ available: null, installing: false, installLocationOk: false });
  assert.equal(view.hint, "");
});
```

（`fakeUpdate` 为测试内构造 `UpdateCheck` 的 helper，字段按 `types.ts` 的 `UpdateCheck`：`available: true, currentVersion: "0.3.0", version, notes: null, date: null`。）

- [x] **Step 2: 跑测试确认失败**

Run: `npm --prefix apps/desktop test`
Expected: FAIL，`updatePanelViewState` 未导出

- [x] **Step 3: 实现**

在 `format.ts` 新增函数：`installing` → `{ installDisabled: true, hint: "正在下载并安装更新，完成后会自动重启应用…" }`；否则 `available && !installLocationOk` → 禁用 + 引导文案 `发现新版本 v${available.version}，请先把 CodexHelper 拖到「应用程序」文件夹后再更新`；其余 `{ installDisabled: false, hint: "" }`。hint 文案与 updatepanel.ts 现有正常路径不冲突（本函数只负责新增分支）。

- [x] **Step 4: 跑测试**

Run: `npm --prefix apps/desktop test`
Expected: 全 PASS

- [x] **Step 5: Commit**

```bash
git add apps/desktop/src/format.ts apps/desktop/src/format.test.ts
git commit -m "feat(desktop): 升级区视图状态纯函数（安装位置联动）

Co-Authored-By: Pi"
```

### Task 4: updatepanel 应用视图状态 + main.ts 传参

**Files:**
- Modify: `apps/desktop/src/updatepanel.ts`
- Modify: `apps/desktop/src/main.ts`
- Test: `apps/desktop/src/format.test.ts`（Task 3 已建，此处补跨模块一致性断言）

**Interfaces:**
- Consumes: `updatePanelViewState`（Task 3）
- Produces: `UpdatePanelState` 增加 `installLocationOk: boolean`（Task 3 已引用该字段名，本任务补字段定义）。

- [x] **Step 1: 写失败测试**

format.test.ts 追加一致性断言（无需 DOM）：

```ts
test("updatePanelViewState 与安装位置横幅文案来源一致", () => {
  const view = updatePanelViewState({ available: fakeUpdate("0.4.0"), installing: false, installLocationOk: false });
  assert.ok(view.hint.includes("「应用程序」"));
});
```

（轻量守卫：文案与 `INSTALL_LOCATION_MESSAGE` 同义，避免两处漂移。）

- [x] **Step 2: 改 updatepanel.ts**

- `UpdatePanelState` 增加 `installLocationOk: boolean`（doc 注释：`/** 安装位置是否正常；异常且有可用更新时禁用安装并提示 */`）。
- `update()` 内调用 `updatePanelViewState`，用其 `installDisabled` 合并现有 `state.installing` 禁用逻辑（`installButton.disabled = view.installDisabled || state.installing`，installing 时本就 true），hint 分支在"安装中"与"发现新版本"之间插入"安装位置异常"分支（优先级：安装中 > 位置异常 > 正常新版本 > 无）。

- [x] **Step 3: 改 main.ts**

render 中 `updatePanel.update({ ... })` 增加 `installLocationOk: status?.installLocationOk ?? true`。

- [x] **Step 4: 跑测试与类型检查**

Run: `npm --prefix apps/desktop test && npx --prefix apps/desktop tsc --noEmit -p apps/desktop`
Expected: 全 PASS（若仓库无 tsc 脚本则 `cd apps/desktop && npx tsc --noEmit`）

- [x] **Step 5: Commit**

```bash
git add apps/desktop/src/updatepanel.ts apps/desktop/src/main.ts apps/desktop/src/format.test.ts
git commit -m "feat(desktop): 升级区禁用态联动安装位置

Co-Authored-By: Pi"
```

### Task 5: dev-mock 场景更新

**Files:**
- Modify: `apps/desktop/src/dev-mock.ts`

**Interfaces:**
- Consumes: `Status.updateSupported` / `platform` 语义
- Produces: macos 场景 `updateSupported: true`；新增 `other` 场景；mock 命令拒绝条件 `status.platform === "other"`。

- [x] **Step 1: 定位现有场景分支与 mock 拒绝点**

`grep -n "scenario\|case \"macos\"\|updateSupported\|UPDATE_UNSUPPORTED" apps/desktop/src/dev-mock.ts`，理解 `applyScenario` 结构后再改。

- [x] **Step 2: 修改**

- `case "macos"`：删去 `updateSupported: false` 覆盖（继承 base 的 `true`）。
- 新增 `case "other"`：`{ ...status, platform: "other", platformSupported: false, updateSupported: false }`（置于 macos 分支后）。
- mock 的 `checkUpdate` / `installUpdate`（约 L317/L330）：`if (!status.updateSupported) return reject(UPDATE_UNSUPPORTED);` → `if (status.platform === "other") return reject(UPDATE_UNSUPPORTED);`

- [x] **Step 3: 跑测试**

Run: `npm --prefix apps/desktop test`
Expected: 全 PASS（dev-mock 无独立测试文件；保证不破坏现有导入）

- [x] **Step 4: Commit**

```bash
git add apps/desktop/src/dev-mock.ts
git commit -m "feat(desktop): dev-mock 放开 macos 升级场景并新增 other 场景

Co-Authored-By: Pi"
```

### Task 6: publish-update.sh 多平台扫描合并

**Files:**
- Modify: `scripts/publish-update.sh`

**Interfaces:**
- Produces: `latest.json` 支持 `windows-x86_64` + `darwin-aarch64` + `darwin-x86_64`（darwin 两键同 URL 同 signature）；调用方式不变（`BUNDLE_DIR` 可覆盖）。

- [x] **Step 1: 写失败测试（shell 自测模式）**

在脚本内加 `--self-test` 分支（不触碰网络、不写文件，仅当参数为 `--self-test` 时执行）：构造临时目录放假产物（`CodexHelper_0.4.0_x64-setup.exe` + `.sig`、`CodexHelper_0.4.0_universal.app.tar.gz` + `.sig`），调用内部生成函数，断言：三平台键存在、darwin 两键 URL 相同、只含 Windows 时单键、空目录报错退出码非 0。生成逻辑抽成 `generate_manifest <bundle_dir> <out_json>` 函数供 self-test 与主流程共用。

- [x] **Step 2: 跑 self-test 确认失败**

Run: `bash scripts/publish-update.sh --self-test`
Expected: FAIL（`--self-test` 不存在，脚本按无产物目录报错退出）

- [x] **Step 3: 实现**

- 开头参数分支：`if [[ "${1:-}" == "--self-test" ]]; then self_test; exit $?; fi`。
- 主流程：glob 收集 `*-setup.exe.sig` 与 `*.app.tar.gz.sig`，逐个配对产物与签名，node 生成 manifest 时 platforms 注入三键（darwin 键复用同一 url/signature）。
- 上传循环：逐产物 PUT + 最后 `latest.json` PUT（覆盖）。保持"无凭据仅生成"。
- 版本号校验：若同时存在 Windows 与 darwin 产物且 `tauri.conf.json` 版本一致才合并（从产物文件名解析 `<版本>_`，不一致时报错——防发布目录混入异版本产物）。

- [x] **Step 4: 跑 self-test 与门禁**

Run: `bash scripts/publish-update.sh --self-test && bash -n scripts/publish-update.sh`
Expected: self-test PASS、语法检查通过

- [x] **Step 5: Commit**

```bash
git add scripts/publish-update.sh
git commit -m "feat(release): publish-update.sh 支持多平台产物合并 manifest

Co-Authored-By: Pi"
```

### Task 7: build-macos.sh 升级产物与降级路径

**Files:**
- Modify: `scripts/build-macos.sh`
- Modify: `apps/desktop/src-tauri/tauri.macos.conf.json`

**Interfaces:**
- Consumes: `TAURI_SIGNING_PRIVATE_KEY` 环境变量（有则产升级产物）
- Produces: 有私钥时 `target/universal-apple-darwin/release/bundle/macos/*.app.tar.gz` 与 `.sig`；`GITHUB_OUTPUT` 增加 `updater_artifact=<tar.gz 绝对路径>`。

- [x] **Step 1: 改配置**

`tauri.macos.conf.json`：`"createUpdaterArtifacts": false` → `"createUpdaterArtifacts": true`。

- [x] **Step 2: 改 build-macos.sh**

- 构建命令前判断：`if [[ -z "${TAURI_SIGNING_PRIVATE_KEY:-}" ]]` → 追加第三个 `--config '{"bundle":{"createUpdaterArtifacts":false}}'` 并打印 `本地构建：未提供签名私钥，跳过升级产物`；否则正常构建。
- 构建后自检：有私钥时 `ls "$BUNDLE"/macos/*.app.tar.gz` 恰一个、对应 `.sig` 非空，`updater_artifact` 写入 `GITHUB_OUTPUT`；无私钥时断言 tar.gz 不存在（防旧产物残留误传）。dmg 自检保持。

- [x] **Step 3: 验证**

Run: `bash -n scripts/build-macos.sh && node -e "JSON.parse(require('fs').readFileSync('apps/desktop/src-tauri/tauri.macos.conf.json'))"` 
Expected: 语法通过
（无私钥全量构建冒烟由用户在有 Xcode 工具链时执行：`bash scripts/build-macos.sh` 应产出 dmg 且提示跳过。）

- [x] **Step 4: Commit**

```bash
git add scripts/build-macos.sh apps/desktop/src-tauri/tauri.macos.conf.json
git commit -m "build(macos): 升级产物生成与无签名私钥降级路径

Co-Authored-By: Pi"
```

### Task 8: release.yml macOS job 接入

**Files:**
- Modify: `.github/workflows/release.yml`（macos-release job）

**Interfaces:**
- Consumes: secret `TAURI_SIGNING_PRIVATE_KEY`；Task 7 的 `updater_artifact` 输出。

- [x] **Step 1: 修改**

- 「构建 universal dmg」步骤加 `env: TAURI_SIGNING_PRIVATE_KEY: ${{ secrets.TAURI_SIGNING_PRIVATE_KEY }}`（步骤名可改为「构建 universal dmg 与升级产物」）。
- 「上传 dmg 到 GitHub Release」步骤 files 增加：
  - `target/universal-apple-darwin/release/bundle/macos/*.app.tar.gz`
  - `target/universal-apple-darwin/release/bundle/macos/*.app.tar.gz.sig`
  - `fail_on_unmatched_files: true` 保持（无私钥时 glob 不匹配会失败——若 CI 未配 secret，构建步骤会先失败并给出清晰报错，可接受）。
- Release 正文 macOS 安装段补一句：`已安装旧版的内网用户也可在应用内「检查更新」直接升级。`
- 头部注释更新：macos job 也调用签名（不再是"不生成升级产物"）。

- [x] **Step 2: 验证**

Run: `node -e "const y=require('js-yaml'); y.load(require('fs').readFileSync('.github/workflows/release.yml','utf8')); console.log('yaml ok')"`（若 js-yaml 不可用：`python3 -c "import yaml,sys; yaml.safe_load(open('.github/workflows/release.yml')); print('yaml ok')"`，两者都缺则 `grep -n` 人工核对缩进）
Expected: yaml ok

- [x] **Step 3: Commit**

```bash
git add .github/workflows/release.yml
git commit -m "ci(release): macOS 产物接入升级渠道

Co-Authored-By: Pi"
```

### Task 9: 设计文档状态更新与全量门禁

**Files:**
- Modify: `docs/superpowers/specs/2026-10-18-macos-update-design.md`（状态行）

**Interfaces:** 无。

- [x] **Step 1: 全量门禁**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace && npm --prefix apps/desktop test && bash scripts/publish-update.sh --self-test`
Expected: 全部 PASS

- [x] **Step 2: 更新文档状态**

设计文档头部状态改为：`- 状态：已实现（见 docs/superpowers/plans/2026-10-18-macos-update.md）`。

- [x] **Step 3: Commit**

```bash
git add docs/superpowers/specs/2026-10-18-macos-update-design.md
git commit -m "docs: macOS 升级设计标记为已实现

Co-Authored-By: Pi"
```
