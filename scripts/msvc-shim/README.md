# 最小 MSVC 头 shim（仅用于 macOS 上的 Windows 目标 cargo check）

`scripts/check-windows.sh` 在 macOS 上对 `x86_64-pc-windows-msvc` 做 `cargo check`。
引入 rustls（ring provider）后，ring 的 C 代码在无 Windows SDK 的机器上缺少
`assert.h`/`stdlib.h`/`string.h`/`stdalign.h`/`intrin.h` 等系统头。

本目录提供这些头的最小声明，全部转发到 clang 内建（`__builtin_*`），不提供任何
运行时实现。脚本用 `-idirafter` 把本目录追加到搜索路径末尾，仅当系统头缺失时兜底：

- 正式 Windows 构建在 Windows/CI 上进行，有真实 MSVC SDK，本目录不参与；
- 本地 macOS 的原生构建目标为 aarch64-apple-darwin，使用 Apple SDK，不经过本目录；
- `cargo check` 不链接不运行，shim 中无实现体不影响类型检查结论。

安全性说明：这是编译期兜底，不改变任何产品行为；若未来在本机出现真实 MSVC SDK
（如安装 cargo-xwin），`-idirafter` 优先级最低，真实头优先生效。
