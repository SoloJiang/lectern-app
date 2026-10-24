# ui_probe（`lectern-ui-probe`）

UI 探针：导出语义树（`ui_tree`）和带元素标注的截图（`screenshot`），让 agent 能读取和操作运行中的 Lectern（[技术调研「UI 结构探针」](../../docs/design/tech-research.md#6-ui-结构探针)）。

- 层：tool 层，只用于调试与测试构建，可以依赖任何层。只有 `app` 可以依赖本 crate，本 crate 不进入发布构建。
- 设计文档：[docs/design/ui_probe.md](../../docs/design/ui_probe.md)（已接受，实现中）。实现按它的「验收」分四步：第 3 步（本 crate）已实现；第 4 步（`app` 的 probe profile、`justfile` 的 `probe` 配方、端到端测试）还没有，`app` 现在不依赖本 crate。
- issue：[#6](https://github.com/SoloJiang/lectern-app/issues/6)

## 边界

- 只在调试和测试构建中编译，发布构建不包含。
- 只通过 Action 操作界面，不模拟坐标点击。P0 只读：`hello`、`ui_tree`、`screenshot`、`quit`。
- 语义节点的声明与记录在 `lectern-ui`（`semantic` 模块），本 crate 只读 `Recorder` 的记录；视图 crate 不依赖本 crate。
- 日志只经 `lectern-logging` 的宏，target 为 `ui_probe`。不记请求和应答的 JSON、节点的标签文字、业务键、属性值、窗口标题和像素；`method` 记 `'static` 的固定表（未知方法记 `"unknown"`），路径只经 `Value::path`。库部分不用 `println!` / `eprintln!`，只有二进制 `lectern-probe` 打印。
- 主线程上的工作只有取帧和 `render_to_image`；标注、PNG 编码、写文件、JSON 编码都在连接线程。

## 模块

| 文件 | 内容 |
|---|---|
| `protocol.rs` | JSON Lines 的 `Request`、`Response`、`ErrorKind`，请求解析，`name` 与 `dir` 的检查 |
| `tree.rs` | `Tree`（`ui-tree/1`）、`from_records`、ID 语法与 `warnings`，`capture`（两帧、`Cancel`）与测试用的 `snapshot` |
| `annotate.rs` | `annotate`（边框与点阵编号）、`write_png`（临时文件加改名） |
| `server.rs` | `Probe::start` / `shutdown`、监听线程、连接线程、`Session::process`（请求到应答，不开 socket 就能测）、主线程的前台任务 `handle` |
| `client.rs` | 阻塞客户端 `Client`（标准库与 `serde_json`），`app` 的集成测试也用它 |
| `bin/lectern-probe.rs` | 命令行：`hello`、`ui_tree`、`screenshot`、`quit`、`raw` |

## 测试

依赖 GPUI，只在 macOS 上编译：

```sh
just test -p lectern-ui-probe
```

- 同一套函数既供运行中的应用调用，也在 `#[gpui::test]` 里直接调用做断言；语义树存成 insta 快照（`src/tests/snapshots/`）。
- 记录器的开关是进程级的，记录的测试要用 nextest（一个测试一个进程）。
- `capture` 的测试用 `Window::simulate_next_frame` 送帧：GPUI 的测试上下文在一次 `update` 结束时画脏窗口，所以第一个回调之后不要再手动 `draw`。整条会话的测试（`a_session_answers_*`）用真实的 Unix socket 和另一个线程里的 `Client`，要 `cx.executor().allow_parking()`。
- 测试平台没有渲染器：`render_to_image` 返回错误，`screenshot` 应答 `render`。标注用伪造的 `RgbaImage` 单独测；真实窗口的读回、PNG 不是单一颜色只能在有图形会话的 macOS 上验证（设计文档「实际验证」列出了没有验证的部分）。
- 隐私：`canary_stays_out_of_probe_logs`。
