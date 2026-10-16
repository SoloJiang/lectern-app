# CI 与约束检查

对应 issue [#4](https://github.com/SoloJiang/lectern-app/issues/4)。规则以脚本执行，违反时构建立即失败（[技术调研](tech-research.md)「约束检查」）。

## 两个任务

| 任务 | Runner | 内容 | 为什么分开 |
|---|---|---|---|
| Checks | `ubuntu-latest` | 格式、架构规则、cargo-deny、不依赖 GPUI 的 crate 的 clippy 与测试 | 快：不编译 GPUI，几十秒出结果，agent 改领域逻辑时先看这个 |
| macOS | `macos-15`（Apple Silicon） | 整个工作区的 clippy、测试、构建 `lectern` | GPUI 与视图层只能在 macOS 编译 |

两个任务都是必需检查，步骤都调用 `just`（`just check`、`just deny`、`just test --profile ci`、`just doctest`，macOS 上另有 `just bench`），本地和 CI 跑的是同一套命令。`RUSTFLAGS=-D warnings`，clippy 警告即失败。clippy 与测试都带 `--all-features`，feature 后面的代码也编译和测试。

## 构建配置

- 开发构建：依赖用 `opt-level = 2`（GPUI 在 0 级下慢到不能用），本仓库的 crate 保持 0 级，增量编译快；`debug = "limited"`，`split-debuginfo = "unpacked"`。
- 发布构建：`lto = "thin"`，`codegen-units = 1`。
- `just` 不设置编译器 wrapper，开发者自己配置的 `RUSTC_WRAPPER`、`CARGO_BUILD_RUSTC_WRAPPER` 或 `build.rustc-wrapper` 都照常生效。要用 `sccache` 在多个 worktree 间共享编译结果，在 `~/.cargo/config.toml` 里设置 `build.rustc-wrapper = "sccache"`。
- nextest 不跑 doctest，另有 `just doctest`（`cargo test --doc`），CI 两个任务都跑。
- nextest：默认只完整打印失败的测试；CI 用 `ci` profile，不提前停止。

## 架构规则检查

`scripts/check_architecture.py` 读 `cargo metadata` 的依赖图，检查 AGENTS.md 的架构规则：

1. 每个 crate 声明了 `[package.metadata.lectern] layer`。
2. 依赖自上而下：`app` → `view` → `domain` → `infra`；`kernel` 层可被任何层依赖，自身只依赖 kernel 层；只有 `app` 可以依赖 `tool` 层，而且只能是 dev-dependency 或可选（feature 控制）的依赖，tool crate 不进入发布构建。
3. `domain`、`infra`、`kernel` 层的依赖图（含间接依赖）里没有 GPUI。
4. macOS 系统绑定只能是 `platform` 的直接依赖。绑定按两种方式识别：名字（前缀 objc*、cocoa*、core-foundation*、security-framework*、metal*、dispatch*、screencapturekit* 等，或名字里按 - 和 _ 切出的词含 macos、darwin、appkit、metal 等），以及 target 条件只在 Apple 平台成立的依赖，不论名字：脚本用 `rustc --print cfg` 取得 rustc 已知的全部 target 的真实 cfg（按 rustc 版本缓存在 `target/` 下），条件在某个 Apple target 上成立、在其他 target 上都不成立即为 Apple 专属；与 Cargo 一致，target 不设置的 cfg（`feature`、`test`、`proc_macro` 等）一律为假；Cargo 配置的 rustflags（环境变量或 `.cargo/config.toml`）会一并传给 rustc，并计入缓存键。工作区内的 crate 也按名字检查。`scripts/test_check_architecture.py` 是这部分的单元测试，CI 先跑它。

检查用 `cargo metadata --all-features`，藏在可选 feature 后面的依赖也算在内。每个 crate 自己的 dev-dependencies 也检查（会编译进测试和基准），依赖的 dev-dependencies 不会被编译，跳过。

`--gpui-free-packages` 输出在 Linux 上不依赖 GPUI 的 crate 列表（按 `x86_64-unknown-linux-gnu` 过滤依赖图，只为 macOS 声明的 GPUI 依赖不会把 crate 排除在外；含 crate 自身的 dev-dependencies，测试也不编译 GPUI），Linux 任务用它决定 clippy 与测试的范围，新增 crate 不用改 CI。

本地运行：

```sh
python3 scripts/check_architecture.py
```

规则 7（插件之间不直接依赖）要等 kernel 定义出插件与接口 crate 后再加入检查（[#8](https://github.com/SoloJiang/lectern-app/issues/8)）。

## 许可证与来源

`deny.toml`（`all-features = true` 与 `include-dev = true`，可选依赖和 dev-dependencies 也检查；目标平台含 macOS 与 Linux CI；本仓库自己的 crate 也检查许可证）：只允许宽松许可证（MIT、Apache-2.0、BSD、ISC、Zlib、Unicode 等）和文件级弱 copyleft 的 MPL-2.0；GPL、LGPL、AGPL 不在允许列表里，出现即失败。依赖只能来自 crates.io，不允许未知的 git 源。`[bans]` 拒绝嵌入式运行时（V8 / Deno、Node 绑定、JNI、wasmtime / wasmer / wasmi 等），对应架构规则 3。安全公告由 `cargo deny check advisories` 检查：漏洞和 unsound 公告对整个依赖图生效，unmaintained 只对直接依赖生效。

## Metal

GPUI 在构建时编译 Metal shader。Xcode 26 起 Metal 工具链需单独下载，macOS 任务在缺失时执行 `xcodebuild -downloadComponent MetalToolchain`。
