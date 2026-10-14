# CodexHelper 第二阶段：macOS MVP 设计

> 基线：`2026-09-23-codex-helper-design.md`（下称“原设计”），代码基线 `f4b3bae`（v0.1.2）。
> 本文只写与原设计不同或新增的部分；未提及的行为（固定参数、分流规则、启用/停用/对账流程、
> 界面结构、安全约定）一律沿用原设计。原设计 §1.2“不支持 macOS 正式使用”一条由本文取代。

## 1. 意图与范围

在 macOS 上提供与 Windows 版相同的受管模式，供内部少量同事试用，快速验证“Codex 桌面版 →
本地分流代理 → 网关 / SOCKS5”在 macOS 上能否跑通。MVP 不追求安全与性能上的完备，投入小的
加固顺手做。

成功标准：同事在 Apple Silicon 或 Intel Mac 上从 GitHub Release 下载 dmg，安装、录入 Key、
启用受管模式、重启 Codex 后，Codex 桌面版能经网关完成对话，“最近连接”里 OpenAI 域名走 SOCKS5。

### 1.1 已拍板的决策

| 编号 | 决策 |
| --- | --- |
| D1 | 面向内部少量同事试用（不是正式推广） |
| D2 | ad-hoc 签名，不做 Developer ID 签名与公证 |
| D3 | API Key 存 Keychain，全部经系统自带 `/usr/bin/security` 读写 |
| D4 | Universal 二进制（arm64 + x86_64） |
| D5 | `release.yml` 增加 macOS job，tag 触发后上传 dmg 到 GitHub Release |
| D6 | 不接入内部升级渠道 |
| D7 | 安装说明写明“先撤销再删除”，并在托盘增加“撤销配置并退出” |
| D8 | 保持程序坞图标 + 菜单栏图标（现状） |

### 1.2 非目标

- Developer ID 签名、公证、Mac App Store。
- 内部升级渠道（macOS 界面隐藏“检查更新”）。
- ChatGPT.app 界面层流量（同原设计 §9）。
- 卸载程序（.app 拖进废纸篓没有钩子）。
- Linux 正式支持（继续是开发模式）。

### 1.3 反目标（做到了也算失败）

- 弄坏同事现有的 Codex 配置且无法还原：`config.toml` 逐字节还原的保证在 macOS 上必须同样成立。
- Codex 每次请求都弹钥匙串授权框。
- 改动造成 Windows 版行为变化：Windows CI 门禁与 `scripts/check-windows.sh` 必须保持全绿。

## 2. 前提验证（2026-10-14，本机实测）

环境：macOS 27.0.1 arm64，ChatGPT.app（`com.openai.codex` 26.1007.30912），
引擎 `/Applications/ChatGPT.app/Contents/Resources/codex-cli/CodexCLI.app/Contents/MacOS/codex`
（codex-cli 0.162.0-alpha.17.2，hardened runtime，无 App Sandbox entitlement）。
均用临时 `CODEX_HOME` 与测试 Keychain 条目，验证后已清理。

| 前提 | 结果 |
| --- | --- |
| macOS 版引擎读取 `$CODEX_HOME/.env` | 成立：写入与本工具相同格式的受管块后，`codex doctor --json` 的 `proxy env vars present = HTTP_PROXY, HTTPS_PROXY, ALL_PROXY, NO_PROXY`；无 `.env` 时为 `none` |
| `auth.command` 在 macOS 上可调用外部程序 | 成立：`codex exec` 以 `args = ["get", "codex-helper/managed-gateway"]` 调用了脚本，网关收到 `Authorization: Bearer <Key>`（`/v1/models` 与 `/v1/responses` 均带） |
| Codex 子进程经 `/usr/bin/security` 读 Keychain 不弹框 | 成立：同一 `security` 写入、Codex 调起的脚本内 `security find-generic-password -w` 读回，无授权框、无超时 |
| `security` 退出码 | 条目不存在：`find` / `delete` 均为 44；成功为 0。`security -i` 从 stdin 执行的子命令失败时进程退出码非零（重复条目 45、参数错误 2），写入失败可直接由退出码发现 |
| `-i` + `-U` 覆盖写入后由 Codex 读取 | 成立：端到端测试中 Codex 读到的就是经 `security -i` + `-U` 覆盖后的值，不弹框 |
| `security -w` 的输出格式 | 纯 ASCII 原样输出；含非 ASCII 字节时输出十六进制串（`sk-中文` 读回 `736b2de4b8ade69687`），无法与“恰好是十六进制的 ASCII Key”区分 |
| 本机到网关 / SOCKS5 | `10.20.30.61:8080`、`:7891` TCP 可达 |

未验证：桌面版 GUI 实际发起对话的全链路（留到 §9 实机验收）。

## 3. 凭据存储（Keychain）

### 3.1 后端

`helper-core::credential` 新增 `#[cfg(target_os = "macos")]` 后端；Windows 不变；其他非 Windows
平台保留进程内 `dev_store`。macOS 后端的读、写、删都经 `/usr/bin/security`（绝对路径，不查 PATH），
不链接 Security.framework。理由：Keychain 条目的 ACL 记录创建它的程序；ad-hoc 签名的本工具每次
构建签名都会变，如果由本工具直接调 API 创建条目，Codex 调起的凭据程序读取时会弹授权框。
全部经同一个 Apple 签名的 `security` 程序，§2 已实测不弹框。

| 项 | 值 |
| --- | --- |
| 条目类型 | generic password，登录钥匙串（默认钥匙串） |
| service（`-s`） | `codex-helper/managed-gateway`（即 `consts::CREDENTIAL_TARGET`） |
| account（`-a`） | `codex-helper` |
| 读 | `security find-generic-password -s <svc> -a <acct> -w`：退出码 0 → stdout 去掉末尾换行即 Key；44 → `Ok(None)`；其他 → 错误 |
| 写（覆盖） | `security -i`，从 stdin 送入一行 `add-generic-password -U -a <acct> -s <svc> -w "<转义后的 Key>"`。Key 不出现在 argv（`ps` 看不到）；转义规则：`\` → `\\`，`"` → `\"` |
| 删 | `security delete-generic-password -s <svc> -a <acct>`：0 或 44 都视为成功 |
| 超时 | 每次子进程调用 10 秒超时，超时按错误处理并结束子进程 |

错误信息只含操作名与退出码，不含 stdout、stderr 原文（stdout 可能是 Key）。

### 3.2 Key 字符约束（跨平台）

`save_key` 在现有 trim、非空校验之后加一条：Key 只能由可打印 ASCII（0x21–0x7E）组成，
否则拒绝保存，提示“Key 只能包含英文字母、数字和符号”。依据是 §2 的十六进制输出问题；
该约束对 Windows 同样生效，网关 Key 本身就是 ASCII，不影响现有用户。

### 3.3 测试

- 转义函数、退出码映射为纯函数单测，各平台都跑。
- 真实 Keychain 读写删测试仅 `cfg(target_os = "macos")`，service 用带随机后缀的测试专用名，
  结束时删除；CI `macos-latest` 上运行。若 CI 的 runner 没有可用的默认钥匙串导致失败，
  允许把这组测试标为 `#[ignore]` 并在本机手动跑，在计划里注明。
- `codex-helper-credential get` 的 stdout / stderr 约定测试采用进程内方式：用
  `SystemCredentialStore::new(<随机 service>)` 注入 `run` / `run_get`，走真实 Keychain。不为测试给生产二进制
  加 service 覆盖入口（保持“只接受固定 target”的约定）；子进程级的真实链路由 §9.2 实机验收覆盖。
- 硬性规则：在 macOS 上跑 `cargo test` 绝不读写生产条目 `codex-helper/managed-gateway`。现有
  `apps/credential/tests/cli.rs` 里以 `cfg(not(windows))` 假设“内存后端、无副作用”的子进程用例（如
  `cleanup_with_purge_key_succeeds_when_nothing_exists`）改为同时排除 macOS；实施时把所有在 macOS 上
  会触及生产 target 的用例逐一排查。

## 4. 平台标识与界面

### 4.1 平台支持标识

- `Status.platform_supported` 与 `DevEnvReport.platform_supported` 改为 `cfg!(any(windows, target_os = "macos"))`。
  macOS 不再显示“开发模式”横幅；Linux 继续显示。开发环境检测在 macOS 上继续用 `python3`，
  Store 别名等 Windows 专属逻辑不变。
- 新增“是否支持内部升级”标识（仅 Windows 为 true）。为 false 时前端隐藏“检查更新”入口，
  `check_update` / `install_update` 命令直接返回 `code = "update"` 的错误，不请求检测地址。
  放在 `Status` 里还是单独的命令里由实现者决定。

### 4.2 安装位置检查

`auth.command` 写的是当前可执行文件旁边的凭据程序绝对路径。从 dmg 里直接运行、或带隔离属性在
“下载”里运行时，macOS 会把应用放到随机临时路径（App Translocation）或 `/Volumes/...`，
这个路径之后会失效。

- 判定：`current_exe()` 路径含 `/AppTranslocation/` 或以 `/Volumes/` 开头，即“安装位置不对”。
  放在 `/Applications`、`~/Applications` 或其他普通目录都算正常。
- 界面：顶部横幅“请先把 CodexHelper 拖到「应用程序」文件夹，再从那里打开”。
- 行为：启用、打开开机自启均被拒绝（新错误码，提示同横幅）；启动对账不把 `auth.command`
  改写为这个临时路径（已启用时保留旧值，只显示横幅；状态里的“配置已受管”由实现者按旧值合理判定）。
  其余功能照常。
- 判定函数写成接收路径的纯函数，单测覆盖。

## 5. 退出前的 Codex 进程检测

`process::is_codex_running()` 在 macOS 上执行 `/usr/bin/pgrep -x codex`：退出码 0 → 在运行；1 → 未运行；
其他或无法执行 → 按“在运行”处理（与 Windows 快照失败时一致）。桌面版引擎进程名实测为 `codex`；
终端里运行的 codex CLI 也会命中，它同样读 `.env`，弹确认是对的。进程名常量按平台区分
（Windows `codex.exe`，macOS `codex`）。

## 6. 托盘“撤销配置并退出”（仅 macOS）

macOS 没有卸载钩子，用这个菜单项代替 Windows NSIS 卸载钩子里的 cleanup。

- 菜单：显示窗口 / 受管模式 / ── / 撤销配置并退出 / 退出。Windows 菜单不变。
- 点击后弹确认框：“将还原 Codex 的 config.toml、移除 .env 受管块、关闭开机自启，然后退出。
  API Key 保留在钥匙串。删除 CodexHelper 前请先做这一步；完成后需要重启 Codex。”
  按钮“撤销并退出” / “取消”，默认聚焦“取消”。
- 确认后依次：停止代理 → 执行现有 `manager::cleanup(paths, store, purge_key = false)` → 关闭开机自启（移除
  LaunchAgent）→ 退出。不再弹普通“退出”的 Codex 运行确认。
- 任一步失败：不退出，弹窗显示错误码，让用户重试或去主窗口处理；若代理已停而受管配置仍在，
  尽力重新启动代理，避免 Codex 留在“指向 17891 但无人监听”的状态。
- 只想清除 Key 的用户用主窗口现有的“清除 Key”；安装说明里也给出命令行删除方法。

## 7. 打包与发布

### 7.1 打包配置

- 新增 `apps/desktop/src-tauri/tauri.macos.conf.json`（Tauri 2 在 macOS 上自动合并的平台配置），
  `tauri.conf.json` 不动，Windows 打包不受影响：
  - `bundle.targets = ["app", "dmg"]`
  - `bundle.createUpdaterArtifacts = false`（D6；也因此不需要 `TAURI_SIGNING_PRIVATE_KEY`）
  - `bundle.macOS.signingIdentity = "-"`（ad-hoc，D2）
  - `bundle.macOS.minimumSystemVersion = "12.0"`
- Universal（D4）：`tauri build --target universal-apple-darwin`。凭据程序分别按
  `aarch64-apple-darwin` 与 `x86_64-apple-darwin` 构建，`lipo -create` 合成后放到
  `src-tauri/binaries/codex-helper-credential-universal-apple-darwin`，继续由现有
  `tauri.bundle.conf.json` 的 externalBin 引用。打包后凭据程序位于
  `CodexHelper.app/Contents/MacOS/codex-helper-credential`，与主程序同目录，`paths::credential_command_path`
  不用改。
- 新增 `scripts/build-macos.sh`：完成上述两步并产出 dmg。本机与 CI 共用这一份脚本。

### 7.2 发布（D5）

- `release.yml` 新增 `macos-release` job，`runs-on: macos-latest`，`needs: windows-release`
  （串行，避免两个 job 同时创建同一 tag 的 Release）。步骤：同样的 tag 与版本号校验 →
  安装两个 darwin target → `scripts/build-macos.sh` → 把 dmg 上传到同一个 GitHub Release。
- 不跑 `publish-update.sh`，`latest.json` 仍只含 `windows-x86_64`。
- 版本号升到 0.2.0，Windows 与 macOS 同步发布。升级渠道里的 Windows 用户会收到 0.2.0，
  Windows 侧的变化只有 §3.2 的 Key 字符约束。

### 7.3 安装说明（D7）

写在 GitHub Release 正文中（由 `release.yml` 的 release body 提供模板），内容：

1. 打开 dmg，把 CodexHelper 拖到「应用程序」。
2. 首次打开被拦截时：系统设置 → 隐私与安全性 → “仍要打开”；或执行
   `xattr -dr com.apple.quarantine /Applications/CodexHelper.app`。
3. 卸载：先在菜单栏图标选“撤销配置并退出”，再把应用拖进废纸篓。如果已经先删了应用、
   Codex 打不开对话：重新安装后执行一次撤销。
4. 删除 Key（可选）：`security delete-generic-password -s codex-helper/managed-gateway -a codex-helper`。

主窗口底部（仅 macOS）加一行小字：“卸载前请先在菜单栏选择「撤销配置并退出」”。

## 8. 沿用不改的部分

- 数据目录：`directories` 在 macOS 上解析为 `~/Library/Application Support/CodexHelper`（state.json、logs）。
- 开机自启：已配置的 `MacosLauncher::LaunchAgent` + `--minimized`。
- 窗口与程序坞（D8）：关闭窗口即隐藏，点程序坞图标或菜单栏“显示窗口”重新显示。
- 代理、`.env` 受管块、`config.toml` 校正与还原、对账、日志脱敏：代码不动。
- 退出确认、单实例：逻辑不动，只是 macOS 上进程检测变成真实结果（§5）。

## 9. 测试与验收

### 9.1 自动化

| 内容 | 环境 |
| --- | --- |
| security 转义、退出码映射、Key 字符约束、安装位置判定、pgrep 退出码映射 | 全平台 CI |
| 真实 Keychain 读写覆盖删除、缺失、特殊字符；`credential get` stdout / stderr 约定 | CI `macos-latest`（见 §3.3） |
| `platform_supported` 断言改为包含 macOS | 全平台 CI |
| Windows 交叉编译检查 `scripts/check-windows.sh`、现有全部测试 | 本机门禁 + CI |

### 9.2 macOS 实机验收（发布前）

至少一台 Apple Silicon；有 Intel Mac 时各跑一遍第 1–3 条。

1. 从 GitHub Release 下载 dmg，按 §7.3 安装，Gatekeeper 处理流程走得通。
2. 录入 Key → 启用 → 重启 ChatGPT.app → 对话成功；“最近连接”里 OpenAI 域名走 SOCKS5；
   全程没有钥匙串授权框。
3. 终端 `codex` CLI 同样可用。
4. 停用 → `config.toml` 与启用前逐字节一致（允许启用期间的无关修改保留）。
5. “撤销配置并退出” → `config.toml` 还原、`.env` 无受管块、`~/Library/LaunchAgents` 里无本工具条目、
   Keychain 里 Key 仍在；重启 Codex 后按原配置工作。
6. 开机自启：登录后只驻留菜单栏，受管模式自动恢复。
7. 直接在 dmg 里双击运行：显示安装位置横幅，启用被拒绝。
8. 受管模式开着且 Codex 在运行时点“退出”：弹出确认。
9. Windows 安装包回归：原设计 §13 中的安装、启用、对话三项。

## 10. 已知风险（MVP 接受）

| 风险 | 影响 | 应对 |
| --- | --- | --- |
| Keychain 条目由 `/usr/bin/security` 创建，本机任何进程调 `security` 都能不弹框读到 Key | 与 Windows Credential Manager 同类（当前用户任意进程可读），但不落明文文件 | 接受；正式推广时评估 Developer ID 签名 + 自建 ACL |
| ad-hoc 签名，每次下载新版需重新放行 Gatekeeper | 试用体验差一点 | 安装说明给出 `xattr` 命令 |
| 带隔离属性的凭据程序被 Codex 直接 exec 时可能被 Gatekeeper 拦 | Codex 拿不到 Key，请求失败 | §9.2 第 2 条验证；若发生，安装说明改为必须执行 `xattr` 命令 |
| 用户不撤销就删应用 | Codex 指向不存在的凭据程序，对话失败；LaunchAgent 残留 | §6 菜单项 + §7.3 说明；重装后撤销可恢复 |
| `pgrep -x codex` 误判（别的同名进程） | 多弹一次退出确认 | 接受 |
| macOS 版没有自动升级 | 试用者需手动下新版 | 接受（D6） |

## 11. 留给实现者的部分

- 平台差异的代码组织方式（`cfg` 分支、子模块划分）、子进程超时的实现。
- 安装位置、升级支持标识在 `Status` 里的字段名与错误码命名。
- `tauri.macos.conf.json` 的合并行为、universal 构建下 externalBin 的文件后缀、dmg 产物路径：
  以 Tauri 2.11 实际行为为准，实施时本机先跑通 `scripts/build-macos.sh` 再改 CI。
- 安装说明与界面文案的最终措辞。
- 任务拆分与顺序；建议先做 Keychain 后端（解除端到端阻塞），再做打包，最后做界面与发布。
