# CI 与约束检查

状态：已实现（[#34](https://github.com/SoloJiang/lectern-app/pull/34)、[#36](https://github.com/SoloJiang/lectern-app/pull/36)、[#50](https://github.com/SoloJiang/lectern-app/pull/50)）。

对应 issue [#4](https://github.com/SoloJiang/lectern-app/issues/4)。规则以脚本执行，违反时构建立即失败（[技术调研](tech-research.md)「约束检查」）。

## 两个任务

| 任务 | Runner | 内容 | 分开的原因 |
|---|---|---|---|
| Checks | `ubuntu-latest` | 格式、架构规则、cargo-deny、不依赖 GPUI 的 crate 的 clippy 与测试 | 不编译 GPUI，几十秒出结果；agent 改领域逻辑时先看这个任务 |
| macOS | `macos-15`（Apple Silicon） | 整个工作区的 clippy、测试、构建 `lectern` | GPUI 与视图层只能在 macOS 编译 |

两个任务都是必需检查，步骤都调用 `just`（`just check`、`just deny`、`just test --profile ci`、`just doctest`，macOS 上另有 `just bench`），本地和 CI 跑的是同一套命令。`RUSTFLAGS=-D warnings`，clippy 警告即失败。clippy 与测试都带 `--all-features`，可选 feature 后的代码也参与编译和测试。

## 构建配置

- 开发构建：依赖用 `opt-level = 2`（GPUI 在 0 级下慢到不能用），本仓库的 crate 保持 0 级，增量编译快；`debug = "limited"`，`split-debuginfo = "unpacked"`。
- 发布构建：`lto = "thin"`，`codegen-units = 1`。
- `just` 不设置编译器 wrapper，开发者自己配置的 `RUSTC_WRAPPER`、`CARGO_BUILD_RUSTC_WRAPPER` 或 `build.rustc-wrapper` 都照常生效。要用 `sccache` 在多个 worktree 间共享编译结果，在 `~/.cargo/config.toml` 里设置 `build.rustc-wrapper = "sccache"`。
- nextest 不跑 doctest，另有 `just doctest`（`cargo test --doc`），CI 两个任务都跑。
- nextest：默认只完整打印失败的测试；CI 用 `ci` profile，不提前停止。

## 架构规则检查

`scripts/check_architecture.py` 读 `cargo metadata` 的依赖图，检查 AGENTS.md 的架构规则：

1. 每个 crate 声明了 `[package.metadata.lectern] layer`。
2. 依赖自上而下：`app` → `view` → `domain` → `infra`；`kernel` 层可被任何层依赖，自身只依赖 kernel 层；只有 `app` 可以依赖 `tool` 层，且只能是 dev-dependency 或不在 `default` feature 里的可选依赖。这一项读取 Cargo 按默认 feature 解析出的依赖图，保证 tool crate 不进入发布构建。
3. `domain`、`infra`、`kernel` 层的依赖图（含间接依赖）里没有 GPUI。`cargo metadata` 会在整个工作区统一 feature，所以只经过间接依赖的 GPUI 路径会再用 `cargo tree -p <crate>` 单独解析确认，避免别的 crate 打开的 feature 造成误报。
4. macOS 系统绑定只能是 `platform` 的直接依赖。绑定按两种方式识别。一是名字：前缀 objc*、cocoa*、core-foundation*、security-framework*、metal*、dispatch*、screencapturekit* 等，或名字里按 - 和 _ 切出的词含 macos、darwin、appkit、metal 等。二是 target 条件只在 Apple 平台成立的依赖，不论名字。脚本用 `rustc --print cfg` 取得 rustc 已知的全部 target 的真实 cfg（按 rustc 版本缓存在 `target/` 下），条件在某个 Apple target 上成立、在其他 target 上都不成立，即为 Apple 专属。与 Cargo 一致，target 不设置的 cfg（`feature`、`test`、`proc_macro` 等）一律为假。target 按默认配置取，不应用本机的 rustflags，因此同一个提交在任何机器和 CI 上结果一致。工作区内的 crate 也按名字检查。`scripts/test_check_architecture.py` 是这部分的单元测试，CI 先跑它。
5. 每个 crate 的 `Cargo.toml` 写有 `[lints] workspace = true`，继承工作区的 lint 配置（见「Lint」）。

每条报错末尾的方括号写明规则出处（AGENTS.md 的规则编号或设计文档），agent 可以直接找到规则原文。

检查用 `cargo metadata --all-features`，可选 feature 引入的依赖也计入。每个 crate 自己的 dev-dependencies 也检查（它们会编译进测试和基准）；依赖项的 dev-dependencies 不会被编译，不检查。

`--gpui-free-packages` 输出在 Linux 上不依赖 GPUI 的 crate 列表（按 `x86_64-unknown-linux-gnu` 过滤依赖图，只为 macOS 声明的 GPUI 依赖不会把 crate 排除在外；含 crate 自身的 dev-dependencies，测试也不编译 GPUI），Linux 任务用它决定 clippy 与测试的范围，新增 crate 时无需修改 CI。

本地运行：

```sh
python3 scripts/check_architecture.py
```

规则 7（插件之间不直接依赖）在 kernel 定义出插件与接口 crate 后加入检查（[#8](https://github.com/SoloJiang/lectern-app/issues/8)）。

## Lint

工作区的 `[workspace.lints.clippy]` 在 clippy 默认规则之外增加三条，`-D warnings` 下都是错误：

- `allow_attributes_without_reason`：`#[allow]` 和 `#[expect]` 必须写 `reason = "..."`，说明为什么关掉这条规则。
- `dbg_macro`：提交的代码里不留 `dbg!`。
- `undocumented_unsafe_blocks`：每个 `unsafe` 块前用 `// SAFETY:` 注释说明为什么安全。

今后用 `clippy.toml` 的 `disallowed-methods`、`disallowed-types` 禁用某个 API 时，`reason` 写明对应的架构规则或 ADR，例如 `reason = "视图层不做 IO（AGENTS.md 架构规则 4）"`。

## 文档链接检查

tool crate `lectern-doc-check`（`cargo run -p lectern-doc-check`）检查仓库里所有受 git 跟踪的 Markdown 文件（`.md`、`.markdown`，不分大小写）的相对链接：目标文件必须存在且在仓库之内，`#锚点` 必须对应目标文件里的某个标题（按 GitHub 生成锚点的规则计算，区分大小写）。先用 pulldown-cmark 按 CommonMark 把整篇文档渲染成 HTML，再用 html5gum 按 HTML 标准对渲染结果整体分词，所以引用式链接、容器里的标题、代码块与代码片段、HTML 注释、原始 HTML 的 `href` / `src` 和 `id` 锚点、`script` 等原始文本元素都按浏览器看到的结果处理。不靠正则逐条适配（依赖选型见 [ADR 0021](../adr/0021.md)）。目标必须受 git 跟踪，符号链接按解析后的路径再检查一次。外部 URL 不检查，查询参数在查找文件前去掉。设计文档、ADR、AGENTS.md 之间的链接改名或删节后失效，这一步会让 `just check` 失败。单元测试在 crate 里，随 `just test` 运行。

## 许可证与来源

`deny.toml`（`all-features = true` 与 `include-dev = true`，可选依赖和 dev-dependencies 也检查；目标平台含 macOS 与 Linux CI；本仓库自己的 crate 也检查许可证）：只允许宽松许可证（MIT、Apache-2.0、BSD、ISC、Zlib、Unicode 等）和文件级弱 copyleft 的 MPL-2.0；GPL、LGPL、AGPL 不在允许列表里，出现即失败。依赖只能来自 crates.io，不允许未知的 git 源。`[bans]` 拒绝嵌入式运行时（V8 / Deno、Node 绑定、JNI、wasmtime / wasmer / wasmi 等），对应架构规则 3。安全公告由 `cargo deny check advisories` 检查：漏洞和 unsound 公告对整个依赖图生效，unmaintained 只对直接依赖生效。

## Metal

GPUI 在构建时编译 Metal shader。Xcode 26 起 Metal 工具链需单独下载，macOS 任务在缺失时执行 `xcodebuild -downloadComponent MetalToolchain`。

## 实际验证

[#50](https://github.com/SoloJiang/lectern-app/pull/50) 加入的 lint 继承检查、规则出处和文档链接检查：

- 在 Linux 上运行 `just check` 通过。
- 临时加入不写理由的 `#[allow(dead_code)]` 和 `dbg!` 时，clippy 报两个错误。
- 删掉某个 crate 的 `[lints] workspace = true` 时，架构检查失败，报错指向本文件。
- `lectern-doc-check` 的单元测试覆盖行内与引用式链接、带括号和尖括号的路径、查询参数、百分号编码、仓库外的路径、引用块和列表里的标题、setext 标题、缩进与围栏代码块、跨行代码片段、HTML 注释、脚注、重复标题的锚点和锚点大小写；在 README 里临时加一条指向不存在锚点的链接时报错；在 main 和当时打开的各个 PR 分支上都通过。
- macOS 上的 `just check` 由 CI 的 macOS 任务运行。

## 与设计的差异

[#34](https://github.com/SoloJiang/lectern-app/pull/34) 与 [#36](https://github.com/SoloJiang/lectern-app/pull/36) 的实现没有记录差异。[#50](https://github.com/SoloJiang/lectern-app/pull/50) 新增的规则 5、Lint、文档链接检查三处在本文对应小节里直接写成设计。文档链接检查最初是 Python 脚本，用正则识别 Markdown；审阅中陆续发现引用式链接、容器、代码片段等写法无法逐条覆盖，改为基于 CommonMark 解析器的 tool crate。
