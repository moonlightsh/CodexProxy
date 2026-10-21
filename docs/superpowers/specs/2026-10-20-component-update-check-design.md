# CodexHelper 组件更新检查设计（ChatGPT Desktop / Codex CLI）

- 日期：2026-10-20（2026-10-21 两轮评审修订，见 §10）
- 状态：已实现（2026-10-21，分支 `pi/component-update-check`；真机验收见 §6，待执行）
- 上游设计：`docs/superpowers/specs/2026-09-23-codex-helper-design.md`（§9 界面层流量、§15 升级渠道）、
  `docs/superpowers/specs/2026-10-16-proxy-mtls-process-binding-design.md`（Secure 进程门禁与 openai-restrict）
- 数据源依据：对 `ChatGPT-x64.msix`（SHA-256 `e2d892e6…afbe7f`）的静态逆向，
  以及 2026-10-21 对线上两个清单、MSIX AppxManifest、固定地址 HEAD 响应、
  `$CODEX_HOME/version.json`、aitools-download `prepare.py` 的实测核对（见 §2、§10）

## 1. 背景与目标

CodexHelper 管理的两个核心组件都有独立于 CodexHelper 的发布节奏：

- **ChatGPT Desktop**（MSIX，`OpenAI.Codex`）：应用内 Help→检查更新走三层——
  Microsoft Store 服务（WinRT，系统进程发起）、`persistent.oaistatic.com` 清单、MSIX 直下。
  内网机器上 Store 链路基本不可用（应用自身的更新器在 Store 不可用时就改读
  `windows-release.json` 清单，见 §2.1），且 `.env` 代理对它完全不生效（Chromium 网络栈
  不读环境变量，Store 服务流量不经过用户态进程）。
- **Codex CLI**（`codex.exe`）：GitHub Releases 发布，内网机器需经离线包
  （aitools-download 管线）更新，版本容易滞后。

用户在 CodexHelper 里无法知道"这两个组件现在最新是什么版本、我落后多少"。
本设计新增**组件更新检查**：手动触发一次检测，展示本地与远端最新版本，
发现新版本时给出官方下载地址与离线包更新指引，更新动作本身仍走既有离线包管线。

### 目标

- 一键检查 ChatGPT Desktop（Windows）与 Codex CLI（Windows/macOS）的组件更新。
- 本地版本、远端最新版本、落后情况一目了然；有更新时提供官方下载 URL 或 release 页面。
- 更新检测请求经本地分流代理出网（代理运行时），内网机器可用；Secure 模式下同样
  经代理（门禁对本进程放行，见 §7），绝不回退直连——代理未就绪时**零出站**，
  如实展示"Secure 代理未就绪"。

### 非目标

- 不下载、不安装组件更新（MSIX 安装归 Store/PackageManager，CLI 安装归离线包管线；
  离线安装还需 `ChatGPT-License.xml`）。
- 不做组件更新的定时自动检查（避免内网环境反复超时；仅手动触发）。
- 不接管 ChatGPT Desktop 的流量入口（§9 的 PAC 方案仍是独立议题）。

### 反目标

- 检测请求把网关 Key、用户凭据带出去（检测全程无凭据）。
- 采信下载地址不在官方白名单内的清单（伪清单指向第三方 MSIX）。
- 拖慢应用启动或现有更新区（自身升级）行为。

## 2. 数据源与机制（逆向 + 实测结论）

对 MSIX 内 `app.asar` 的静态分析，加上 2026-10-21 的线上实测：

### 2.1 ChatGPT Desktop（Windows，prod 渠道）

- MSIX 内 AppxManifest 实测：`Identity Name="OpenAI.Codex" Version="26.1013.4894.0"
  Publisher="CN=50BDFD77-8903-4850-9FFE-6E8522F64D5B"`；
  无 `codex.exe` 执行别名（只有 codex-chrome-native-host.exe、
  codex-core-command-runner.exe），应用自带 `app/resources/codex.exe`（不在 PATH）。
- 两个公开清单（无鉴权，2026-10-21 实测）：
  - 主清单 `…/codex-app-prod/windows-store-update.json`：
    `{ schemaVersion:1, buildVersion:"26.1013.4894.0", storeProductId:"9PLM9XGG6VKS",
    packageIdentity:"OpenAI.Codex", releaseVersion:"26.1013.40934" }`
    ——实际**没有** `packages`、`packagePublisher` 字段。
  - 发布源 `…/codex-app-prod/windows-release.json`：
    `{ schemaVersion:1, releaseDate:"2026-10-17T05:55:44Z",
    packageVersion:"26.1013.4894.0", packages:[
    { architecture:"x64", downloadUrl:"…/releases/26.1013.4894.0/ChatGPT-x64.msix" },
    { architecture:"arm64", downloadUrl:"…/releases/26.1013.4894.0/ChatGPT-arm64.msix" } ] }`。
- **应用自身更新器 `direct()` 的行为**：Store 不可用（`store-unavailable`，内网即此情形）
  时改读 `windows-release.json`，按 `packages[].architecture === process.arch` 取
  `downloadUrl`。本设计的 ChatGPT 主源与之同构（见 C2）。
- **版本字段语义（纠错，关键）**：比较一律用四段的 `buildVersion` / `packageVersion`
  （如 `26.1013.4894.0`）与 `Get-AppxPackage` 的四段 Version（同为四段）。
  `releaseVersion`（如 `26.1013.40934`）仅展示用、**绝不参与比较**——误用后果：
  装机版本第三段是 `4894`，而 `releaseVersion` 第三段是五位数 `40934`，逐段数值比较
  恒小于，会导致**永远提示有更新**。
- 版本比较语义（应用内 `un()`）：点分数字逐段数值比较、**缺段按 0**；
  不需要"去尾部零段对齐"的处理。
- 解析要求：serde 结构体只声明所需字段（`schemaVersion`/`packageVersion`/
  `packages[].architecture`/`packages[].downloadUrl`），**容忍未知字段**（上游加字段不致解析失败）。
- 固定下载地址（aitools-download `prepare.py` 在用）：
  `…/codex-app-prod/ChatGPT-x64.msix`；其 HEAD 响应头含
  `x-ms-meta-package_version: 26.1013.4894.0`、`x-ms-meta-package_identity: OpenAI.Codex`、
  `x-ms-meta-architecture: x64`——可作为验收对照手段（见 §6），不进运行时代码。
- macOS 参照（二期，本设计仅记录）：Sparkle appcast。实际 URL 由应用内 `sn()` 构造：
  基址 `https://updates.oaistatic.com/codex/app/appcast`，并附 arch、app version、
  os-version、plan_type 等查询参数——二期解析时需按同构方式带参请求；
  本地版本读 `/Applications/ChatGPT.app/Contents/Info.plist`（`plutil` 解析列二期）。

### 2.2 Codex CLI

- 远端主源：GitHub API `https://api.github.com/repos/openai/codex/releases/latest`，
  `tag_name` 形如 `rust-v0.162.1`，去掉 `rust-v` 前缀得到版本号。
  （aitools-download `prepare.py` 同源，保证两边"最新版"判断一致。）
- **限流与回退（与 prepare.py 同构）**：Legacy 模式下全内网共用一个 SOCKS5 出口 IP，
  匿名限额 60 次/时为全员共享——且 Codex CLI 自身的更新检查也计入。
  API 返回 403/429 时，回退请求 `https://github.com/openai/codex/releases/latest`，
  在 `redirect(Policy::none())` 下读取 302 `Location` 头的末段取 tag，再用与 prepare.py
  相同的正则 `^rust-v\d+\.\d+\.\d+$` 校验（prepare.py 为 `re.fullmatch(r"rust-v[0-9]+\.[0-9]+\.[0-9]+")`，
  判定完全一致；预发布 tag 被该正则拒绝）。回退链：**API → releases/latest 302 → 本地
  `version.json` 缓存（见 §4.3）**。除这条与离线管线同构的回退外不做多源重试。
- 本地：PATH 上第一个 `codex`(`.exe`) 执行 `--version`，输出形如 `codex-cli 0.162.1`。
- 版本比较：semver（major.minor.patch；需处理预发布后缀如 `0.164.0-alpha.2`；
  本地 `0.0.0` 为开发版、**不比较**（结果 `Unknown`，不提示更新），见 C6）。

### 2.3 分流规则佐证

`rules.rs` 的 `DOMAIN_SUFFIXES` 已含 `oaistatic.com` 与 `github.com`，
`api.github.com` 命中后缀规则 → 本地代理运行时，更新检测请求经 17891 走上游 SOCKS5；
本地代理支持 `CONNECT` 隧道（HTTPS 不解密），reqwest 以 HTTP 代理身份接入即可。
经代理的检测请求会出现在"最近连接"列表（`ConnectionRecord` 记录主机与端口），属预期行为。

## 3. 已拍板的决策

| 编号 | 决策 |
| --- | --- |
| C1 | 只检测不安装：展示新版本号与官方下载 URL / release 页面，安装走离线包管线 / 应用自身更新器 |
| C2 | ChatGPT 主源 = §2.1 **`windows-release.json`**（`packageVersion` + x64 `downloadUrl`，带版本号不可变地址）；本地版本 = `Get-AppxPackage -Name OpenAI.Codex`（子进程，非 WinRT）。理由：应用自身在 Store 不可用（即内网）时就用它；Store 版与离线 MSIX 版可能分叉，用户升级路径是离线包。**不请求** `windows-store-update.json`（减少一次出站；其 `buildVersion` 与 `packageVersion` 同值，无增量信息） |
| C3 | Codex CLI 源 = GitHub releases latest API（403/429 时回退 releases/latest 302，§2.2）；本地版本 = PATH 首个 `codex --version` |
| C4 | 网络路径判定来自 `Manager` 的状态快照（`Manager::status()` 的 `Status.mode`（状态文件模式：Legacy/Secure）与 `ProxyStatus.state`，**不用 `probe_existing`**——manager.rs 注释已说明自识应答可被伪造；**判定用 `status.mode` 而非 `secure.active`**——Secure 模式下代理被阻断（SecureBlocked、端口被占）时 state 为 Stopped，按 `secure.active`/state 判定会错误直连）。决策表：`mode == Secure` 且 `state == Running` → 经代理（依赖 C11/D-Q1 定案的门禁放行）；`mode == Secure` 且其余状态 → **不发任何请求**，两组件远端状态为 `RemoteState::Blocked`（"Secure 代理未就绪"），本地探测照常；`mode == Legacy`：Running → 经代理；External → 视为模式未知，直连并注明；Stopped → 直连。**Secure 模式下绝不直连**（遵循 Secure 设计"绝不自动回退"） |
| C5 | `reqwest` workspace 依赖追加 `native-tls` feature（系统证书库，兼容企业 TLS 检查；与自身升级的 tauri-plugin-updater 选型一致；Cargo.lock 已有 native-tls/hyper-tls）。**不加 `json` feature**（用 `bytes()` + `serde_json::from_slice`） |
| C6 | 版本比较手写小函数：ChatGPT 用点分数字逐段比较、缺段按 0（四段对四段）；CLI 用 semver（major.minor.patch 数值比较，预发布后缀按"有后缀 < 无后缀"处理；本地 `0.0.0` 为开发版，**不提示更新**，比较结果定为 `Unknown`，文案"开发版，不比较"）。不新增 semver crate |
| C7 | 新 Tauri 命令 `check_component_updates` 一次返回两组件结果（避免前端两次调用与状态竞态）；命令按 `detect_dev_env` 先例放进 `commands.rs`，不新建文件 |
| C8 | 范围：Windows 两组件全量；macOS 仅 Codex CLI（ChatGPT 行由 Rust 侧返回 `unsupported_platform` 态，appcast XML 解析列二期） |
| C9 | 清单采信校验：`downloadUrl` 必须 https 且**主机+路径前缀**命中白名单 `https://persistent.oaistatic.com/codex-app-prod/`，否则判为"清单无效"（`invalid`）。不再校验 `packageIdentity`——本功能不下载不安装，该字段对防伪意义有限；防伪锚点是下载地址白名单。（HEAD 固定地址的 `x-ms-meta-package_version` 可作验收对照，不进运行时） |
| C10 | 仅手动触发，不做启动静默检测与定时器 |
| C11 | Secure（进程门禁）模式：门禁对本进程 PID 放行（D-Q1 已定案为方案 (d)，见 §7），检测请求经代理出网，绝不回退直连 |
| C12 | 本机 arch 固定按 `x64` 取清单 `packages` 条目（内网目标机为 x64；ARM64 列二期，届时清单 arm64 无条目时只显示版本不展示下载 URL） |
| C13 | `Get-AppxPackage` 子进程超时对齐 devenv 惯例 15s（`DEVENV_PROBE_TIMEOUT_SECS`），不新造 5s |

## 4. 详细设计

### 4.1 helper-core 新模块 `component_updates.rs`

数据结构（统一**内部标签**序列化：`#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]`。
理由：枚举上的 `rename_all` 只重命名变体名、不重命名 struct 变体内的字段；且默认外部
标签会让单元变体序列化成字符串、struct 变体序列化成对象，前端形状不统一。内部标签
+ `rename_all_fields`（serde ≥ 1.0.183 支持，workspace 为 1.0.229）后每个枚举值都是
含 `kind` 的对象、字段全部 camelCase；全单元变体的 `Comparison` 仍为纯字符串。
`local`/`remote` 独立建模，总体比较结果由两者派生；
`checking` 是前端状态、不进 Rust 枚举；**不再有 `update_available: Option<bool>` 冗余字段**）：

```rust
/// 单组件的本地探测结果。
#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum LocalState {
    Installed { version: String },
    /// 仅用于 CLI：PATH 无 codex 命中
    NotInstalled,
    /// ChatGPT 空输出一律判此态（无法区分"未安装"与"当前用户未注册"：不带
    /// -AllUsers 时 Get-AppxPackage 只返回当前用户已注册的包）
    NotRegisteredForUser,
    ProbeFailed { detail: String },
}

/// 远端最新版本结果。`source` 为展示用来源标记。
#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum RemoteState {
    Ok {
        version: String,
        source: RemoteSource,
        /// 远端失败的简要原因，仅 `source == CodexLocalCache`（本地缓存兜底）时有值，
        /// 避免"兜底结果落在 Ok 里、远端失败原因被吞掉"
        #[serde(skip_serializing_if = "Option::is_none")]
        fallback_from: Option<String>,
    },
    Unreachable { detail: String },
    /// 清单/响应格式无效、或下载地址未命中白名单
    Invalid { detail: String },
    /// Secure 模式下代理未就绪（state != Running）：本次未发出任何远端请求
    Blocked { detail: String },
    UnsupportedPlatform,
}

/// 远端来源标记。
#[derive(Serialize, Clone)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum RemoteSource {
    /// windows-release.json 清单
    DesktopReleaseFeed,
    /// GitHub releases latest API
    GithubApi,
    /// releases/latest 302 回退
    GithubRedirect,
    /// $CODEX_HOME/version.json 本地缓存（非公开格式，仅兜底）
    CodexLocalCache { checked_at: String },
}

/// 两组件的总体比较结果（由 local+remote 派生，前端据此渲染状态徽标）。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Comparison {
    UpdateAvailable,
    UpToDate,
    /// 本地比远端新（如离线包装了新版、清单滞后）：不提示更新
    LocalNewer,
    /// 任一侧缺版本（未安装/探测失败/远端不可达），或本地为 0.0.0 开发版不比较
    Unknown,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComponentUpdate {
    pub local: LocalState,
    pub remote: RemoteState,
    pub comparison: Comparison,
    /// 有更新时的下载入口：ChatGPT 为带版本号 downloadUrl；CLI 为 release 页面
    /// `https://github.com/openai/codex/releases/tag/<tag>`
    pub download_url: Option<String>,
    /// 本次检测是否经本地代理出网（直连为 false；Secure 未就绪无远端请求亦为 false）
    pub via_proxy: bool,
    /// 直连/未就绪原因文案；`&'static str` 仅用于序列化输出（无需 Deserialize）
    pub direct_reason: Option<&'static str>,
}

/// 两组件总返回（具名结构，替代匿名 Vec：前端与命令层按字段取用，不靠下标）。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComponentUpdates {
    pub chatgpt_desktop: ComponentUpdate,
    pub codex_cli: ComponentUpdate,
}
```

- `pub async fn check_all(ctx: &CheckContext) -> ComponentUpdates`（两组件，具名结构）。
- `CheckContext` 注入：网络路径判定结果（由调用方按 Manager 状态预先算好：
  `ViaProxy` / `Direct { reason: Option }` / `Blocked { detail }`——Secure 未就绪，
  远端探测整体跳过，本地探测照常）、超时、子进程探测函数
  （测试注入假实现，见 §4.5）、`HelperPaths`（读 `version.json` 兜底）。
- 两组件并行 `tokio::join!`；各组件失败互不影响（单组件失败不拖累另一个）。
- 纯函数（可单测）：清单 JSON 解析、GitHub release 解析、302 `Location` 末段取 tag、
  两种版本比较、`Get-AppxPackage` / `--version` / `version.json` 输出解析。
- 日志沿用 `{host, ...}` 风格，只记主机与版本，不记 URL 查询参数与响应体。

序列化形状示例（前端对照，见 §4.4 `types.ts`）：

```json
{
  "chatgptDesktop": {
    "local": { "kind": "installed", "version": "26.1013.4894.0" },
    "remote": { "kind": "ok", "version": "26.1013.4894.0",
                "source": { "kind": "desktopReleaseFeed" } },
    "comparison": "upToDate",
    "downloadUrl": null,
    "viaProxy": true,
    "directReason": null
  },
  "codexCli": {
    "local": { "kind": "installed", "version": "0.162.1" },
    "remote": { "kind": "ok", "version": "0.163.0",
                "source": { "kind": "codexLocalCache", "checkedAt": "2026-10-16T05:53:49Z" },
                "fallbackFrom": "github api 403 限流" },
    "comparison": "updateAvailable",
    "downloadUrl": "https://github.com/openai/codex/releases/tag/rust-v0.163.0",
    "viaProxy": false,
    "directReason": "代理为外部实例，模式未知，已直连"
  }
}
```

（`NotInstalled`、`UnsupportedPlatform`、`Comparison` 等纯单元变体在各自枚举内
序列化为 `{"kind":"notInstalled"}` / `{"kind":"unsupportedPlatform"}` 形状；
`Comparison` 为全单元变体、保持纯字符串 `"upToDate"`。）

### 4.2 网络层

- 两个 client 构建函数（纯逻辑拆出便于测试）：
  - `build_proxied_client(proxy)`：`reqwest::Client::builder().proxy(Proxy::all(...))`
  - `build_direct_client()`：`.no_proxy()`（与 `gateway.rs` 同惯例，防环境变量干扰）
  - 共同：`.timeout(15s)`、`.redirect(Policy::none())`（清单与 API 均无正常重定向；
    302 回退路径显式读 `Location` 头，不跟随）。
- 代理选择：**由 Tauri 命令层从 `AppState` 的 Manager 读状态快照**（`Manager::status()`
  已含 `Status.mode`（状态文件模式：Legacy/Secure）、`ProxyStatus.state`，
  均为短锁快照、无网络开销）；判定用 `status.mode` 而非 `secure.active`（原因见 C4）：
  - `mode == Secure`：
    - `state == Running` → proxied client（依赖 §7 门禁对本进程放行）；
    - 其余（Stopped、SecureBlocked 后端口被占等）→ **不发任何远端请求**，
      两组件 `remote = RemoteState::Blocked { detail: "Secure 代理未就绪（代理未运行）" }`
      （文案随具体状态，如"端口被外部进程占用"），本地探测照常执行；
  - `mode == Legacy`：
    - `state == Running` → proxied client；
    - `state == External` → direct client，结果中注明"代理为外部实例，模式未知，已直连"；
    - `state == Stopped` → direct client。
  结果写进返回值 `via_proxy` 与 `direct_reason`（前端展示"经代理/直连"及原因）；
  `Blocked` 时 `via_proxy = false` 且远端列展示"Secure 代理未就绪"。
- ChatGPT 清单请求：GET `windows-release.json` → 取 `packageVersion` 与
  `architecture == "x64"` 的 `downloadUrl` → C9 白名单校验（不过 → `invalid`）。
- CLI 请求：GET releases latest → 解析 `tag_name` → 去前缀。403/429 时走 §2.2 的
  releases/latest 302 回退；仍失败 → 读本地 `version.json`（§4.3）。
  固定 `User-Agent: codex-helper/<version>`（GitHub API 必需；无凭据，匿名限流见 §2.2）。

### 4.3 本地版本探测与缓存兜底

- **ChatGPT（Windows）**：spawn
  `powershell.exe -NoProfile -NonInteractive -Command "Get-AppxPackage -Name OpenAI.Codex | ForEach-Object { $_.Version }"`
  （输出每行一个版本号；不加 `-Command Get-AppxPackage -Name OpenAI.Codex` 那种形式，
  那样输出的是格式化对象的多属性行 Name/Publisher/…/Version，不是版本列表），
  沿用 devenv 子进程防护惯例（CREATE_NO_WINDOW、Job Object、kill_on_drop、
  15s 超时（C13，与 `DEVENV_PROBE_TIMEOUT_SECS` 同源））。
  多行输出时**取最大版本**——多行的实际成因是同一用户下多个版本/架构并存
  （如升级过渡期）；不带 `-AllUsers` 时 `Get-AppxPackage` 只返回**当前用户**
  已注册的包，预配（provisioned）包不在其中，不存在"预配+用户各一份"的情况；
  空输出一律判 `NotRegisteredForUser`（无法区分"未安装"与"当前用户未注册"：
  预配安装但未注册时同样空输出），文案"未安装或当前用户未注册（预配安装后
  可能需重新登录）"；`NotInstalled` 仅用于 CLI（PATH 无命中）。
  命令失败/超时为 `ProbeFailed`。不用 WinRT `PackageManager`：省 `windows` crate
  feature 与 COM 初始化，与 devenv 的子进程探测模式一致。
- **Codex CLI（两平台）**：PATH 解析出首个 `codex`(`.exe`) → `--version`。
  复用 devenv 的能力：`SearchEnv::find` 与 `devenv::run` 当前为模块私有，
  **需提升为 `pub(crate)`**（交付物加入 devenv.rs；`with_login_shell_path` 已是 `pub`，
  配套的 `pick_login_shell` 需一并提为 `pub(crate)`）；输出 `codex-cli X.Y.Z` 取版本段。
  **macOS 必须经 `with_login_shell_path` 合并登录 shell PATH**（否则 Finder 启动的
  CodexHelper 看不到 Homebrew/npm 安装的 codex）。PATH 无命中 = 未安装。
  UI 中该行命名为"Codex CLI（独立安装）"；ChatGPT Desktop 自带的
  `app/resources/codex.exe` 版本随应用、不在本行统计。
- **CLI 本地缓存兜底**：远端失败、或任何原因无法得到远端版本时，读 `HelperPaths`
  的 codex home 下 `version.json`（Codex CLI 自身更新检查缓存，**非公开格式**：
  实测形如 `{"latest_version":"0.163.0","last_checked_at":"2026-10-16T05:53:49.254443Z",
  "dismissed_version":null}`）的 `latest_version`；结果带来源标记 `CodexLocalCache` 与
  `checked_at`，并把**远端失败的简要原因**写进 `Ok.fallback_from`（不吞掉），
  UI 显示"远端不可达（github api 403 限流），以下为 Codex 本地缓存，检查于 X"。
  解析失败**静默忽略**（兜底的兜底没有意义）；该结果仍是"远端可用的降级近似"，
  `fallback_from` 字段保留"为什么不是远端结果"的语义（不参与"远端可达"判定）。

### 4.4 Tauri 命令与前端

- 命令放进 `apps/desktop/src-tauri/src/commands.rs`（按 `detect_dev_env` 先例）：
  `#[tauri::command] check_component_updates(state: State<AppState>) -> ComponentUpdates`（serde camelCase；
  **直接复用** §4.1 的 `ComponentUpdates` 具名结构，不另建 `ComponentUpdatesPayload`）。
  内部：`state.manager.status().await` 取 `status.mode` 与 `proxy.state`（C4）算出网络路径，
  代理地址为 `ManagerOptions` 的端口（`CODEX_HELPER_PROXY_PORT` 覆盖值同源），
  再调 helper-core `check_all`。命令注册进 `main.rs` invoke_handler。
- `types.ts`：`ComponentUpdates` / `ComponentUpdate` / `LocalState` / `RemoteState` /
  `Comparison`（与 §4.1 序列化形状对齐：内部标签 `kind` + camelCase 字段；
  `Comparison` 为字符串字面量联合）。
- **模块头注释约定（交付物）**：`component_updates.rs` 模块头注明——门禁的本进程
  放行对进程内**任何**经 17891 的客户端生效，门禁本身不校验目标；因此约定
  **进程内只有组件检测把 17891 当代理使用，新增进程内代理客户端须评审**（见 §7
  不变量与 §8 交付物）。
- `api.ts`：`checkComponentUpdates()`。
- UI：更新区下方新增"组件版本"区块（`updatepanel.ts` 同款 `el()` 模式新建
  `componentpanel.ts`）：两行组件 ×（本地 / 最新 / 状态徽标），一个"检查组件更新"按钮；
  有更新时行内展示 `v新版本` 与下载 URL（可复制，不自动打开浏览器——内网机器打开也没用）；
  ChatGPT 行的下载文案引导"请通过离线包管线更新（离线安装还需 ChatGPT-License.xml）"；
  CLI 行展示 release 页面 URL。检测中禁用按钮（`checking` 为前端状态）；
  失败显示原因（远端不可达 / 未安装 / 未注册 / 超时 / 清单无效 / Secure 代理未就绪）；
  本地缓存兜底时显示“远端不可达（原因），以下为 Codex 本地缓存，检查于 X”
  （`fallbackFrom`）。
- 纯函数按先例新建 `componentformat.ts` + `componentformat.test.ts`
  （`componentRowViewState(row)`：状态文案与徽标 class），`node --test` 断言；
  不塞进 `format.ts`。
- `dev-mock.ts`：三场景——两组件最新、CLI 有更新、远端不可达（本地缓存兜底）。

### 4.5 测试

| 层 | 内容 | 方式 |
| --- | --- | --- |
| 纯函数 | 清单解析（正常/缺字段/未知字段容忍/非 https 或白名单外拒绝）、release 解析、302 末段取 tag 与正则、版本比较（点分数字缺段按 0、semver 预发布与 0.0.0 开发版→Unknown、local_newer）、输出解析（Get-AppxPackage 多行取最大/空输出、`--version`、`version.json`）、序列化形状断言（内部标签 `kind`、字段 camelCase、`fallbackFrom` 仅缓存兜底时出现） | `cargo test` |
| HTTP | wiremock：清单 200/404/超时/白名单外拒绝、GitHub API 正常与 403→302 回退、302 Location 缺失/正则不过；**网络路径决策**：mode=Secure 且非 Running（注入假状态）→ 零 HTTP 请求、remote=Blocked | wiremock（`[dev-dependencies]` 已有） |
| 子进程 | 假探测函数注入（`CheckContext`），不发真 spawn | `cargo test` |
| 前端 | `componentRowViewState` 文案/禁用逻辑（含 Blocked"Secure 代理未就绪"、`fallbackFrom` 缓存兜底文案） | `node --test` |

### 4.6 依赖变更与风险

- workspace `Cargo.toml`：`reqwest` 追加 `features = ["native-tls"]`（**不加 `json`**）。
  native-tls 使用系统证书库（Windows SChannel / macOS Security.framework），
  兼容企业 TLS 检查设备；与自身升级渠道（tauri-plugin-updater）选型一致，
  Cargo.lock 中 `native-tls`、`hyper-tls`（来自 reqwest 0.13.5）已就位，本次追加
  只是让 helper-core 的 reqwest 0.12 也启用同一后端，不引入新依赖树。
  （注：tokio-rustls/rustls 已因 Secure 上游 mTLS 以 `ring` feature 编进 helper-core，
  macOS→windows-msvc 交叉编译路径已打通，`scripts/msvc-shim` 已存在——TLS 后端
  选型与该交叉编译问题无冲突。）
- 响应体解析用 `.bytes()` + `serde_json::from_slice`，不依赖 reqwest `json` feature
  （少一条 feature 链）。
- `gateway.rs` 的 no_proxy 行为不变（其 client 独立构建，未共用）。
- 体积影响：native-tls 在两个平台都链系统库，无 webpki-roots 类额外数据文件。
- **已否决方案（记录）**：原拟用 `rustls-tls` + webpki-roots。否决理由：webpki-roots
  只含 Mozilla 根集合，不含企业根证书，过不了企业 TLS 检查设备（内网恰好有此场景）；
  与自身升级渠道选型一致（native-tls）。（先前版本所列“ring 交叉编译”理由与事实
  不符——ring 已因 Secure 上游 mTLS 编进 Windows 目标，已删去。）
- GitHub API 匿名限流（60 次/时/IP，Legacy 下全内网共享；Codex CLI 自身检查也计入）：
  手动触发频率下可接受；403/429 走 §2.2 回退链（API → 302 → 本地缓存）。

## 5. 兼容性

- 现有自身升级链路（`update.rs` / `updatepanel.ts`）零改动，仅在其下追加新区块。
- 网络路径按 C4 决策表：Legacy 模式下代理未运行（Stopped）时检测走直连：外网机器
  行为不变，内网机器直连失败如实展示（与网关状态灯“不可达”同一语义，不误导）；
  External 时直连并在结果中注明；**Secure 模式下代理未就绪（非 Running）时不发
  任何远端请求**（零出站），远端列展示“Secure 代理未就绪”——绝不静默直连。
- Secure 模式下检测经代理（门禁对本进程 PID 放行，§7），不回退直连——
  与 Secure“绝不自动回退”的承诺一致。
- `codex --version` 探测不启动引擎、不读配置，对正在运行的引擎无影响。
- 本地 `0.0.0`（开发版）不提示更新：比较结果为 `Unknown`，文案“开发版，不比较”。
- macOS：ChatGPT 行显示“暂不支持（二期）”，CLI 行全功能。
- 经代理的检测请求会出现在"最近连接"列表（主机与端口级，无 URL 细节），属预期。

## 6. 验收

- `cargo test --workspace` 与前端 `node --test` 全绿（含新增用例）；gate.sh 全绿。
- 依赖复核：`cargo tree -i native-tls` 确认 helper-core 的 reqwest 已切到 native-tls，
  且 check-windows 门禁（scripts/check-windows.sh）通过。
- Windows 真机（代理运行）：检查两组件，结果与
  `https://persistent.oaistatic.com/codex-app-prod/windows-release.json`、
  GitHub latest 一致；Get-AppxPackage 版本与"设置→应用"一致。
- 上游连通性确认：上游 SOCKS5 服务端白名单（openai-restrict）放行
  `persistent.oaistatic.com` 与 `api.github.com` / `github.com`
  （后者用于 releases/latest 302 回退）。**已确认（2026-10-21）**：服务端已放行上述地址。
- Windows 真机（代理停止，Legacy）：直连路径生效或失败原因正确展示。
- 清单 `downloadUrl` 白名单外（wiremock 注入伪清单）被拒，UI 显示“清单无效”而非假版本。
- **Secure 代理未就绪时零出站**：Secure 模式下停止代理（state 非 Running）触发检测，
  远端列显示“Secure 代理未就绪”、无任何出站请求（抓包/日志核对），本地探测照常。
- Secure 模式真机（代理运行）：检测请求经代理成功（门禁对本进程放行），“最近连接”可见对应记录；
  他进程（如 curl 走 17891）仍被 403 拒绝（回归确认门禁未松动）。
- 未安装组件的机器：显示“未安装”与最新版本，无崩溃。
- CLI 本地缓存兜底：远端不可达时展示“远端不可达（原因），以下为 Codex 本地缓存，检查于 X”。
- 本地 `0.0.0` 开发版：不提示更新，显示“开发版，不比较”（Unknown）。
- macOS：CLI 检测可用（登录 shell PATH 合并生效）；ChatGPT 行显示二期占位。

## 7. D-Q1 定案：Secure（进程门禁）模式下放行本进程（方案 d）

原 §7 列举的 (a)(b)(c) 已全部否决或后置，定案为**方案 (d)：门禁对本进程 PID 放行**。
（本节由"开放决策"改为"定案说明"；(a)(b)(c) 保留在下方"已否决/后续"留档。）

**结论**：连接所有者解析出的 `PeerInfo.pid` 等于 CodexHelper 自身进程
（`std::process::id()`）时，直接 `Decision::Allow { role: "codex-helper-self" }`——
不改策略模型、不改注册包结构、不新增白名单角色。

**论证**：

- CodexHelper 本进程是**策略执行者本身**：代理、门禁、上游凭据都在它手里，
  它已经可以发起任意经批准的出站；允许它自己的检测请求经代理**未扩大信任面**。
  这与 (b) 的"把映像哈希加进注册包白名单"语义等价，但不需要动 `process_policy.rs`
  的策略模型、管理员签名链与注册包格式。
- 插入点（经核实现有代码）：`connection_gate.rs::ConnectionGate`——在
  `resolve_with_retry` 成功返回 `PeerInfo` 之后、`admit` 计算映像哈希之前，
  比较 `peer.pid == std::process::id() as i32`（`PeerInfo.pid` 为 `i32`）。命中即返回
  Allow，跳过映像摘要计算与策略判定。解析本身仍按四元组、仍经复查预算、仍 fail-closed
  （解析失败/超时照旧 403，绝不会因这条放行而放宽其他路径）。
- 检测 URL 全部为**编译期常量**（§2.1/§2.2 的固定地址），本进程不可能借门禁放行
  去请求任意目标——放行的只是"发起者身份"，目标仍受上游 openai-restrict 白名单约束。

**改动点**：

- `connection_gate.rs`：新增 `self_pid` 字段（构造时取 `std::process::id()`）；
  `admit` 前置 PID 判定（或新增 `admit_with_self` 并在 proxy.rs 调用处替换）。
- `proxy.rs` 门禁调用处（现 `gate.resolve_with_retry(local, remote).map(|peer| gate.admit(&peer))`
  闭包）无需改动语义——放行逻辑收敛在 `ConnectionGate` 内部。
- 日志字段：自身放行记
  `proxy.gate_allowed { "self": true }`（不带 pid——pid 是本进程的，日志里无意义）；
  其他路径日志不变。

**不变量（须写入 `component_updates.rs` 模块头注释，列入交付物）**：门禁的本进程
放行对进程内**任何**经 17891 的客户端生效，门禁本身不校验目标。因此约定：
**进程内只有组件检测把 17891 当代理使用，新增进程内代理客户端须评审**
（评估是否落入门禁放行面与上游白名单约束）。

**测试**（`connection_gate.rs` 单测）：

- 解析出 PID == 本进程 → Allow（即使策略不含该映像）；
- 解析出其他 PID → 照旧走 `decide` 策略判定（未批准映像仍 Deny）；
- 解析失败 → 照旧 fail-closed（回归）。

**已否决/后续**：

- **(a) 回退直连（已否决）**：Secure 模式的内网机器直连大概率不通，结果仍失败；
  且与"绝不自动回退"原则相悖——Secure 的安全承诺不允许"代理层失败就静默绕过"。
- **(b) 注册包白名单加 codex-helper 角色（后置为独立设计）**：语义最正式
  （管理员可审计、可撤销该角色的哈希），但需改 `process_policy.rs` 策略模型、
  管理员侧签名工具与注册包结构（`POLICY_VERIFY_KEY_HEX` 生态），超出本设计范围。
  若后续做 (b)，方案 (d) 可平滑退役（策略命中优先级高于 PID 判定即可）。
- **(c) Secure 模式禁用检测（已否决）**：功能覆盖不到最需要本功能的场景
  （Secure 模式的内网机器），实现最小但语义躺平。

## 8. 交付物清单

| 文件 | 动作 |
| --- | --- |
| `crates/helper-core/src/component_updates.rs` | 新增（检测编排 + 解析 + 比较纯函数 + 数据结构；**模块头注明不变量**：门禁本进程放行对进程内任何 17891 客户端生效，新增进程内代理客户端须评审，见 §7） |
| `crates/helper-core/src/lib.rs` | 注册模块 |
| `crates/helper-core/src/devenv.rs` | `SearchEnv::find`、`run` 提升 `pub(crate)`（`with_login_shell_path` 已是 `pub`，`pick_login_shell` 一并提为 `pub(crate)` 供 macOS 探测复用） |
| `crates/helper-core/src/connection_gate.rs` | 门禁对本进程 PID 放行（§7 方案 d）+ 单测 |
| `crates/helper-core/tests/component_updates.rs` | 新增（wiremock + 注入探测） |
| `Cargo.toml`（workspace） | reqwest 追加 native-tls（不加 json） |
| `apps/desktop/src-tauri/src/commands.rs` | 新增 `check_component_updates` 命令（按 detect_dev_env 先例，不新建文件） |
| `apps/desktop/src-tauri/src/main.rs` | 注册命令 |
| `apps/desktop/src/componentpanel.ts` | 新增前端区块 |
| `apps/desktop/src/componentformat.ts` + `componentformat.test.ts` | 新增纯函数与测试（按 devenvformat 先例，不塞进 format.ts） |
| `apps/desktop/src/{types,api,dev-mock}.ts` | 类型/封装/mock |
| 本文档 | 修订（含 §10 评审修订记录） |

## 9. 留给实施计划（builder）的事项

- ~~`ComponentUpdatesPayload` 具体字段命名与 serde 细节~~（已定：不再另建
  `ComponentUpdatesPayload`，直接复用 §4.1 的 `ComponentUpdates` 具名结构；
  枚举统一**内部标签** `#[serde(tag = "kind", rename_all = "camelCase",
  rename_all_fields = "camelCase")]`，序列化示例见 §4.1）。
- `componentpanel.ts` 的 DOM 结构与样式 class（沿用现有 update-region 风格）。
- wiremock 用例的清单夹具数据。
- GitHub API 响应体仅取 `tag_name`，其余忽略的具体解析写法。
- 302 回退请求是否复用同一 client 实例（预计是——均 `redirect(Policy::none())`）。

## 10. 评审修订记录（2026-10-21）

1. **§2.1 版本字段纠错**：比较一律用四段 `buildVersion`/`packageVersion` 与
   `Get-AppxPackage` 四段 Version；`releaseVersion` 仅展示、绝不参与比较，写明误用
   后果（永远提示有更新）；删"去尾部零段对齐"（缺段按 0 即可）；schema 补全实测字段
   （主清单无 `packages`/`packagePublisher`），解析容忍未知字段；修正文中错误示例
   版本号（原"buildVersion 为三段 26.1013.40934"系误读 `releaseVersion`）。
2. **数据源调整**：ChatGPT 主源改为 `windows-release.json`（packageVersion + x64
   downloadUrl，带版本号不可变地址），理由=应用自身 Store 不可用时即用它 + Store 版与
   离线包可能分叉 + 用户升级路径是离线包；`windows-store-update.json` 定为**不请求**
   （择一：不请求，减少一次出站；其信息无增量）。C9 改为 https + 主机/路径前缀白名单
   `https://persistent.oaistatic.com/codex-app-prod/`，说明 packageIdentity 对防伪意义
   有限；补 HEAD `x-ms-meta-package_version` 作验收对照。
3. **C4 与 §7 矛盾消解**：不再用 `probe_existing`（自识应答可伪造）；改由 Tauri 命令从
   `AppState` 的 Manager 状态快照（`ProxyStatus.state` / `SecureStatus.active`，实测
   均为 `Manager::status()` 公开返回）判定：Running→经代理（含 Secure，依赖 §7 门禁
   放行）、External→直连并注明、Stopped→直连；Secure 绝不直连。
4. **D-Q1 定案为方案 (d)**：门禁对本进程 PID（`std::process::id()`）放行；插入点经核实
   为 `connection_gate.rs` 的 resolve 之后、admit 映像哈希之前（`PeerInfo.pid` 为
   `i32`，比较需 `as i32`）；(a)(b)(c) 保留为已否决/后续并给理由；交付物加入
   connection_gate.rs；含日志字段与两条单测要求。
5. **CLI 本地缓存兜底**：读 codex home `version.json` 的 `latest_version`（非公开
   格式，实测见 §4.3），带 `CodexLocalCache` 来源标记与 `last_checked_at`，UI 显示
   "来源：Codex 本地缓存，检查于 X"；解析失败静默忽略、仅作兜底。
6. **GitHub 限流**：写明 Legacy 下全内网共享出口 IP 与 60 次/时（Codex CLI 自身检查
   计入）；403/429 回退 releases/latest 302（`redirect(Policy::none())` 读
   `Location` 末段）+ 与 prepare.py 相同正则；"不做多源重试"改写为"仅一条与离线管线
   同构的回退"；回退链 API → 302 → 本地缓存。
7. **TLS 选型改为 native-tls**：C5 与 §4.6 重写；与自身升级（tauri-plugin-updater）
   一致，Cargo.lock 已有 native-tls/hyper-tls（经核实来自 reqwest 0.13.5）；不加
   `json` feature（`bytes()` + `serde_json::from_slice`）；rustls/webpki-roots 论证
   移入"已否决方案"；验收改为复核 `cargo tree -i native-tls` 与 check-windows 门禁。
8. **数据结构重构**：删除 `update_available: Option<bool>` 与 `checking`（前端状态）；
   拆 `LocalState`（installed/not_installed/not_registered_for_user/probe_failed）与
   `RemoteState`（ok{version,source}/unreachable/invalid/unsupported_platform）+
   派生 `Comparison`（update_available/up_to_date/local_newer/unknown）；
   §4.1 给出最终 Rust 结构草图（camelCase、变体名即 tag）；semver 处理预发布后缀与
   `0.0.0`；local 比 remote 新 → local_newer、不提示更新。
9. **本地探测细节**：`devenv::run`/`SearchEnv::find` 需提升 `pub(crate)`（交付物加
   devenv.rs）；macOS 经 `with_login_shell_path` 合并登录 shell PATH；
   `Get-AppxPackage` 多行取最大版本；空输出文案"当前用户未注册（预配安装后可能需
   重新登录）"；CLI 行命名"Codex CLI（独立安装）"并注明 ChatGPT 自带 codex.exe
   不在本行统计。
10. **下载地址与文案**：CLI 给 release 页面 `…/releases/tag/<tag>`；ChatGPT 给带版本号
    downloadUrl + 引导"请通过离线包管线更新（离线安装还需 ChatGPT-License.xml）"。
11. **小项**：§4.1/§2.1 删 `plutil` 解析（二期）；appcast 补充需查询参数说明；Tauri
    命令进 `commands.rs`（删新建 src-tauri 文件的交付物）；前端纯函数新建
    `componentformat.ts` + 测试（不塞 format.ts）；验收新增上游 SOCKS5 白名单
    （openai-restrict）确认 persistent.oaistatic.com 与 api.github.com / github.com；
    注明经代理的检测会出现在"最近连接"列表；§8 清单同步全部变更。
12. **后续事项（不纳入本设计）**：aitools-download 的 manifest.json 增记
    `desktop_package_version`，便于离线包侧对齐 ChatGPT 版本。

### 第二轮复审修订（2026-10-21）

1. **§4.3 PowerShell 命令回归**：改回管道式
   `-Command "Get-AppxPackage -Name OpenAI.Codex | ForEach-Object { $_.Version }"`
   （每行一个版本），删除裸 `-Command Get-AppxPackage …` 形式（输出的是格式化对象
   多属性行，不是版本列表）；多行取最大，成因修正为**同一用户下多版本/架构并存**
   （升级过渡期）；删除“预配+用户各一份”说法，并写明不带 `-AllUsers` 时
   `Get-AppxPackage` 只返回**当前用户**已注册的包。
2. **C4/§4.2/§5 漏洞修复（Secure 直连违背）**：判定改用 `status.mode`（状态文件
   模式）而非 `secure.active`：`mode == Secure` 且 `state == Running` → 经代理；
   其余 → **零出站**，新增 `RemoteState::Blocked { detail }` 变体（文案“Secure
   代理未就绪”），本地探测照常；`mode == Legacy`：Running→经代理、External→直连
   并注明、Stopped→直连。§1/§4.1/§4.2/§4.5/§5/§6/§8 同步（验收新增“Secure 代理
   未就绪时零出站”用例）。
3. **§4.1 serde 草图修正**：枚举改统一**内部标签**
   `#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]`
   （serde ≥ 1.0.183，workspace 为 1.0.229）；写明 `rename_all` 只重命名变体名、
   `rename_all_fields` 才管 struct 变体字段，及默认外部标签形状不统一的问题；补序列化
   示例 JSON；§9“tag 方式已定：变体名即 tag”同步改正。
4. **§4.1 结构缺口**：`check_all` 返回值由 `Vec<ComponentUpdate>` 改为具名结构
   `ComponentUpdates { chatgpt_desktop, codex_cli }`；`ComponentUpdate` 补
   `#[derive(Serialize)]`；`direct_reason: Option<&'static str>` 注明仅用于序列化；
   §4.4 直接复用 `ComponentUpdates`、不再另建 `ComponentUpdatesPayload`。
5. **§4.5 表格分隔行**：修正为 3 列；全文其余表格（决策表、交付物清单）核对无2列
   对3列表头问题。
6. **空输出语义（§4.3/§4.1）**：ChatGPT 空输出一律 `NotRegisteredForUser`，文案
   “未安装或当前用户未注册（预配安装后可能需重新登录）”；`NotInstalled` 仅用于
   CLI（PATH 无命中）。
7. **§4.6 已否决方案理由与事实不符**：删去“ring 交叉编译是 check-windows 门禁
   存在的原因”类推测；否决理由仅保留“webpki-roots 不含企业根证书”与“与自身
   升级渠道选型一致”；正文注明 rustls/ring 已因 Secure 上游 mTLS 编进目标、
   `scripts/msvc-shim` 已存在。
8. **本地缓存兜底语义（§4.1/§4.3/§6）**：`RemoteState::Ok` 增加可选字段
   `fallback_from: Option<String>`（仅 `CodexLocalCache` 时有值，记录远端失败
   简要原因），UI 文案“远端不可达（原因），以下为 Codex 本地缓存，检查于 X”，
   远端失败原因不再被吞。
9. **§7 新增不变量**：门禁本进程放行对进程内**任何**经 17891 的客户端生效、不校验
   目标；约定“进程内只有组件检测把 17891 当代理使用，新增进程内代理客户端须评审”，
   写入 `component_updates.rs` 模块头注释（列入 §8 交付物与 §4.4）。
10. **C6 开发版语义（C6/§5/§6）**：本地 `0.0.0` 比较结果定为 `Unknown`（文案
    “开发版，不比较”），不提示更新；替换“视为开发版参与比较”；§2.2/§4.5 同步。

### 实施偏差（2026-10-21）

实施中对本设计的补充与偏差共 15 条（D1–D15），以实施计划
`docs/superpowers/plans/2026-10-21-component-update-check.md` 的“实施偏差与设计缺口”表为准。
其中改变数据形状或对外语义的几条：

- **D1**：`check_component_updates` 返回 `Result<ComponentUpdates, ErrorPayload>`（Tauri 对带
  `State` 的 async 命令的要求），恒 `Ok`，前端形状不变。
- **D2**：`LocalState` 新增 `UnsupportedPlatform`（`{"kind":"unsupportedPlatform"}`），用于 macOS
  的 ChatGPT 行本地列。
- **D6/D7**：GitHub API 仅 403/429 走 302 回退，其他失败直接本地缓存兜底；API 的
  `tag_name` 也按 `^rust-v\d+\.\d+\.\d+$` 校验。
- **D8**：清单缺 x64 条目时版本照常比较、不给下载地址。
- **D9/D10**：`schemaVersion != 1` 或响应体超过 256 KiB 判 `invalid`。
- **D11**：Legacy + Stopped 的直连原因文案为“本地代理未运行，已直连”。
- **D14**：先完成本地探测再发远端请求。
- **C9 收紧**：下载地址原串含非可打印 ASCII（控制符、空白、双向符、全角符号）一律拒绝，
  采信后返回规范化地址（展示与校验对象一致）。
