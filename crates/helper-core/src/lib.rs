//! CodexHelper 核心逻辑库。
//!
//! 设计文档：`docs/superpowers/specs/2026-09-23-codex-helper-design.md`
//! 实施计划：`docs/superpowers/plans/2026-09-23-codex-helper-implementation.md`
//!
//! 纯逻辑、跨平台可编译与测试；Windows 专属行为（Credential Manager）以 `cfg(windows)` 隔离，
//! 其他平台提供开发模式实现。

pub mod codex_config;
pub mod codex_env;
pub mod consts;
pub mod credential;
pub mod fsutil;
pub mod gateway;
pub mod log;
pub mod manager;
pub mod paths;
pub mod proxy;
pub mod rules;
pub mod state;
pub mod types;
