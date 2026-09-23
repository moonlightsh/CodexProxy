# CodexHelper 设计：受管网关配置与本地分流代理

状态：已评审（对话确认），待实施计划
日期：2026-09-23
来源：移植自 CodexPlusPlus `claude/windows-managed-gateway-pac` 分支
（`docs/superpowers/specs/2026-09-16-windows-managed-gateway-pac-design.md`、
`2026-09-17-managed-gateway-local-proxy-design.md`）

## 1. 背景与目标

CodexPlusPlus 已实现“Windows 受管网关”：固定模型网关、OpenAI 流量经 SOCKS5、其余直连。
但它依附于 Codex++ 的启动器、CDP 注入、helper 服务等大量无关功能。

CodexHelper 只保留其中最小可用的部分，做成一个独立的单窗口托盘工具：

- 录入网关 API Key，存入 Windows Credential Manager。
- 修改 `~/.codex/config.toml`，让 Codex 使用受管网关 provider。
- 修改 `~/.codex/.env`，让 Codex 引擎把出站流量交给本地代理。
- 在 `127.0.0.1:17891` 常驻一个本地分流代理：命中 OpenAI 规则走上游 SOCKS5，其余由本机直连。

### 1.1 固定参数（全部写死，不提供界面修改）

| 项 | 值 |
| --- | --- |
| 模型网关 | `http://10.20.30.61:8080`（`wire_api = "responses"`） |
| 上游 SOCKS5 | `10.20.30.61:7891`（无认证） |
| 本地代理端口 | `17891`；仅允许环境变量 `CODEX_HELPER_PROXY_PORT` 应急覆盖，界面不暴露 |
| OpenAI 规则 | BlackMatrix7 OpenAI Clash 规则 2025-06-06 域名类快照（7 精确 + 24 后缀 + 关键字 `openai`） |
| 凭据 target | `codex-helper/managed-gateway` |
| provider id | `managed_gateway` |

### 1.2 非目标（第一阶段）

- 不做 PAC，不接管 Codex 启动，不注入 `--proxy-pac-url` / `--proxy-server`。
- 不处理 Codex 界面层（`ChatGPT.exe` Chromium）的流量入口，见 §9。
- 不支持 macOS / Linux 正式使用（macOS 仅作开发调试）。
- 不支持修改网关、SOCKS5、端口或规则；不在运行时下载规则。
- 不修改 Windows 系统代理。
- 不做自动更新、诊断包导出、多语言（界面仅中文）。
- 不修改 `~/.codex/auth.json`，ChatGPT 官方登录状态完全保留。

## 2. 总体架构

```text
codex.exe app-server（Rust 引擎，reqwest）
  │  启动时 load_dotenv() 读 ~/.codex/.env
  │    HTTP_PROXY / HTTPS_PROXY = http://127.0.0.1:17891
  │    NO_PROXY = 10.20.30.61,127.0.0.1,localhost
  │  模型请求：provider managed_gateway → http://10.20.30.61:8080
  │    鉴权：auth.command = codex-helper-credential.exe get codex-helper/managed-gateway
  ▼
CodexHelper 本地分流代理 127.0.0.1:17891（分流规则内置于此，唯一判定点）
  ├─ 命中 OpenAI 规则 → SOCKS5 10.20.30.61:7891 建隧道；失败 502，绝不回退直连
  └─ 未命中           → 由本机直接连出（出口仍是本机）
```

`.env` 中的代理变量会被 codex 派生的子进程（git、npm 等）继承；这些流量同样经 17891，
未命中规则即本机直连，结果与原本直连等价。

## 3. 组件划分

```text
codex-helper/                         Cargo workspace
├─ crates/helper-core/                纯逻辑库，跨平台可编译与测试
│  ├─ rules.rs         OpenAI 规则快照、主机规范化、route_for_host
│  ├─ codex_config.rs  config.toml 受管 provider 的校正与还原
│  ├─ codex_env.rs     .env 受管块的写入 / 移除 / 检测旧 Codex++ 块
│  ├─ proxy.rs         17891 分流代理：CONNECT、明文转发、SOCKS5 客户端、自识端点
│  ├─ gateway.rs       Key 校验（GET /v1/models）、TCP 可达性检测
│  ├─ credential.rs    Windows Credential Manager；非 Windows 为内存实现（开发模式）
│  ├─ state.rs         工具自身状态文件
│  ├─ log.rs           脱敏诊断日志
│  └─ manager.rs       启用 / 停用 / 启动对账 / 清理的编排
├─ apps/credential/                   codex-helper-credential.exe（控制台程序）
└─ apps/desktop/                      Tauri 2 应用
   ├─ src-tauri/   薄命令层，全部委托 helper-core::manager；托盘、单实例、自启
   └─ src/         原生 TypeScript + HTML，单页，不引入 React / Tailwind
```

### 3.1 从 CodexPlusPlus 移植的对应关系

| 源 | 目标 | 处理 |
| --- | --- | --- |
| `managed_gateway.rs` 规则与匹配函数 | `rules.rs` | 原样移植；删除 `build_pac_script`、`inject_managed_pac_arg`、PAC 路径与参数函数 |
| `managed_gateway.rs` 配置校正 | `codex_config.rs` | 移植并新增“记录原值 / 还原”；损坏配置改为拒绝写入（见 §7） |
| `managed_gateway.rs` Key 校验与凭据编排 | `gateway.rs`、`manager.rs` | 移植；凭据 target 改名 |
| `managed_env.rs` | `codex_env.rs` | 移植；标记改为 `codex-helper managed gateway`；新增检测旧 Codex++ 块 |
| `managed_proxy.rs` | `proxy.rs` | 移植；自识端点改为 `/__codex_helper_proxy_id`；新增直连连接超时与连接记录回调 |
| `credential.rs` | `credential.rs` | 移植；非 Windows 由报错改为内存实现 |
| `apps/codex-plus-credential` | `apps/credential` | 移植；`clear-managed-env` 改为完整的 `cleanup [--purge-key]` |
| `diagnostic_log` 依赖 | `log.rs` | 重写，写到 `%LOCALAPPDATA%\CodexHelper\logs\` |

原模块的现有单元与集成测试一并移植，只改标识名。

## 4. 数据与文件

### 4.1 工具状态文件

路径：`%LOCALAPPDATA%\CodexHelper\state.json`。不保存 Key。

```json
{
  "enabled": true,
  "previous_model_provider": "custom",
  "previous_model_catalog_json": null,
  "autostart": false
}
```

- `previous_model_provider`：启用前 `model_provider` 的值；不存在则为 `null`。
- `previous_model_catalog_json`：启用时被移除的外部 catalog 指针；无则为 `null`。
- 原值只在“停用 → 启用”的转换时记录，已启用状态下的重复校正不得覆盖。

### 4.2 `~/.codex/config.toml` 受管内容

```toml
model_provider = "managed_gateway"

[model_providers.managed_gateway]
name = "Managed Gateway"
base_url = "http://10.20.30.61:8080"
wire_api = "responses"

[model_providers.managed_gateway.auth]
command = "C:\\Users\\<user>\\AppData\\Local\\Programs\\CodexHelper\\codex-helper-credential.exe"
args = ["get", "codex-helper/managed-gateway"]
```

- `command` 每次校正时改写为当前安装目录下 credential.exe 的规范化绝对路径。
- 校正时移除受管 provider 中的 `env_key`、`experimental_bearer_token`、`requires_openai_auth`
  （与命令鉴权互斥）。
- 若存在根键 `model_catalog_json`：启用前在界面确认，记录原值后移除该指针，不删除外部文件。
- 只增删改上述受管键，其余内容（注释、其他 provider、projects、plugins 等）用 `toml_edit` 原样保留。

### 4.3 `~/.codex/.env` 受管块

```dotenv
# >>> codex-helper managed gateway (自动生成，请勿手改) >>>
HTTP_PROXY=http://127.0.0.1:17891
HTTPS_PROXY=http://127.0.0.1:17891
NO_PROXY=10.20.30.61,127.0.0.1,localhost
# <<< codex-helper managed gateway <<<
```

- 幂等块编辑，只管理两行标记之间的内容，保留用户其他行。
- 块固定在文件末尾（dotenv 后定义生效），覆盖用户自定义的 `HTTPS_PROXY`。
- 移除后文件只剩空白则删除文件。
- 绝不写入任何凭据。
- 检测到 Codex++ 的旧块（`codex-plus-plus managed gateway` 标记）时，界面警告并提供一键移除；
  不自动删除。

### 4.4 凭据

- Windows Credential Manager，`CRED_TYPE_GENERIC`，target `codex-helper/managed-gateway`，
  当前用户作用域。
- `codex-helper-credential.exe get codex-helper/managed-gateway`：成功时 stdout 仅输出 Token；
  失败时 stderr 输出不含凭据的简短原因，退出码非零。
- 只允许读取这一个固定 target，其他参数一律拒绝。

## 5. 核心流程

### 5.1 启用（首次录入 Key 或打开开关）

按顺序执行，任一步失败则撤销已完成的步骤，界面显示原因：

1. **校验 Key**（仅在录入新 Key 时）：trim，空值拒绝；`GET /v1/models` 带 Bearer。
   - 2xx → 通过。
   - 401 / 403 → 拒绝保存，不动已有凭据。
   - 5xx / 超时 / 网络错误 → 提示网关暂时不可用，提供“仍然保存”。
2. **写凭据**：写入 Credential Manager；前端立即清空明文。
3. **记录原值**：仅当状态由停用转为启用时，写 `previous_*` 到状态文件。
4. **校正 config.toml**：备份到 `config.toml.codex-helper-bak`（固定名，覆盖）后原子写入。
5. **启动 17891 并自检**：已是本工具实例则复用；被其他进程占用则失败，不换端口。
6. **写 `.env` 受管块**：原子写入。先启代理再写 `.env`，避免 `.env` 指向无人监听的端口。
7. **状态检测**：网关 8080、SOCKS5 7891 TCP 可达性，仅用于状态灯，不阻断启用。

### 5.2 停用

1. 移除 `.env` 受管块。
2. 还原 `model_provider`、`model_catalog_json` 为 `previous_*`（原为 `null` 则删除该键）；
   删除 `[model_providers.managed_gateway]` 整节。
3. 停止 17891。
4. 更新状态文件 `enabled = false`，清空 `previous_*`。

凭据默认保留；界面另有“清除 Key”按钮删除凭据。

还原只针对受管键做定点修改，不以备份整文件覆盖，保证用户在启用期间对其他配置的修改不丢失。

### 5.3 工具启动对账（开机自启或手动打开）

- `enabled = true`：
  - 凭据存在 → 幂等重做 §5.1 的第 4–6 步（修复被改动的配置、安装目录变化后的路径、被删的 `.env`）。
  - 凭据缺失 → 不启动代理，弹出窗口要求录入 Key。此时 Codex 请求会失败（fail-closed，有意为之），
    界面说明原因。
- `enabled = false`：检查 `.env` 与 `config.toml` 是否残留受管内容（例如上次停用中途崩溃），
  发现后提示一键清理。

### 5.4 清理（卸载时）

`codex-helper-credential.exe cleanup [--purge-key]`：

- 执行 §5.2 的第 1、2、4 步（代理随主程序退出已停止）。
- 状态文件缺失时：仍移除 `.env` 受管块；`config.toml` 中若 `model_provider = "managed_gateway"`
  则删除该键，并删除受管 provider 节。
- `--purge-key` 时同时删除凭据。
- 文件或块不存在都视为成功。

## 6. 本地分流代理

- 仅绑定 `127.0.0.1`，不做鉴权。
- 支持两种代理语义，二者缺一不可：
  - `CONNECT host:port`：建隧道，不解密 TLS。
  - 绝对 URI 明文请求（`GET http://host:port/path`）：改写为 origin-form 后转发。
    网关是明文 HTTP，实测其请求会经过代理，只做 CONNECT 会断模型调用。
- 请求头上限 32 KiB，超出或首行无法解析返回 400。
- 分流：`route_for_host(host)`；网关 IP 与回环地址恒为直连。
- SOCKS5：无认证协商，CONNECT 使用域名寻址（ATYP=0x03），不在本地解析 DNS；
  上游连接与握手各 3 秒超时；失败返回 `502 Bad Gateway` 并关闭连接，**绝不回退直连**；不重试。
- 直连：连接超时 10 秒，失败返回 502。
- 自识端点：`GET /__codex_helper_proxy_id` 返回固定标识，用于端口复用判定。
- 端口：默认 17891；`CODEX_HELPER_PROXY_PORT` 可覆盖；非法值回落默认。
- 连接记录：每条连接结束后产生 `{time, host, port, decision, ok, ms}`，
  写入内存环形缓冲（50 条）供界面展示，并按采样写入日志。

## 7. 异常处理

| 场景 | 行为 |
| --- | --- |
| 17891 被其他进程占用 | 启用失败，界面显示端口冲突；不写 `.env`，回滚已改的 `config.toml` |
| `config.toml` 解析失败 | 中止，不写入，提示用户修复（与原代码“视为空配置覆盖”不同） |
| `model_providers` 或受管节不是 table | 中止，不写入，提示原因 |
| `.env` 写入失败 | 回滚 `config.toml`，停止代理 |
| 网关或 SOCKS5 不可达 | 对应状态灯变红，不阻断；每 30 秒重测 |
| 录入 Key 返回 401 / 403 | 拒绝保存，已有凭据不动 |
| 录入 Key 返回 5xx / 超时 | 提示网关暂时不可用，提供“仍然保存” |
| 运行中 SOCKS5 中断 | 命中规则的请求返回 502，不回退直连 |
| 工具退出而 Codex 仍在运行 | Codex 请求失败（fail-closed）；退出前若检测到 `codex.exe` 在运行则弹窗确认 |
| 用户不经工具启动 Codex 且工具未运行 | 同上；界面文案说明“受管模式需要本工具保持运行” |
| 凭据缺失（已启用） | 不启动代理，弹窗要求录入 |
| `.env` 存在 Codex++ 旧块 | 警告并提供一键移除 |

所有文件写入均为“临时文件 + 原子替换”。

## 8. 界面

单窗口，关闭按钮隐藏到托盘；托盘“退出”才真正结束进程。

```text
┌ CodexHelper ─────────────────────────────┐
│ 受管模式  [■ 已启用]                       │
│ API Key   已配置   [重新录入] [清除 Key]    │
│ ───────────────────────────────────────  │
│ ● 本地代理  127.0.0.1:17891 运行中          │
│ ● 模型网关  10.20.30.61:8080 可达           │
│ ● SOCKS5   10.20.30.61:7891 可达           │
│ ● Codex 配置 config.toml / .env 已接管      │
│ ───────────────────────────────────────  │
│ 最近连接（内存保留 50 条）                   │
│  chatgpt.com:443      SOCKS5  ✓  120ms    │
│  github.com:443       直连    ✓   40ms    │
│ ───────────────────────────────────────  │
│ [□ 开机自启]    提示：修改后请重启 Codex     │
└──────────────────────────────────────────┘
```

- 首次运行（无凭据）只显示 Key 录入区与说明。
- 不回显已保存的 Key，只显示“已配置 / 未配置”。
- 存在 `model_catalog_json` 时，启用前弹出确认说明。
- 托盘菜单：显示窗口 / 受管模式开关 / 退出。
- 单实例：重复启动时唤起已有窗口。
- 开机自启：写当前用户 Run 注册表项（`tauri-plugin-autostart`），带 `--minimized` 仅驻留托盘。
- 非 Windows 平台顶部显示“开发模式”横幅。

## 9. 界面层流量（明确留待后续）

`ChatGPT.exe`（Chromium 界面层：登录页、webview、遥测）不读取 `.env`，第一阶段不为其提供进入
17891 的入口，它按系统代理设置（通常为直连）访问网络。

实机验收时观察官方登录、插件页、对话是否正常，并通过“最近连接”确认相关请求来源。
若界面层存在依赖 OpenAI 的失败功能，后续优先评估“系统 PAC 只导流到 17891”
（OpenAI 规则 → `PROXY 127.0.0.1:17891`，其余 `DIRECT`，规则由同一份 Rust 常量生成），
不采用“系统代理整体指向 17891”（工具退出会导致全机断网）。

## 10. 安全与隐私

- Key 只存在于 Credential Manager；不写入 `config.toml`、`.env`、`auth.json`、状态文件或日志。
- Key 校验请求不记录 Authorization 头；错误对象与事件不携带 Key。
- 代理日志只记录 `{host, port, decision, ok, ms}`，不记录请求头、body、URL 查询参数。
- 代理仅绑定回环地址（本机任意进程可用，与常见本地代理工具同类风险，已知并接受）。

## 11. 测试

| 层 | 内容 | 运行环境 |
| --- | --- | --- |
| 纯函数 | 规则匹配、请求行解析、SOCKS5 报文、`.env` 块、`config.toml` 校正与还原、状态文件 | macOS `cargo test` + CI |
| 集成 | 假 SOCKS5 + 假目标服务器：命中隧道、命中但上游失败 502 且目标**未收到直连**、未命中直连、端口冲突；wiremock 网关：Key 校验 200/401/5xx/超时 | macOS + CI |
| 编排 | 临时 `CODEX_HOME` 下启用→停用：`config.toml` 逐字节还原且保留启用期间的无关修改；中途失败回滚；启动对账修复漂移；损坏 `config.toml` 拒写；`cleanup` 在有无状态文件下的行为 | macOS + CI（可替换凭据实现） |
| Windows 专属 | Credential Manager 读写覆盖删除；credential.exe `get` 的 stdout / stderr 约定 | CI `windows-latest` |
| 前端 | `tsc --noEmit` | CI |

## 12. 构建与发布

- `ci.yml`：push / PR 触发；`windows-latest` 与 `macos-latest` 上运行
  `cargo fmt --check`、`cargo clippy --workspace -- -D warnings`、`cargo test --workspace`、前端 `tsc`。
- `release.yml`：`v*` tag 触发；`windows-latest` 上 `tauri build` 生成 NSIS 安装包并上传 GitHub Release。
- NSIS：`installMode: currentUser`，安装到 `%LOCALAPPDATA%\Programs\CodexHelper`，无需管理员。
- `codex-helper-credential.exe` 作为 Tauri `externalBin` 与主程序同目录发布。
- 卸载：`installerHooks` 的 `NSIS_HOOK_PREUNINSTALL` 中调用 `codex-helper-credential.exe cleanup`，
  随后弹窗询问“是否同时清除 API Key？”，选“是”追加 `--purge-key`。

## 13. Windows 实机验收

1. 全新安装，录入 Key 并启用；`config.toml` 与 `.env` 内容符合 §4；全盘搜索不到 Key 明文。
2. 至少两个 Codex 内置模型完成真实对话。
3. “最近连接”中 OpenAI 域名走 SOCKS5、`github.com` 直连、网关不出现（`NO_PROXY` 生效）或为直连。
4. 界面层观察：官方登录、插件页、对话是否正常，记录结论（§9 的决策依据）。
5. 断开 SOCKS5：OpenAI 请求得到 502，无直连记录。
6. 退出工具：Codex 请求失败；重新打开工具后恢复。
7. 停用：`config.toml` 与启用前逐项一致，`.env` 受管块已移除。
8. 开启自启并重启：工具仅驻留托盘，代理可用。
9. 占用 17891 后启用：失败且未写 `.env`。
10. 卸载两种选择（保留 Key / 清除 Key）结果符合预期。

## 14. 兼容性关注点

- Codex 升级后确认：`arg0::load_dotenv()` 仍读取 `CODEX_HOME/.env`；`auth.command` 命令鉴权格式未变；
  引擎仍尊重代理环境变量。
- 官方功能新增域名时，更新规则快照需经代码评审与测试，随版本发布。
