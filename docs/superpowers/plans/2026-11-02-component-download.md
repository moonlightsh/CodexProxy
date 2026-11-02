# 组件代理下载实施计划

> 执行方式：使用 superpowers:executing-plans 在当前会话逐项实施；先写测试并确认失败，再实现与验证。

**目标：** 下载已检查到的组件更新，全程经代理，默认保存到系统下载目录，不安装。

**架构：** helper-core 负责下载和文件生命周期，Tauri 提供系统路径与 IPC，前端使用 Channel 显示进度和取消。

**技术栈：** Rust、reqwest 0.12/native-tls、tokio、tempfile、Tauri 2、TypeScript。

**设计：** `docs/superpowers/specs/2026-11-02-component-download-design.md`

## 全局约束

- 仅代理运行中允许下载；任何模式都不回退直连。
- 默认保存至系统下载目录的 CodexHelper 子目录，不覆盖已有文件，不安装、不解压。
- 前端不得指定任意下载地址；每次跳转验证官方 HTTPS 主机与路径。
- 连接 15 秒、读取 60 秒、总时限 2 小时、文件最大 2 GiB。
- 分支使用 claude/ 前缀，提交尾注 Co-Authored-By: Claude Code；不推送。

## 评审重点

1. 代理检查与真正开始下载之间状态改变，连接失败时仍不得直连（任务 1）。
2. 带签名参数的 GitHub 重定向应能通过，但凭据、HTTP 和白名单外主机必须拒绝（任务 1）。
3. 缺 Content-Length、响应截断与磁盘写入失败不能留下看似完整的成品（任务 1）。
4. 取消、完成与重复点击竞态不能清理另一个任务或覆盖原文件（任务 1、2）。
5. 检查结果刷新与延迟进度回调不能覆盖其他组件/已结束任务的状态（任务 2）。

### Task 1: 核心下载与代理分流

**文件：** 新建 `crates/helper-core/src/component_downloads.rs`；修改 `lib.rs`、`rules.rs` 和根 `Cargo.toml`。

**接口：** `DownloadManager::download(request, network, directory, progress)` 返回 `DownloadResult`；`cancel(id)` 取消当前 ID；请求仅含 `id/component/version`，进度仅含 `receivedBytes/totalBytes`。生产 URL 由核心构造。

- [x] 先写平台文件选择、URL 限制与分发域名经 SOCKS5 的失败测试，运行确认失败。
- [x] 实现受控目标解析、代理客户端与分发域名规则。
- [x] 先写真实 HTTP 服务/代理下的文件下载、截断、取消、无覆盖和互斥测试，运行确认失败。
- [x] 实现分块写临时文件、长度与大小校验、取消与进度，成功后无覆盖发布。
- [x] 运行 `cargo test -p helper-core`，核对输出。

### Task 2: 桌面接线与下载 UI

**文件：** 新建 `apps/desktop/src-tauri/src/downloads.rs`、`apps/desktop/src/componentdownload.ts` 及测试；修改 `main.rs`、`api.ts`、`types.ts`、`main.ts`、`componentpanel.ts`、`componentformat.ts`、`styles.css`、`dev-mock.ts`。

**接口：** `download_component(request, onProgress: Channel<DownloadProgress>) -> DownloadResult`；`cancel_component_download(id)`。前端维护每个组件的状态，以任务 ID 屏蔽过期回调。

- [x] 先写下载状态/进度/过期事件测试并确认失败。
- [x] 实现纯函数与 Tauri 命令；系统下载目录由 app.path().download_dir() 取得。
- [x] 接入按钮、进度、取消、完成路径和失败重试；重绘仅更新下载区域，保留原组件结果。
- [x] dev-mock 增加下载进度与取消，生产构建不包含 mock。
- [x] 运行前端单测、类型检查和构建，以及 Tauri 编译检查。

### Task 3: 全量验证、评审与交付

- [x] 执行 `scripts/gate.sh`，记录实际通过和受环境限制项。
- [x] 审查整条分支，修复影响代理约束、文件完整性或取消的缺陷并补回归测试。
- [x] 更新设计状态和此计划完成记录，提交全部相关改动，不推送。

## 执行记录

- 用户已授权内置方案直接推进，无须再次选择执行方式；在既有干净工作区内创建 `claude/component-download` 分支，当前会话实施。
- 核心新增 16 项下载测试，前端新增 5 项；最终门禁 Rust 570 项通过、1 项既有忽略，前端 51 项通过，Windows 目标编译通过。
- 既有规则快照计数随两个精确分发域名从 7 更新为 9；TLS ClientHello 测试接受不同系统 TLS 后端合法的记录版本。
- 浏览器完成开始/取消/重试/成功/失败/无代理禁用验收；独立评审无待修复问题。实机与文件完整性边界见设计“实施结果与保证边界”。
