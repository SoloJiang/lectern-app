# syntax（`lectern-syntax`）

语法高亮的领域库：语言表与高亮查询、着色区间（`Token`、`WindowSpans`）、整文件高亮、grammar 的卡死检测（`health`）。后续步骤加入首屏窗口、窗口链、文本与行索引、行检查点、LRU、打开的文档、预取和编辑模式的增量高亮。

- 层：领域层，普通的库 crate（不实现 `Plugin`，没有 `plugin = true`，[ADR 0041](../../docs/adr/0041.md)）。`file_reader`、`review_view` 和 `code_intel` 都可以依赖它；它不依赖任何插件 crate，不依赖 GPUI（架构规则 2）、`gix`、`rusqlite`、`ropey`。
- 设计文档：[file_reader 设计](../../docs/design/file_reader.md)的「`syntax` 的公开项」；实现进度见那里的状态行和「验收」。已实现第 1 步：`Languages`、`Lang`、`Token`、`TokenKind`、`WindowSpans`、`highlight::whole_file`、`health`、`limits` 里的卡死宽限。
- issue：[#10](https://github.com/SoloJiang/lectern-app/issues/10)
- ADR：[0018](../../docs/adr/0018.md)（核心高亮语言）、[0030](../../docs/adr/0030.md)（与 `code_intel` 共用 tree-sitter 和 grammar）、[0033](../../docs/adr/0033.md)、[0034](../../docs/adr/0034.md)、[0041](../../docs/adr/0041.md)、[0042](../../docs/adr/0042.md)

## 边界

- 解析和高亮查询只在 executor 的线程上运行，UI 线程不调用本 crate 的阻塞函数。每次进入 tree-sitter 之前在调用的线程上 `health::global().enter`，返回时（任何结果）drop 守卫；得到 None 时这种语言显示纯文本。
- 首帧之前不做任何事：`Languages::core()` 只建表，不编译查询、不做 IO；查询在第一次高亮时编译。
- 核心语言只在 `src/lang.rs` 的 `CORE_LANGUAGES` 和两个 `Cargo.toml` 的 grammar 依赖里列出；名字和扩展名与 `code_intel` 的语言表一致。
- 高亮查询在 `queries/<语言>/highlights.scm`，文件头写上游文件、版本、commit、版权行、这个查询用的优先规则（同一节点被几个模式捕获时取第一个还是最后一个）和 Lectern 的改动；MIT 的许可声明在 `queries/NOTICE`。JavaScript、TypeScript、TSX 由 `ecmascript`、`js`、`jsx`、`ts` 几个文件拼成。
- 捕获名到 `TokenKind` 的映射在 `lang::classify`；改映射、优先规则或按行切分的方式时把 `SPANS_VERSION` 加一（它进查询版本，缓存的区间随之失效）。

## 测试

不依赖 GPUI，在任何平台上都能编译和测试：

```sh
cargo check -p lectern-syntax     # 只改本 crate 时最快的反馈
just test -p lectern-syntax
just doctest                      # ParseGuard 不是 Send 的 compile_fail 示例
```

- 测试在 `src/tests/`，按前缀分文件：`lang_*`（`lang.rs`：扩展名、规则族、版本对照 Cargo.lock、ABI 范围、查询能编译、捕获名都认识、查询文件头）、`spans_*`（`spans.rs`：每种语言一份整文件区间的 insta 快照、`Token` 的编码边界、按行存放、捕获的解析、取消 / 超时 / 让路、与 `health` 的登记）、`health_*`（`health.rs`）。
- 快照的源码是 `src/tests/fixtures/` 下合成的小文件；改查询、升级 grammar 或 tree-sitter 时，`src/tests/snapshots/` 的变化逐个看过。
- `health_*` 用 `src/tests/health.rs` 的 `ManualClocks` 代替 `platform::ThreadCpuClock`：测试推进每个模拟线程的 CPU 时间和墙上时间，`run_on` 决定下一次 `enter` 在哪个线程上；`health_global_on_real_clocks` 用真实时钟。
- `rg -n 'std::fs::|File::open|mmap' crates/syntax/src --glob '!**/tests*'` 和 `rg -n 'thread::spawn' crates/syntax/src` 没有结果。
