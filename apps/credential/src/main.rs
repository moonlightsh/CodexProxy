//! `codex-helper-credential`：供 Codex 命令鉴权调用的凭据读取器，兼作卸载清理入口（设计 §4.4、§5.4）。
//!
//! - `get codex-helper/managed-gateway`：成功时 stdout 仅输出 Token；失败时 stderr 输出不含凭据的
//!   简短原因，退出码非零。只允许这一个固定 target。
//! - `cleanup [--purge-key]`：卸载时撤销受管配置，可选同时删除凭据。

fn main() {
    // 阶段 2 任务 2.2 实现。
    eprintln!(
        "usage: codex-helper-credential get {} | codex-helper-credential cleanup [--purge-key]",
        helper_core::consts::CREDENTIAL_TARGET
    );
    std::process::exit(2);
}
