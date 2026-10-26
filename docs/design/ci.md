# CI 与约束检查

状态：已实现（[#34](https://github.com/SoloJiang/lectern-app/pull/34)、[#36](https://github.com/SoloJiang/lectern-app/pull/36)、[#50](https://github.com/SoloJiang/lectern-app/pull/50)）。macOS 的基准拆成单独的任务（[ADR 0036](../adr/0036.md)），见「任务」「实际验证」。打印 lint 与架构规则检查第 8 条由 [#195](https://github.com/SoloJiang/lectern-app/pull/195) 加入（[logging 设计「对其他 crate 的要求」](logging.md#对其他-crate-的要求)第 10 项，验证见 [logging 设计「实际验证」](logging.md#实际验证)）。判定任务 `Judge (physical Mac)` 和它的自托管 runner 由 [ADR 0052](../adr/0052.md) 加入，见「判定机器」；runner 还没有注册，任务现在跳过。

对应 issue [#4](https://github.com/SoloJiang/lectern-app/issues/4)。规则以脚本执行，违反时构建立即失败（[技术调研](tech-research.md)「约束检查」）。

## 任务

| 任务（检查名） | Runner | 内容 | 分开的原因 |
|---|---|---|---|
| `Checks (Linux, GPUI-free crates)` | `ubuntu-latest` | 格式、架构规则、cargo-deny、不依赖 GPUI 的 crate 的 clippy 与测试、基准的冒烟运行 | 不编译 GPUI，约 2 分钟出结果；agent 改领域逻辑时先看这个任务 |
| `Build and test (macOS arm64)` | `macos-15`（Apple Silicon） | 整个工作区的 clippy、测试、doctest、构建 `lectern` | GPUI 与视图层只能在 macOS 编译 |
| `Benchmarks (macOS arm64)` | `macos-15`（Apple Silicon） | `just bench`：判定性能预算，上传 `report.json`；测量的时间止于首帧的条目只记录 | 与上一个任务并行；基准用 release 构建，与测试的 debug 构建不共用产物（[ADR 0036](../adr/0036.md)） |
| `Judge (physical Mac)` | `[self-hosted, macOS, ARM64, lectern-judge]`（自托管的实体 M 系列 Mac） | `LECTERN_BENCH_JUDGE=1 just bench baseline/ app/cold_start`：判定测量的时间止于首帧的预算，上传 `report.json` | 托管 runner 不给 `presentedTime`（[ADR 0052](../adr/0052.md)）；只在仓库变量 `LECTERN_JUDGE_RUNNER` 为 `true` 时运行，见「判定机器」 |

前三个任务是必需检查，`Judge (physical Mac)` 在 runner 注册、第一次运行通过之后加入（「判定机器」）。步骤都调用 `just`（`just check`、`just deny`、`just test --profile ci`、`just doctest`、`just bench`），本地和 CI 跑的是同一套命令。`just bench` 在 Linux 上是冒烟运行，在 macOS 上判定预算，之后上传 `target/lectern-bench/report.json`（[bench 设计「`just bench` 与 CI」](bench.md#just-bench-与-ci)）。

两个 macOS 任务各用一个 `Swatinem/rust-cache` 的缓存键（`test`、`bench`）：缓存按任务和键区分，debug 与 release 的产物不互相挤占。基准原来是 macOS 任务的最后一步，任务的墙钟时间是测试与基准之和（#129 之后约 13 分钟，其中 setup、check、测试和 doctest 约 4 分钟）；拆开之后取两者中较长的一个。目标：典型 PR 的 macOS 墙钟时间 ≤ 15 分钟，基准任务 ≤ 12 分钟。

`Benchmarks (macOS arm64)` 另用 `actions/cache` 保存和还原 `target/lectern-bench/fixtures`（跨运行保留的基准夹具，现在是 `code_intel` 的 10 万文件虚拟仓库，约 330 MB）。键是生成它的源码的哈希（`crates/code_intel/src`、`crates/cache/src`、`crates/bench/benches/fixtures` 和 `Cargo.lock`）；这个键只决定命中率。正确性靠夹具自己：还原的夹具在使用前对照当前的写入逻辑核对单元数和抽样单元，不符就重写（[ADR 0039](../adr/0039.md)，[bench 设计「夹具与 `test-support`」](bench.md#夹具与-test-support)）。源码哈希是另一层防护：`code_intel` 的源码一改就不命中，连核对看不出的、只影响样本之外文件的改动也会重写。键没变时还原约十几秒，代替约一分钟的写入。每个基准的耗时写进 `report.json`，最慢的 10 个是 check run 的一条 notice（[bench 设计「报告」](bench.md#报告)），超过目标时据此定位。

检查名就是 GitHub 分支保护里必需检查的名字，改名要同时改分支保护。`Benchmarks (macOS arm64)` 由 ADR 0036 加入，要由仓库管理员加进必需检查；在那之前，基准超标只让这个任务失败，不阻止合并。`RUSTFLAGS=-D warnings`，clippy 警告即失败。clippy 与测试都带 `--all-features`，可选 feature 后的代码也参与编译和测试。

## 判定机器

[ADR 0052](../adr/0052.md) 决策第 3 条的判定机器：一台实体 M 系列 Mac，注册为本仓库的自托管 runner。`Judge (physical Mac)` 只在它上面运行，判定测量的时间止于首帧的预算（[bench 设计「平台与噪声」](bench.md#平台与噪声)）。仓库是公开的，任务按 ADR 0052 决策第 4 条写，`ci.yml` 里逐条对应：

- 条件：仓库变量 `LECTERN_JUDGE_RUNNER` 为 `true`，并且是 push 到 `main`，或 `head.repo.full_name` 等于 `github.repository` 的 `pull_request`。没有 `pull_request_target`。
- `runs-on: [self-hosted, macOS, ARM64, lectern-judge]`，`timeout-minutes: 60`。
- `permissions: contents: read`，不引用 secret；checkout 带 `persist-credentials: false` 和 `clean: true`（开始时 `git clean -ffdx`）；最后一步（`if: always()`）结束遗留的 `empty_window`、`lectern` 进程，再清一次工作区。没有 `rust-cache`，构建产物不跨任务保留，每次从头构建 release。
- 环境变量 `LECTERN_BENCH_JUDGE=1`，运行 `just bench baseline/ app/cold_start`（过滤按子串匹配，后者也选中 `app/cold_start_purged`），上传 `report.json`（`lectern-bench-judge-report`）。

准备机器（产品负责人或仓库管理员，按顺序；bench 第 4 步合并之后才做第 8 步）：

1. 仓库 Settings → Actions → General，「Approval for running fork pull request workflows from contributors」选「Require approval for all external contributors」，保存。之后批准 fork PR 的运行之前先看它的 diff，改了 `.github/` 的不批准。
2. 机器：实体 M 系列 Mac，接显示器或 HDMI 假负载。最好接在不能访问其他内网主机的网络里。
3. 新建一个专用的标准用户（不是管理员），不登录 Apple ID，不放 SSH 密钥、git 凭据和别的凭据。
4. 电源（管理员终端）：`sudo pmset -a sleep 0 displaysleep 0 disksleep 0`。系统设置 → 锁定屏幕：「不活跃时启动屏幕保护程序」和「屏幕保护程序启动或显示器关闭后需要密码」都选「永不」（以专用用户设置）。
5. 登录：先在系统设置 → 隐私与安全性里关闭 FileVault，再在系统设置 → 用户与群组里把「自动以此身份登录」设为专用用户。不关 FileVault 时，开机后手动登录专用用户，一直保持登录。
6. 工具：安装 Xcode，运行一次 `xcodebuild -downloadComponent MetalToolchain`（可能要管理员）；以专用用户按 rustup.rs 安装 rustup。`just` 和仓库要的工具链由任务安装。
7. 注册 runner：以专用用户在它的图形会话里打开终端，按仓库 Settings → Actions → Runners → New self-hosted runner（macOS、ARM64）页面下载并解压 runner；运行 `./config.sh --url https://github.com/SoloJiang/lectern-app --token <页面上的令牌> --labels lectern-judge --unattended`，再运行 `./svc.sh install` 和 `./svc.sh start`。`svc.sh` 装的是专用用户的 LaunchAgent，随这个用户登录启动；不改成 LaunchDaemon（没有图形会话，窗口不显示）。Runners 页上这台 runner 的标签应是 `self-hosted`、`macOS`、`ARM64`、`lectern-judge`（前三个是默认标签）。
8. 检查（以专用用户，在它的图形会话里，在本仓库 `main` 的一份检出里）：`cargo run --release -p lectern-platform --features bench-hooks --example frame_hooks` 退出码 0，输出里有 `first_presented_source presented`；`LECTERN_BENCH_JUDGE=1 just bench baseline/` 退出码 0，冷启动的说明里是 `source presented 20, handler 0`。任何一项是 `handler` 时不做下一步，交给产品负责人决定（ADR 0052「已知限制」）。
9. 设置仓库变量：`gh variable set LECTERN_JUDGE_RUNNER --body true --repo SoloJiang/lectern-app`（或 Settings → Secrets and variables → Actions → Variables）。
10. 第一次 `Judge (physical Mac)` 通过之后，在分支保护里把它加进必需检查。

运行中：机器离线时任务排队，加进必需检查之后 PR 不能合并；把变量改为 `false` 后任务跳过，GitHub 把跳过的任务算作成功，这期间冷启动没有判定。fork 的 PR 总是跳过这个任务，合并之后由 `main` 上的 push 判定。

## 构建配置

- 开发构建：依赖用 `opt-level = 2`（GPUI 在 0 级下慢到不能用），本仓库的 crate 保持 0 级，增量编译快；`debug = "limited"`，`split-debuginfo = "unpacked"`。
- 发布构建：`lto = "thin"`，`codegen-units = 1`。
- `just` 不设置编译器 wrapper，开发者自己配置的 `RUSTC_WRAPPER`、`CARGO_BUILD_RUSTC_WRAPPER` 或 `build.rustc-wrapper` 都照常生效。要用 `sccache` 在多个 worktree 间共享编译结果，在 `~/.cargo/config.toml` 里设置 `build.rustc-wrapper = "sccache"`。
- nextest 不跑 doctest，另有 `just doctest`（`cargo test --doc`），Linux 任务和 `Build and test (macOS arm64)` 都跑。
- nextest：默认只完整打印失败的测试；CI 用 `ci` profile，不提前停止，并写出 JUnit 报告（`target/nextest/ci/junit.xml`）。测试失败时，`scripts/ci_annotate_failures.py` 把每个失败的测试和它的输出写成 GitHub 注释（GitHub 每个步骤最多保留 10 条；失败超过 10 个时前 9 个各一条，其余的测试名合成第 10 条），不下载任务日志也能通过 check run 的接口看到失败原因。

## 架构规则检查

`scripts/check_architecture.py` 读 `cargo metadata` 的依赖图，检查 AGENTS.md 的架构规则：

1. 每个 crate 声明了 `[package.metadata.lectern] layer`。
2. 依赖自上而下：`app` → `view` → `domain` → `infra`；`kernel` 层可被任何层依赖，自身只依赖 kernel 层；只有 `app` 可以依赖 `tool` 层，且只能是 dev-dependency 或不在 `default` feature 里的可选依赖。这一项读取 Cargo 按默认 feature 解析出的依赖图，保证 tool crate 不进入发布构建。
3. `domain`、`infra`、`kernel` 层的依赖图（含间接依赖）里没有 GPUI。`cargo metadata` 会在整个工作区统一 feature，所以只经过间接依赖的 GPUI 路径会再用 `cargo tree -p <crate>` 单独解析确认，避免别的 crate 打开的 feature 造成误报。
4. macOS 系统绑定只能是 `platform` 的直接依赖。绑定按两种方式识别。一是名字：前缀 objc*、cocoa*、core-foundation*、security-framework*、metal*、dispatch*、screencapturekit* 等，或名字里按 - 和 _ 切出的词含 macos、darwin、appkit、metal 等。二是 target 条件只在 Apple 平台成立的依赖，不论名字。脚本用 `rustc --print cfg` 取得 rustc 已知的全部 target 的真实 cfg（按 rustc 版本缓存在 `target/` 下），条件在某个 Apple target 上成立、在其他 target 上都不成立，即为 Apple 专属。与 Cargo 一致，target 不设置的 cfg（`feature`、`test`、`proc_macro` 等）一律为假。target 按默认配置取，不应用本机的 rustflags，因此同一个提交在任何机器和 CI 上结果一致。工作区内的 crate 也按名字检查。`scripts/test_check_architecture.py` 是这部分的单元测试，CI 先跑它。
5. 每个 crate 的 `Cargo.toml` 写有 `[lints] workspace = true`，继承工作区的 lint 配置（见「Lint」）。
6. 只有 `app` 和 tool 层启用 `lectern-logging` 的 `sink` feature：依赖声明里的 feature 和 crate 自己的 feature 表（`lectern-logging/sink`、`lectern-logging?/sink`，含改名的依赖）都算（[logging 设计](logging.md)）。
7. `Value::panic_message` 只出现在 `crates/kernel`、`crates/executor`、`crates/logging` 的源码里：其他 crate 目录下任何 `.rs` 文件（去掉注释和字符串后）出现 `panic_message` 这个名字即报错（[logging 设计「隐私」](logging.md#隐私)）。
8. 工作区的 `[workspace.lints.clippy]` 把 `print_stderr`、`print_stdout` 设为 `warn`（或 `deny`、`forbid`）；tool 层之外的 crate 目录下，除 crate 根目录的 `build.rs` 外，任何 `.rs` 文件（去掉注释和字符串后）出现这两个名字即报错，即不能用 `allow`、`expect` 关掉它们（[logging 设计「对其他 crate 的要求」](logging.md#对其他-crate-的要求)第 10 项）。
9. 只有 `lectern-bench` 启用 `lectern-platform` 的 `bench-hooks` feature，而且只经它自己的 `probe` feature（`lectern-platform/bench-hooks`、`lectern-platform?/bench-hooks`，含改名的依赖）：在依赖声明的 feature 里打开、经别的 feature 打开、别的 crate 打开都报错；`lectern-platform` 自己的 `default` 里有它也报错（[platform 设计「测量构建的方法替换」](platform.md#测量构建的方法替换)）。

每条报错末尾的方括号写明规则出处（AGENTS.md 的规则编号或设计文档），agent 可以直接找到规则原文。

检查用 `cargo metadata --all-features`，可选 feature 引入的依赖也计入。每个 crate 自己的 dev-dependencies 也检查（它们会编译进测试和基准）；依赖项的 dev-dependencies 不会被编译，不检查。

`--gpui-free-packages` 输出在 Linux 上不依赖 GPUI 的 crate 列表（按 `x86_64-unknown-linux-gnu` 过滤依赖图，只为 macOS 声明的 GPUI 依赖不会把 crate 排除在外；含 crate 自身的 dev-dependencies，测试也不编译 GPUI），Linux 任务用它决定 clippy 与测试的范围，新增 crate 时无需修改 CI。

本地运行：

```sh
python3 scripts/check_architecture.py
```

规则 7（插件 crate 的依赖，见 [ADR 0025](../adr/0025.md)）的检查由 [#47](https://github.com/SoloJiang/lectern-app/issues/47) 加入。

## Lint

工作区的 `[workspace.lints.clippy]` 在 clippy 默认规则之外增加五条，`-D warnings` 下都是错误：

- `allow_attributes_without_reason`：`#[allow]` 和 `#[expect]` 必须写 `reason = "..."`，说明为什么关掉这条规则。
- `dbg_macro`：提交的代码里不留 `dbg!`。
- `undocumented_unsafe_blocks`：每个 `unsafe` 块前用 `// SAFETY:` 注释说明为什么安全。
- `print_stderr`、`print_stdout`：运行时的输出只经 `lectern-logging` 的宏（[ADR 0046](../adr/0046.md)）。`clippy.toml` 的 `allow-print-in-tests = true` 放过 `#[test]` 函数和 `#[cfg(test)]` 里的代码；tool crate（`bench`、`doc_check`）在打印的目标根上 `#![allow]`，`i18n` 的 `build.rs` 在报错的函数上 `#[allow(clippy::print_stderr)]`。其他 crate 不能关掉它们，由架构规则检查第 8 条执行。

今后用 `clippy.toml` 的 `disallowed-methods`、`disallowed-types` 禁用某个 API 时，`reason` 写明对应的架构规则或 ADR，例如 `reason = "视图层不做 IO（AGENTS.md 架构规则 4）"`。

`clippy.toml` 现有的配置都来自 [ADR 0046](../adr/0046.md)（[logging 设计「隐私」](logging.md#隐私)）：`disallowed-macros` 禁止 `log` 的六个宏，`disallowed-methods` 禁止 GPUI 的错误日志辅助方法（`gpui_util::ResultExt`、`gpui_util::TryFutureExt` 的 `log_err` 等和 `gpui::TaskExt::detach_and_log_err` 等；这些路径只在依赖 GPUI 的 crate 里解析得到，条目带 `allow-invalid = true`），`await-holding-invalid-types` 禁止跨 `.await` 持有 `lectern_logging::Scope`，`allow-print-in-tests` 让测试代码不受上面两条打印 lint 的限制。

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

macOS 基准拆成单独的任务（[ADR 0036](../adr/0036.md)，`Refs #7`）：

- 依据（jobs 接口，最近 30 次运行）：macOS 任务里 setup、check、测试和 doctest 约 4 分钟，基准一步在 #129 之前 5–6 分钟、之后约 13 分钟，`docs/budget-100k` 分支 13–16 分钟，`impl/code-intel-query` 分支 23–39 分钟；Linux 任务约 2 分钟。
- `ci.yml` 的语法和两个 macOS 任务的拆分只能由 CI 运行验证，本机没有运行 GitHub Actions。基准一步缩短多少见 [bench 设计「实际验证」](bench.md#实际验证)；拆开之后 macOS 的墙钟时间是两个任务中较长的一个。
- 没有验证的：分支保护的必需检查要由仓库管理员加上 `Benchmarks (macOS arm64)`。

判定任务（[ADR 0052](../adr/0052.md)，`Refs #7`）：

- `ruby -ryaml` 读 `ci.yml` 通过，`judge` 任务的 `if`、`runs-on`、`permissions` 和步骤顺序与「判定机器」一致。本机没有 actionlint，`ci.yml` 的其余检查由 GitHub 在推送时做。
- 没有验证的：任务本身没有运行过，runner 注册之后第一次运行才知道；仓库变量设为 `true` 之前任务跳过。fork PR 的批准设置、分支保护和机器的准备由产品负责人或仓库管理员做（「判定机器」）。

## 与设计的差异

[#34](https://github.com/SoloJiang/lectern-app/pull/34) 与 [#36](https://github.com/SoloJiang/lectern-app/pull/36) 的实现没有记录差异。[#50](https://github.com/SoloJiang/lectern-app/pull/50) 新增的规则 5、Lint、文档链接检查三处在本文对应小节里直接写成设计。文档链接检查最初是 Python 脚本，用正则识别 Markdown；审阅中陆续发现引用式链接、容器、代码片段等写法无法逐条覆盖，改为基于 CommonMark 解析器的 tool crate。
