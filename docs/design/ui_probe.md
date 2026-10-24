# ui_probe 设计

状态：实现中。第 3 步（`ui_probe` crate：协议、`tree`、`annotate`、`server`、`client`、`lectern-probe`）已实现（[#@@PR@@](https://github.com/SoloJiang/lectern-app/pull/@@PR@@)）；第 4 步（`app` 的 probe profile、`justfile` 的 `probe` 配方、端到端测试）尚未实现。

对应 issue [#6](https://github.com/SoloJiang/lectern-app/issues/6)。`ui_probe` 是 agent 读取运行中的 Lectern 的入口：把窗口里的语义树导出成 JSON（`ui_tree`），把窗口画成带节点编号的 PNG（`screenshot`），让 agent 不必从像素推断界面结构（[技术调研「UI 结构探针」](tech-research.md#6-ui-结构探针)）。它是 tool 层 crate，只在 probe profile 里运行，不进入发布构建（[cargo-workspace 设计](cargo-workspace.md)、[CI「架构规则检查」](ci.md#架构规则检查)第 2 条）。

## 范围

- 做：
  - 探针会话：一条本机 Unix 域 socket，一个 JSON Lines 协议，一个命令行客户端 `lectern-probe`；只有本用户的进程能连。
  - `ui_tree`：读取 `ui` 的语义记录器在一帧里记下的节点（ID、角色、标签的 Fluent 键和渲染文本、范围、状态、层级），输出 `ui-tree/1` 格式的 JSON。
  - `screenshot`：同一帧的窗口画面，PNG，叠加节点边框和短编号，编号与 `ui_tree` 的节点一一对应；文件写到客户端指定的目录。
  - `quit`：按 ⌘Q 的路径退出应用，供探针会话结束时使用。
  - 同一套函数供运行中的应用和 `#[gpui::test]` 调用；语义树可以存成 insta 快照。
- 不做：
  - 语义节点的声明与记录：在 `ui`（见「对其他 crate 的要求」）。视图 crate 用 `ui` 的接口登记节点，不依赖本 crate。
  - probe profile 的选择与探针的启动时机：在 `app`（见「对其他 crate 的要求」）。
  - 输入：P0 不注入点击、按键和 Action。理由见「输入」。
- 本 issue 之外：
  - 输入（P1）：`actions`（列出已注册的 Action 名）、`dispatch`（按名字触发 Action，GPUI 的 `App::build_action`）、`key`（`Window::dispatch_keystroke`）。只经 Action 和按键，不模拟坐标点击（本 crate 的 AGENTS.md「边界」）。
  - `locate`（P1）：节点到源码位置。`ui` 的语义包装元素用 `#[track_caller]` 记下调用位置，记录器一起记下；本 issue 先不输出。
  - CI 检查「可交互元素都带 ID」：等视图 crate 有内容后，随第一个视图的实现 PR 加入。
  - 多窗口：协议里的 `window` 参数已预留；P0 只有一个窗口。

## 依赖

| 依赖 | 用途 | 选用理由 |
|---|---|---|
| `gpui` | `Window::on_next_frame`、`refresh`、`render_to_image`、`#[gpui::test]` | 已锁定（[ADR 0016](../adr/0016.md)） |
| `gpui_platform`（`test-support` feature） | 打开 macOS 后端的 `render_to_image` | `Window::render_to_image` 只在 `test-support` 下编译，而 macOS 后端（`gpui_macos`、`gpui_apple`）的实现要经 `gpui-pre-platform` 的 `test-support` 才打开，只开 `gpui/test-support` 会走平台 trait 的默认实现、总是返回错误（见「截图的来源」）；本 crate 不进入发布构建，所以这个 feature 不会进入发布构建 |
| `lectern-ui` | 语义记录器 `semantic::Recorder` 与节点类型（`Semantic`、`Role`、`State`） | 视图层共用的声明接口只能放在所有视图 crate 都依赖的 crate 里 |
| `futures`（`std`） | 连接线程到主线程的请求队列：`channel::mpsc::unbounded` 与 `channel::oneshot` | 已在工作区（`workspace` 用同一个队列形式）；不另引入 `async-channel` |
| `rustix`（`process`） | 探针目录属于当前用户的检查（`geteuid`） | 已在工作区；`process` 不带新的依赖 |
| `lectern-logging` | 日志宏 | [ADR 0046](../adr/0046.md) |
| `serde`、`serde_json` | 协议与 `ui-tree/1` 的编码 | 已在工作区 |
| `image`（`png`） | `render_to_image` 的返回类型 `RgbaImage`，PNG 编码 | GPUI 已依赖它并打开 `png`；工作区多一条声明（`default-features = false`，只开 `png`），Cargo.lock 没有新的 package |
| `insta`、`tempfile`、`lectern-i18n`（dev） | 快照；临时目录；测试场景里带 Fluent 键的标签 | 已在工作区 |

不新增外部 crate。`gpui_platform/test-support`（它连带打开 `gpui/test-support`）经本 crate 的普通依赖打开：`just check` 与 `just test` 用 `--all-features`，工作区在这两条路径上本来就带着 `gpui/test-support`（`app` 的 dev-dependency），后端那一层是新增的；不带 feature 的 `cargo build -p lectern-app` 不编译本 crate，发布构建的依赖图里没有 `lectern-ui-probe`，也没有 `test-support`（「验收」第 4 步的 `cargo tree` 检查，与 [bench 设计「验收」](bench.md#验收)相同）。

### 语义树的来源

锁定的 GPUI（[ADR 0016](../adr/0016.md)）自带 AccessKit 无障碍树：元素带 `.id()` 和 `.role()` 时，GPUI 每帧构造 `TreeUpdate`，`Window::debug_a11y_tree_json` 能导出上一帧的树。本设计不读它，而由 `ui` 另设一个记录器，原因：

1. GPUI 只在平台适配器收到辅助技术的激活回调时才构造这棵树（`Window::is_a11y_active`），没有公开的开关；`#[gpui::test]` 的测试平台没有适配器，永远不激活。探针要在测试里和应用里跑同一套函数（本 crate 的 AGENTS.md「测试」），只能自己记录。
2. 探针需要 Fluent 键、`视图.区域.条目[业务键]` 形式的 ID 和视图自定义的属性（已看、偏离），AccessKit 节点没有对应字段。

记录器与 AccessKit 不冲突：`ui` 的 `semantic_interactive` 用同一份声明同时设置 GPUI 的 `accessibility_id`、`role`、`aria_label` 等（见「对其他 crate 的要求」），读屏软件和通用的 computer-use 工具看到的 ID 与探针相同（macOS 的 `AXIdentifier`）。GPUI 以后提供激活开关时，可以改读它的树，不改协议。

### 截图的来源

`Window::render_to_image` 把上一帧的场景（`rendered_frame.scene`）用窗口自己的 Metal 渲染器画到一个 drawable 上，不显示，等命令缓冲完成后读回 `RgbaImage`（物理像素）。它与 `ui_tree` 读的是同一帧，不需要窗口在前台，也不需要屏幕录制权限。代价是这个调用在主线程上同步等 GPU，并占用一个 drawable；实测耗时写进「实际验证」。

两个编译期条件，实现必须同时满足：

1. feature 要开在 `gpui_platform`（`gpui-pre-platform`）上：`gpui-pre-platform/test-support` 打开 `gpui_macos/test-support`，再打开 `gpui_apple/test-support`，macOS 窗口的 `render_to_image` 才存在；只开 `gpui/test-support` 时 `Window::render_to_image` 能编译，但调到平台 trait 的默认实现，总是返回错误。
2. 只有调试构建能读回：Metal 层的 `framebufferOnly = false` 在 `test-support` 且 `debug_assertions` 时才设置，release 构建即使带 `ui-probe` feature，读回也失败。所以 `screenshot` 只在调试构建里可用（`just probe` 用 dev profile）；release 构建里的探针照常提供 `ui_tree`，`screenshot` 不调用 `render_to_image`（对 framebuffer-only 纹理读回可能是 Metal 的断言而不是 `Err`），按 `cfg!(debug_assertions)` 直接应答 `render`。

备选：`platform` 用 `CGWindowListCreateImage` 截取本进程的窗口（macOS 专属，架构规则 6）。它截的是真正显示的画面（含系统材质），但要取 `NSWindow` 的编号，而且本进程窗口是否免于 TCC 的屏幕录制授权本文没有验证。只在 `render_to_image` 在实现阶段不可用时改用，改用要更新本节。

## 概念

| 概念 | 说明 |
|---|---|
| 探针会话 | 一次 probe profile 的运行：`app` 带 `ui-probe` feature 构建，启动时设置环境变量 `LECTERN_UI_PROBE_DIR`，首帧之后监听这个目录里的 socket，直到进程退出 |
| 探针目录 | `LECTERN_UI_PROBE_DIR` 指向的目录：客户端建好（通常是 `mktemp -d` 的临时目录），探针在里面建 `probe.sock`，默认也把截图写在这里。它不是仓库目录 |
| 语义节点 | 视图用 `ui::semantic` 声明的一个界面元素：稳定 ID、角色、标签、状态、属性，加上记录器在一帧里测到的范围和层级 |
| 记录 | 一帧里记录器收到的节点列表。只在探针请求的那一帧里记录，其余帧记录器关闭 |
| 编号 | `ui-tree/1` 里每个节点的序号 `n`，按先序深度优先从 1 起；截图上画的就是它 |

## 接口

本 crate 分五个模块：`protocol`（协议类型）、`tree`（记录到 `ui-tree/1`）、`annotate`（标注与 PNG）、`server`（探针会话）、`client`（命令行客户端用的阻塞客户端），以及二进制 `lectern-probe`（`src/bin/lectern-probe.rs`）。

### 探针会话（`server`）

```rust
pub struct Config {
    /// 探针目录，`app` 从 `LECTERN_UI_PROBE_DIR` 读出；必须是绝对路径。
    pub dir: PathBuf,
    /// 主线程上等一帧的上限。默认 5 s：窗口不在前台时 GPUI 按 30 fps 请求帧，被完全遮挡时更慢。
    pub frame_timeout: Duration,
}

/// 一个探针会话。`app` 在启动屏障放开后创建，退出时 drop。
pub struct Probe { /* 监听线程的句柄、停止标志 */ }

#[derive(Debug)]
pub enum StartError {
    /// 目录不存在、不是目录、不属于当前用户，或其他用户可访问（权限不是 0700）。
    DirNotPrivate,
    /// `bind` 失败；`probe.sock` 已存在时先删除再 bind，删除失败也归这里。
    Bind(io::Error),
    Thread(io::Error),
}

impl Probe {
    /// 主线程，首帧之后调用；不阻塞。检查目录，bind `dir/probe.sock`（权限 0600），
    /// 建监听线程 `lectern-ui-probe`，用 `cx.spawn` 建一个前台任务处理请求。
    /// 失败时 `app` 记一条 warn，应用照常运行。
    pub fn start(cx: &mut App, config: Config) -> Result<Probe, StartError>;

    /// 任意线程，不阻塞：置停止标志、关闭监听 socket，监听线程退出时删除 `probe.sock`。
    /// drop 时也调用。`quit` 请求应答后由 `app` 的退出顺序 drop 它。
    pub fn shutdown(&self);
}
```

### 记录到树（`tree`）

```rust
/// 主线程。让 `window` 多画一帧并在这一帧里打开记录器，帧画完后把记录整理成 `Tree` 交给 `done`。
/// 用两次 `Window::on_next_frame`：第一个回调（下一次帧请求开头，画之前）`refresh`、打开记录器并登记第二个回调；
/// 第二个回调（再下一次帧请求开头，画之后）关闭记录器、读出记录、构造 `Tree`。不阻塞。
/// `with_image` 为 true 时在第二个回调里调用 `Window::render_to_image`，渲染和读回在主线程上同步进行。
/// 返回的 `Cancel` 让等待超时的一方停掉这次捕获：之后两个回调什么也不做，记录器若已为它打开则关闭，
/// 所以迟到的帧不会读到别人的记录。
pub fn capture(
    window: &mut Window,
    with_image: bool,
    done: impl FnOnce(Result<Capture, CaptureError>) + 'static,
) -> Cancel;

/// 测试用：同一个记录器，但不走帧循环：打开记录器，`Window::draw`，关闭并构造 `Tree`。
/// 主线程（测试里是 `VisualTestContext::update`），阻塞到画完。
pub fn snapshot(window: &mut Window, cx: &mut App) -> Tree;

pub struct Capture {
    pub tree: Tree,
    /// 物理像素，`with_image` 为 true 时有。
    pub image: Option<image::RgbaImage>,
}

#[derive(Debug)]
pub enum CaptureError {
    /// `render_to_image` 失败（没有 drawable、渲染器不可用）。
    Render,
}

/// `ui-tree/1` 的内存形式，`serde::Serialize`。字段见「数据结构」。
pub struct Tree { /* window, nodes, warnings */ }

impl Tree {
    /// 纯函数：从记录器的一帧记录构造树。`capture` 与 `snapshot` 都经它；单元测试直接给它伪造的记录。
    pub fn from_records(window: WindowInfo, records: Vec<lectern_ui::semantic::Record>) -> Tree;
    pub fn to_json(&self) -> String;
}
```

### 标注与 PNG（`annotate`）

```rust
/// 纯函数，任意线程。在 `image` 上为 `tree` 里 `visible` 的节点画边框和编号；范围按 `tree.window.scale` 换算成物理像素。
/// 边框 2 物理像素，颜色按深度在 4 种固定颜色里循环；编号用内置的 5×7 点阵字体，按 `ceil(scale)` 放大，
/// 白字黑底，画在节点范围内的左上角；超出图片的部分裁掉。画面不是界面，不受 ADR 0015 约束。
pub fn annotate(image: &mut image::RgbaImage, tree: &Tree);

/// 任意线程，阻塞：编码成 PNG 并原子写入 `path`（先写同目录的临时文件再改名）。
pub fn write_png(image: &image::RgbaImage, path: &Path) -> io::Result<()>;
```

### 协议类型（`protocol`）

```rust
pub const PROTOCOL: u32 = 1;
pub const TREE_SCHEMA: &str = "ui-tree/1";

#[derive(Deserialize)]
pub struct Request {
    pub v: u32,
    /// 客户端给的序号，原样写回应答。
    pub id: u64,
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

#[derive(Serialize)]
pub struct Response {
    pub v: u32,
    pub id: u64,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<Error>,
}

#[derive(Serialize)]
pub struct Error {
    pub kind: ErrorKind,
    /// 固定文案，不含路径和界面文字。
    pub message: &'static str,
}

#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "kebab-case")]
pub enum ErrorKind {
    BadRequest, UnknownMethod, NoWindow, Timeout, Render, Io, Quitting,
}
```

方法与参数（`params` 里的字段都可省略）：

| 方法 | 参数 | 结果 |
|---|---|---|
| `hello` | 无 | `{ "protocol": 1, "pid": …, "schemas": ["ui-tree/1"], "dir": "<探针目录>", "windows": [{ "id": 1, "title": "…", "size": { "w": 820, "h": 520 }, "scale": 2.0 }] }` |
| `ui_tree` | `window`（窗口 ID，默认 key window，没有时取第一个） | `ui-tree/1` 对象（「数据结构」） |
| `screenshot` | `window`；`dir`（绝对路径，默认探针目录）；`name`（`[A-Za-z0-9._-]+`，不能是 `.`、`..`，默认 `shot-<序号>`）；`annotate`（默认 true） | `{ "path": "<dir>/<name>.png", "width": …, "height": …, "scale": 2.0, "tree": <ui-tree/1> }`，`tree` 与图片来自同一帧 |
| `quit` | 无 | `{}`；应答写出后在主线程上分发 `app` 的 `Quit` Action（与 ⌘Q 相同） |

协议是 JSON Lines：UTF-8，一行一条请求、一行一条应答，按顺序一一对应；请求一行不超过 1 MiB，超过即断开连接。`v` 不是 1 时应答 `bad-request`。探针按收到的顺序处理请求，一次一条；多个客户端可以同时连接，各自的请求排进同一个队列。

### 客户端（`client`、`lectern-probe`）

```rust
/// 阻塞客户端，只用标准库和 serde_json；供命令行和 `app` 的集成测试使用。
pub struct Client { /* UnixStream、BufReader、下一个 id */ }

impl Client {
    /// 连接 `dir/probe.sock`；`timeout` 是每次应答的读超时。
    pub fn connect(dir: &Path, timeout: Duration) -> io::Result<Client>;
    /// 发一条请求，等它的应答；`Err` 只在连接层出错时返回，协议层的错误在 `Response::error` 里。
    pub fn call(&mut self, method: &str, params: serde_json::Value) -> io::Result<Response>;
}
```

命令行 `lectern-probe`（`cargo run -p lectern-ui-probe --bin lectern-probe -- …`）：

```
lectern-probe [--dir <探针目录>] [--timeout <秒>] hello
lectern-probe … ui_tree
lectern-probe … screenshot [--name <名字>] [--out <目录>] [--no-annotate]
lectern-probe … quit
lectern-probe … raw '<一行 JSON 请求>'
```

`--dir` 默认取 `LECTERN_UI_PROBE_DIR`。结果 JSON 写到 stdout（`result` 本身，不含信封），协议错误写到 stderr、退出码 1，连不上退出码 2。tool crate 允许打印到 stdout 和 stderr（[logging 设计「隐私」](logging.md#隐私)）。

### 输入

P0 只读。原因：issue #6 的范围是 `ui_tree` 与 `screenshot`，G1 关卡验证的是「agent 能否靠树和截图看懂界面」；视图层还没有内容，没有可触发的 Action。协议的方法名空间已为 P1 预留（`actions`、`dispatch`、`key`、`locate`）。`quit` 是生命周期操作，不算输入。

## 数据结构

### 语义节点（`ui::semantic`，由 `ui` 提供）

视图声明的部分：

```rust
// crates/ui/src/semantic.rs（`ui` 实现；这里是本 crate 读取的最小形状）
pub struct Semantic {
    /// `视图.区域.条目[业务键]`，见下文「ID 的语法」。`NodeId` 是 `ui` 定义的 `SharedString` 新类型，
    /// 用 `NodeId::new(view, area, key)`、`.child(name)`、`.item(key)` 构造，调试构建里校验语法。
    pub id: NodeId,
    pub role: Role,
    /// 纯容器（列表、面板、分组）没有文字，为 None。
    pub label: Option<Label>,
    pub state: State,
    /// 视图自定义的属性，键是静态标识符，如 ("seen", "true")、("drifted", "true")。
    pub attrs: Vec<(&'static str, SharedString)>,
}

pub struct Label {
    /// 标签文字来自 Fluent 时写它的键名（`Key::name()`）；业务文字（路径、会话标题）为 None。
    pub key: Option<&'static str>,
    /// 渲染出来的文字。
    pub text: SharedString,
}

#[derive(Default)]
pub struct State {
    pub selected: bool,
    pub focused: bool,
    pub disabled: bool,
    /// 可展开的节点才有值。
    pub expanded: Option<bool>,
    /// 开关、复选才有值。
    pub checked: Option<bool>,
}

/// 序列化成 kebab-case 字符串。新增角色不改 schema 版本。
pub enum Role {
    Window, Sidebar, Toolbar, Panel, Group, List, ListItem, Tree, TreeItem, TabBar, Tab,
    Button, Toggle, Text, TextField, SearchField, Link, Menu, MenuItem, Dialog,
    Diff, DiffHunk, DiffLine, Comment, Badge, Progress,
}
```

记录器测到的部分：

```rust
pub struct Record {
    pub semantic: Semantic,
    /// 逻辑像素，窗口坐标。
    pub bounds: Bounds<Pixels>,
    /// 范围与当时的 content mask 和视口有交集。
    pub visible: bool,
    /// 父节点在同一帧记录里的下标；没有语义祖先时为 None（挂到窗口根节点下）。
    pub parent: Option<usize>,
    /// `#[track_caller]` 记下的声明位置，P1 的 `locate` 用；P0 不输出。
    pub declared_at: &'static Location<'static>,
}
```

ID 的语法：`segment('.' segment)*`，`segment` 是 `name` 或 `name[key]`；`name` 匹配 `[a-z0-9-]+`；`key` 是业务键（路径、会话 ID、轮次号），任意 UTF-8，不含换行，不含「`]` 紧跟 `.`」的序列（它是段的分隔）。列表条目用业务键，不用下标。`NodeId` 的构造函数在调试构建里校验这条语法；本 crate 在每一帧再检查一次语法和唯一性（`NodeId` 也能从任意 `SharedString` 构造），不合规的节点照常输出，并写进 `warnings`。

### `ui-tree/1`

```json
{
  "schema": "ui-tree/1",
  "window": { "id": 1, "title": "Lectern", "size": { "w": 820, "h": 520 }, "scale": 2.0, "key": true },
  "nodes": [
    { "n": 0, "id": "window[1]", "role": "window", "label": { "key": null, "text": "Lectern" },
      "bounds": { "x": 0, "y": 0, "w": 820, "h": 520 }, "visible": true,
      "state": { "selected": false, "focused": false, "disabled": false, "expanded": null, "checked": null },
      "attrs": {}, "parent": null, "children": [1] },
    { "n": 1, "id": "review.files", "role": "list", "label": null,
      "bounds": { "x": 12, "y": 40, "w": 240, "h": 400 }, "visible": true,
      "state": { "selected": false, "focused": false, "disabled": false, "expanded": null, "checked": null },
      "attrs": {}, "parent": 0, "children": [2] },
    { "n": 2, "id": "review.files.item[src/main.rs]", "role": "list-item",
      "label": { "key": null, "text": "src/main.rs" },
      "bounds": { "x": 12, "y": 48, "w": 240, "h": 22 }, "visible": true,
      "state": { "selected": true, "focused": false, "disabled": false, "expanded": null, "checked": null },
      "attrs": { "seen": "true" }, "parent": 1, "children": [] }
  ],
  "warnings": [ { "kind": "duplicate-id", "id": "…" }, { "kind": "bad-id", "id": "…" } ]
}
```

- `nodes` 是先序深度优先的平铺列表，`n` 是下标，根节点 `n = 0` 是记录器合成的窗口节点（标题来自 `Window::window_title`，范围是视口）。`label` 为 `null` 的是纯容器。
- 范围是逻辑像素（pt），窗口坐标；`scale` 是 `Window::scale_factor`。截图是物理像素，客户端用 `scale` 换算。
- 兼容策略：新增角色、新增可选字段、新增 `warnings` 种类不改版本；改名、删字段、改坐标单位时版本加一，`hello` 的 `schemas` 列出支持的版本。
- 快照：`#[gpui::test]` 把 `Tree::to_json()` 存成 insta 快照（`crates/ui_probe/src/snapshots/`），视图 crate 的测试同样存自己的树。

### 截图文件

`<dir>/<name>.png`，RGBA 8 位，尺寸是 `render_to_image` 返回的物理像素。`dir` 必须是绝对路径且已存在，本 crate 不创建目录，不解析相对路径（客户端进程的工作目录与探针无关）；`name` 的规则见协议表。默认目录是探针目录，它由客户端用临时目录建立，所以截图默认不落进仓库。

## 数据流与线程

### 探针会话的生命周期

1. `app` 在首帧之后、`startup::release` 的末尾调用 `Probe::start`（只在 `ui-probe` feature 且 `LECTERN_UI_PROBE_DIR` 已设置时，见「对其他 crate 的要求」）。首帧之前不读目录、不建线程、不 bind（[ADR 0017](../adr/0017.md)）。
2. `start` 在主线程上：`metadata(dir)` 检查目录（存在、是目录、`uid` 等于当前用户、权限 `0700`）；删除残留的 `probe.sock`；`UnixListener::bind`；`set_permissions(0o600)`；建监听线程；`cx.spawn` 一个前台任务，等待请求队列。
3. 监听线程循环 `accept`；每个连接一个短命线程 `lectern-ui-probe-conn`，按行读请求，解析成 `Request`，连同一个应答通道放进队列（`futures::channel::mpsc::unbounded`；应答通道是标准库的 `mpsc::channel`），阻塞等应答，再写回一行。连接线程之间只共享停止标志、计数和一张连接表（供 `shutdown` 结束它们）。
4. 前台任务在主线程上逐条取请求、处理（下文）、把结果发回应答通道。
5. `shutdown`：置停止标志，对每个打开的连接 `shutdown(Both)`（阻塞在读上的连接线程读到结束后退出），再自己连一次监听 socket 唤醒 `accept`（`UnixListener` 不能从别的线程关闭）；监听线程看到停止标志，删除 `probe.sock` 后退出（`shutdown` 也删一次，所以返回时文件已不在）。`app` 的退出顺序在关闭 kernel 之前先 `shutdown` 探针。

### 一次 `ui_tree`

| 步骤 | 线程 | 做什么 |
|---|---|---|
| 1 | 连接线程 | 读一行，解析；进入 `req` 作用域（下文「日志与诊断」）；入队 |
| 2 | 主线程（前台任务） | 选窗口；`on_next_frame(cb1)`；等 cb2，最多 `frame_timeout` |
| 3 | 主线程（帧请求 N 的开头） | cb1：`window.refresh()`；`Recorder::begin(window_id)`；`on_next_frame(cb2)` |
| 4 | 主线程（帧 N 的 draw） | 每个语义包装元素在 prepaint 里把 `Record` 推进记录器；元素嵌套决定 `parent` |
| 5 | 主线程（帧请求 N+1 的开头） | cb2：`Recorder::finish()` 取出记录；`Tree::from_records`；发回应答通道 |
| 6 | 连接线程 | `serde_json` 编码，写一行 |

GPUI 在每次帧请求的开头先执行上一帧登记的 `on_next_frame` 回调，再 `draw`（[app 设计「启动屏障」](app.md#启动屏障)用的是同一个顺序），所以 cb1 在画之前、cb2 在画之后。`refresh` 在 cb1 里调用（而不是登记 cb1 时）：cb1 在同一次帧请求里先于「是否要画」的判断，所以窗口此刻一定是脏的，帧 N 一定会画，而且绕过视图缓存，每个包装元素都会 prepaint。记录器只在 cb1 与 cb2 之间打开：探针没有请求时，语义包装元素每帧只读一个原子标志，不记录。前台任务用 `frame_timeout` 等 cb2：超时应答 `timeout`，并丢弃之后迟到的记录。

### 一次 `screenshot`

与 `ui_tree` 相同，cb2 里多一步：`window.render_to_image()`，在主线程上同步渲染并读回，`Capture` 带上图片发回连接线程。连接线程做其余工作：`annotate`（CPU 画框，与图片大小成正比）、PNG 编码、原子写入 `dir/name.png`。编码与写文件都不在主线程上。

### 测试里的路径

`tree::snapshot` 不经帧循环：打开记录器，`Window::draw`，关闭，构造树。`#[gpui::test]` 用 `VisualTestContext::update` 调它。测试平台的 `render_to_image` 没有渲染器时返回错误，所以标注用伪造的 `RgbaImage` 单独测。

## 性能

- desktop profile：本 crate 不被编译、不链接；冷启动、空闲内存、安装包体积都不变。检查方式见「验收」第 4 步（`cargo tree`、`just bench` 的 `app/*` 不受影响）。
- 每个构建都有的成本：`ui` 的语义包装元素在 prepaint 里读一次原子标志（Relaxed），每元素每帧不到 1 ns，包装元素自己不分配。视图构造 `Semantic` 的成本（带业务键的 ID 要拼字符串）在所有构建里都存在（「对其他 crate 的要求」第 1 项）。两者都由 `ui` 的帧基准覆盖（`ui` 的设计文档决定基准名），本 crate 不另设基准。
- probe profile 空闲：一个阻塞在 `accept` 的线程、一个等待队列的前台任务，没有定时器、没有每帧工作。内存：线程栈加几 KiB。
- 一次 `ui_tree`：多画一帧；记录器的 `Vec<Record>` 与节点数成正比（每个约 200 B，1000 个节点约 200 KiB），请求结束即释放；`Tree::from_records` 与 JSON 编码是线性的。
- 一次 `screenshot`：`render_to_image` 在主线程上同步等 GPU 并读回（2× 的 820 × 520 窗口是 1640 × 1040 × 4 ≈ 6.8 MB），预期 10 ms 量级，实测写进「实际验证」；PNG 编码在连接线程上，预期几十毫秒。探针会话不是用户场景，这两项不计入帧预算；但主线程被占期间窗口照常响应，不出现超过 `frame_timeout` 的停顿。
- 首帧之前：什么也不做。`app` 读 `LECTERN_UI_PROBE_DIR` 的时机见「对其他 crate 的要求」。

## 状态与异常

| 情况 | 处理 |
|---|---|
| 没有 `ui-probe` feature，或没有设置 `LECTERN_UI_PROBE_DIR` | 探针不存在；应用与 desktop profile 完全一样 |
| 探针目录不存在、不属于当前用户、权限不是 0700 | `start` 返回 `DirNotPrivate`，`app` 记 warn `probe not started`，应用照常运行 |
| `probe.sock` 残留 | 先删除再 bind；删除失败归 `Bind` |
| bind 失败、线程建不起来 | `app` 记 warn，应用照常运行 |
| 请求不是 UTF-8、不是 JSON、缺字段、`v` 不是 1 | 应答 `bad-request`（有 `id` 时带 `id`，否则 `id` 为 0）；一行超过 1 MiB 时断开连接 |
| 未知方法 | 应答 `unknown-method` |
| 没有窗口（全部关闭后点 Dock 之前） | 应答 `no-window` |
| `frame_timeout` 内没画出帧 | 应答 `timeout`；迟到的记录丢弃 |
| `render_to_image` 失败（没有 drawable、渲染器不可用），或 release 构建（不调用它） | 应答 `render`；`ui_tree` 不受影响 |
| `dir` 不是绝对路径或不存在、`name` 不合规、写文件失败 | 应答 `io`，不创建目录，不写任何文件 |
| 退出已开始 | 之后的请求应答 `quitting`；连接随进程结束 |
| 客户端中途断开 | 连接线程退出；正在处理的请求照常完成，结果丢弃 |
| 节点 ID 重复或不合语法 | 照常输出节点，`warnings` 里各记一条 |

## 日志与诊断

记录经 `lectern-logging` 的宏写出，target 为 `ui_probe`。

- 默认记录：
  - info `probe listening`：`dir`（`Value::path`）。
  - info `probe client connected`、`probe client disconnected`：`conn`（连接序号，进程内从 1 递增）。
  - warn `probe request failed`：`req`、`method`（`&'static str`，未知方法记 `"unknown"`）、`error`（`ErrorKind`，实现 `Field`）；IO 错误另带 `io`（`Value::io`）。
  - warn `probe start failed`（由 `app` 在 `Probe::start` 返回 `Err` 时记，target 为 `app`）：`reason`（`dir-not-private`、`bind`、`thread`）、`error`（`Value::io`，有时）。
  - info `probe stopped`：`conns`（本次会话的连接数）、`requests`。
- 详细级别：debug `probe request`：`req`、`conn`、`method`、`took_us`、`nodes`（节点数）、`bytes`（应答字节数）；`screenshot` 另带 `path`（`Value::path`）、`width`、`height`。trace 不另记。
- 关联 ID：`req`（`Ctx::req`，进程内从 1 递增）。logging 设计把它定义为一次对外请求的序号；探针请求是进程外发来的一次请求，同样是一次跨进程的往返，借用同一个字段，不新增 `Ctx` 字段。连接线程解析出请求后 `scope(Ctx { req, .. })`，入队的请求带着序号，主线程处理时 `within` 同一个 `Ctx`。
- 不记录：请求和应答的 JSON；节点的标签文字、业务键和属性值；截图的像素；窗口标题。路径只经 `Value::path` 记录（探针目录、截图路径）。
- 金丝雀测试：`canary_stays_out_of_probe_logs`：`testing::capture()` 在 trace 级别下，用一个标签文字、一个业务键和一个属性值都含金丝雀串的视图跑 `tree::snapshot`，再把树经 `annotate` 和 `write_png` 写到临时目录，并让 `server` 的请求处理函数处理一条 `method` 为金丝雀串的请求（应答 `unknown-method`），`assert_absent(金丝雀)`。

## 测试

`just test -p lectern-ui-probe`，依赖 GPUI，只在 macOS 上编译。

- 单元测试：
  - `tree`：`from_records` 的层级（`parent` 转成 `children`、先序编号）、根节点的合成、ID 语法（合法的各种形式；`]` 后接 `.` 的拆分；非法 ID 进 `warnings`）、重复 ID 进 `warnings`、`visible` 的传递；`to_json` 的 insta 快照 `tree_basic`、`tree_warnings`。
  - `annotate`：伪造 200 × 100 的图片和 3 个节点（含一个不可见、一个超出图片），断言边框像素的颜色与位置、不可见节点没有边框、编号的点阵与预期一致；`write_png` 写出的文件能被 `image` 解码且尺寸相同。
  - `protocol`：请求解析（缺 `params`、`v` 错、`id` 缺失）、应答编码的 insta 快照；`name` 与 `dir` 的校验。
  - `server`：请求处理函数（不开 socket）对 `hello`、`unknown`、`quitting` 的应答；行长度上限。
  - `client`：用 `UnixListener` 起一个假探针，回放固定应答，测 `call` 与读超时。
- `#[gpui::test]`：
  - `snapshot_of_nested_semantics`：一个视图，三层嵌套的语义元素，`tree::snapshot` 的 JSON 存成 insta 快照。
  - `records_only_while_armed`：没有 `begin` 时画一帧，记录器为空。
  - `capture_uses_two_frames`：`capture` 之后 `simulate_next_frame` 两次才调用 `done`，一次不调用。
- 金丝雀：`canary_stays_out_of_probe_logs`（「日志与诊断」）。
- 端到端（在 `app`，见「对其他 crate 的要求」）：真实进程、临时 `HOME`、探针目录，`hello` → `ui_tree` → `screenshot` → `quit`。
- 只能在 macOS 上跑：全部。

## 对其他 crate 的要求

每一项在对应 crate 的设计文档里写入，各自一个 PR，在本 crate 的实现 PR 之前合并（第 4 项除外）。

1. **`ui`：语义记录器与语义包装元素**（模块 `lectern_ui::semantic`，[#214](https://github.com/SoloJiang/lectern-app/issues/214) 的 `ui` 设计文档已按本节写入，形状以它为准）。本 crate 需要的最小形状：
   - 「数据结构」里的 `Semantic`（`id: NodeId`、`label: Option<Label>`）、`Label`、`State`、`Role`、`Record`。
   - 包装元素 `semantic(spec, child: impl IntoElement)`，以及给带交互的子元素用的 `semantic_interactive<E: InteractiveElement + IntoElement>(spec, child)`（`semantic` 擦掉了子元素类型，写不了无障碍属性）。构造 `Semantic` 的成本（带业务键的 ID 要拼字符串）在所有构建里都存在，由 `ui` 的帧基准覆盖；`ui` 可以提供延迟构造的形式，本 crate 不要求。它在 prepaint 里拿到自己的范围；记录器打开时推一条 `Record`（`bounds` 来自 `Window::layout_bounds`，`visible` 按 `Window::content_mask` 判断，`parent` 是当前栈顶），并在子元素 prepaint 期间把自己压进线程本地的父节点栈；关闭时只读一次原子标志就返回。构造函数带 `#[track_caller]`，记下 `declared_at`。
   - `semantic_interactive` 把同一份 `spec` 写进 GPUI 的无障碍属性：`accessibility_id(id)`、`role(映射后的 accesskit::Role)`、`aria_label(label.text)`（`label` 为 None 时不设）、`aria_selected`、`aria_expanded`、`aria_toggled`。角色映射由 `ui` 给出（如 `ListItem → ListItem`、`DiffHunk → Group`）。`ui` 的控件自带角色、标签（来自 `ui::Text`）和状态，视图只给 `NodeId`。
   - 记录器：`Recorder::begin(window: WindowId)`、`Recorder::finish() -> Vec<Record>`、`Recorder::is_armed() -> bool`（一个 `AtomicBool`），进程里一个，主线程使用；`begin` 时丢弃上一次没取走的记录。
   - 每个视图 crate 的设计文档「界面」一节列出它的语义节点（角色与 ID），这是[模板](TEMPLATE.md)已有的要求；不可交互的纯装饰元素不声明。
2. **`i18n`**：`Key::name(&self) -> &'static str` 与 `FmtKey::name(&self) -> &'static str`，返回 Fluent 的消息 ID（如 `review-looks-good`）；常数时间，读 `table::NAMES`。这张名字表已经编译进每个构建（`Strings::format` 的报告用它），不增加体积。
3. **`app`**：
   - Cargo feature `ui-probe`（不在 `default` 里），打开可选依赖 `lectern-ui-probe`（架构规则 2 允许的形式）。`just check`、`just test` 用 `--all-features`，所以它们编译并检查这条路径；不带 feature 的 `cargo build -p lectern-app` 不含探针。`crates/app/AGENTS.md` 的措辞已随本文改为「只在 `ui-probe` feature 下编译，`just` 的入口打开这个 feature；运行时只在 probe profile 里启动」。
   - `Profile::Probe`（`#[cfg(feature = "ui-probe")]`）：插件集合与 desktop 相同，启动屏障与 desktop 相同，多一个探针。`run` 在首帧之前只读环境变量 `LECTERN_UI_PROBE_DIR`（与 bench 的 `LECTERN_BENCH_SOCKET` 做法相同，不读文件）：设置了就是 `Probe`，否则是 `Desktop`。`startup::release` 的末尾、`Kernel::start` 之后调用 `Probe::start`；失败记 warn `probe start failed`。`Started` 持有 `Option<Probe>`。
   - 退出顺序：`lectern-quit` 线程先 `Probe::shutdown`，再 `Kernel::shutdown`。`quit` 请求经 `Quit` Action，与 ⌘Q 相同。
   - 集成测试 `crates/app/tests/probe.rs`（`#[cfg(feature = "ui-probe")]`，与 `tests/startup.rs` 同一套启动方式）：新建临时目录，`home/` 作 `HOME`、`probe/`（0700）作 `LECTERN_UI_PROBE_DIR`，去掉 `CLAUDE_CONFIG_DIR` 与 `CODEX_HOME`，启动 `CARGO_BIN_EXE_lectern`；用 `client::Client` 轮询连接（最多 10 s）；`hello` 的 `windows` 有一个；`ui_tree` 的根节点角色是 `window`（骨架没有语义节点，`nodes` 长度为 1）；`screenshot` 的文件存在、能解码、尺寸等于 `size × scale`；`quit` 后进程在 30 s 内退出，日志里有 `probe listening`、`probe stopped`，没有临时目录的路径以外的内容字符串（金丝雀放在 `name`）。
   - `justfile` 加 `probe` 配方：`cargo build --locked -p lectern-app --features ui-probe`，`mktemp -d` 建会话目录，按上面的环境变量在后台启动 `lectern`，打印 `export LECTERN_UI_PROBE_DIR=…` 和 pid。agent 之后用 `cargo run -p lectern-ui-probe --bin lectern-probe -- ui_tree` 等命令读取，最后 `lectern-probe quit`。配方不读真实的 `HOME`。
4. **CI**：无新任务。macOS 任务的 `just test` 已带 `--all-features`，会跑 `tests/probe.rs`；它需要图形会话，与 `tests/startup.rs` 相同。

## 未决问题

1. **语义 ID 是否同时作为 macOS 的 `AXIdentifier` 暴露到进程外。** 这让 VoiceOver 和通用的 computer-use 工具能按同一个 ID 定位元素，也意味着内部命名成为对外可见的接口。选项：(a) 暴露；(b) 只在探针里用。默认 (a)，第 1 项按它写。

## 验收

实现分四步，每步一个 PR。

1. **`ui`：`semantic` 模块**（随 `ui` 设计文档的实现 PR，或单独一个 PR）
   - `just test -p lectern-ui` 里 `semantic_*` 测试通过：记录器关闭时不记录；打开时记下范围、`visible`、`parent`；嵌套三层的父子关系正确；`declared_at` 指向调用处。
   - `rg 'accessibility_id' crates/ui/src/semantic.rs` 有结果（无障碍属性由同一份声明写出）。
2. **`i18n`：`Key::name`**
   - `just test -p lectern-i18n` 里 `key_name_is_the_message_id` 通过。
3. **`ui_probe`**（`Refs #6`）
   - `just test -p lectern-ui-probe` 全部通过，含「测试」列出的 insta 快照和 `canary_stays_out_of_probe_logs`。
   - `cargo run -p lectern-ui-probe --bin lectern-probe -- --help` 列出 `hello`、`ui_tree`、`screenshot`、`quit`、`raw`。
   - `rg 'println!|eprintln!' crates/ui_probe/src --glob '!**/bin/*'` 没有结果（库部分只经日志宏）。
   - `crates/ui_probe/AGENTS.md` 的设计文档行指向本文。
4. **`app`：probe profile 与端到端**（`Closes #6`）
   - `just test -p lectern-app` 里 `tests/probe.rs` 通过（macOS）。
   - `cargo build --locked -p lectern-app` 成功；`cargo tree --locked -p lectern-app -e normal,build,features --target aarch64-apple-darwin` 的输出里没有 `lectern-ui-probe`，也没有 `"test-support"`。
   - `cargo build --locked -p lectern-app --features ui-probe` 成功。
   - `just probe` 启动一个会话并打印探针目录；在同一台机器上 `cargo run -p lectern-ui-probe --bin lectern-probe -- screenshot` 返回一个存在的 PNG 路径，解码后不是单一颜色（读回真的发生了），`lectern-probe quit` 后进程退出。`tests/probe.rs` 对 PNG 做同一个检查。
   - `just bench app/` 的 `app/cold_start`、`app/idle_empty` 结果与改动前相比不变（探针不在测量构建里）。

## 实际验证

第 3 步（macOS 27.0.1，Apple 芯片，本机屏幕锁定，没有图形会话）：

- `just check`、`just deny` 通过；`python3 scripts/check_architecture.py` 通过（`ui_probe` 在 tool 层，依赖 `ui`、`logging` 和 GPUI；没有 crate 依赖它）。
- `just test -p lectern-ui-probe`：67 个测试通过，连跑 5 次没有失败。含「测试」列出的单元测试和 `#[gpui::test]`：`tree_*`（9 个，含 `tree_basic`、`tree_warnings` 两份快照）、`check_id_*`、`warnings_*`、`annotate_*`（含 `write_png_*`）、`request_*` 与 `response_lines` 快照、`name_*`、`dir_*`、`server` 的请求处理（`hello`、`ui_tree`、未知方法、`quit` 之后的 `quitting`、`screenshot` 的检查与写文件、行长度上限、连接的顺序与 `shutdown` 结束连接）、`client` 的假探针、`snapshot_of_nested_semantics`（快照）、`records_only_while_armed`、`capture_uses_two_frames`、被取消的捕获、`handle_*`（超时、无窗口、两帧）、`quit_dispatches_the_applications_quit_action_by_name`、`probe_can_be_shut_down_from_another_thread`（`Probe` 是 `Send + Sync`，`app` 的 `lectern-quit` 线程可以调 `shutdown`），以及 `canary_stays_out_of_probe_logs`。
- `a_session_answers_hello_ui_tree_screenshot_and_quit`：在 GPUI 测试平台上真的起一个 `Probe`（真实的 Unix socket，0600，探针目录 0700），另一个线程里的 `client::Client` 依次发 `hello`、`ui_tree`、非法 `name` 的 `screenshot`、`screenshot`、未知方法、`quit`、再一个 `hello`；`ui_tree` 返回 6 个节点（窗口根加 5 个）、标签的 Fluent 键 `common-ok`；`quit` 分发了测试注册的 `lectern::Quit` 一次，之后的请求是 `quitting`；`drop(probe)` 后 `probe.sock` 被删除、不能再连接。测试平台没有渲染器，所以这里的 `screenshot` 应答 `render`。
- `cargo run -p lectern-ui-probe --bin lectern-probe -- --help` 列出 `hello`、`ui_tree`、`screenshot`、`quit`、`raw`；没有探针目录时退出码 64。
- `rg 'println!|eprintln!' crates/ui_probe/src --glob '!**/bin/*'` 没有结果（验收里原来的 `'!bin/*'` 在 `rg` 里不排除 `src/bin/`，已改成 `'!**/bin/*'`）。
- `git diff origin/main -- Cargo.lock | rg '^\+name = '` 没有结果：没有新的 package。`cargo tree --locked -p lectern-app -e normal,build,features --target aarch64-apple-darwin` 的输出里没有 `lectern-ui-probe`，也没有 `"test-support"`（现在还没有 crate 依赖本 crate；第 4 步再检查一次）。

没有验证（本机锁屏，或属于第 4 步）：

- 真实窗口上的 `render_to_image`：Metal 读回、PNG 不是单一颜色、`framebufferOnly` 在 debug 构建里的行为、耗时（设计估计 10 ms 量级）、release 构建里 `screenshot` 应答 `render`。
- 真实帧循环里两个 `on_next_frame` 回调与 `draw` 的先后（只在测试平台的 `simulate_next_frame` 里验证）；窗口不在前台或被遮挡时的帧率与 `frame_timeout`。
- `quit` 经 `app` 真实的 `Quit` Action 退出整个进程；`Quit` 的名字 `lectern::Quit` 只对照了 `app/src/quit.rs` 的 `actions!(lectern, [Quit])`，没有在 `app` 里跑过。
- `LECTERN_UI_PROBE_DIR`、`Profile::Probe`、`justfile` 的 `probe` 配方、`tests/probe.rs`、`just bench app/` 不变：第 4 步。

## 与设计的差异

第 3 步：

- 窗口 ID 是 GPUI 的 `WindowId::as_u64()`（`hello` 的 `windows[].id`、`ui_tree` 的 `window.id` 和 `window` 参数相同，例如 `4294967297`），不是示例里的 1；根节点的 ID 是 `window[<这个值>]`。理由：它在窗口的生命周期里稳定，不需要另维护一张表；示例里的 1 只是形状。`window` 参数指定的窗口不存在时应答 `no-window`。`hello` 的窗口条目多一个 `key`（是否为活动窗口）。
- `capture` 没有 `cx` 参数（`Window::on_next_frame` 只要 `&self`），返回 `Cancel`。设计里「迟到的记录丢弃」落在这里：`frame_timeout` 到时，主线程调 `Cancel::cancel`，两个回调之后什么也不做、已打开的记录器被关闭；原先设想的「超时后记录器一直开着，直到第二个回调才关」因此不存在。
- 不依赖 `lectern-i18n`（只作 dev 依赖）：`lectern_ui::semantic::Label::key` 已经是 Fluent 消息 ID，`ui` 在构造 `Label` 时调过 `Key::name`，本 crate 直接写出。
- 请求队列用 `futures::channel::mpsc::unbounded`，应答用标准库的 `mpsc::channel`，没有引入 `async_channel`（只为一个队列把它列为工作区依赖不值得；`workspace` 已经是这个形式）。工作区新增的声明只有 `image`（见「依赖」）。
- `client::Client::call` 返回的是 `client::Reply`（`Deserialize`），不是 `protocol::Response`：后者的 `error.message` 是 `&'static str`，只能序列化。`Client::raw` 另给命令行的 `raw` 命令用。
- `quit` 不经主线程取应答：连接线程自己应答 `{}`，写出后再往队列放一个 `Quit` 任务，主线程在前台任务里按名字 `lectern::Quit`（常量 `server::QUIT_ACTION`，对应 `app` 的 `actions!(lectern, [Quit])`）`build_action` 并 `dispatch_action`。`app` 改了 Action 的名字，`quit` 就退不出去：记 warn `probe quit failed`。收到 `quit` 之后到来的所有请求（包括未知方法）都应答 `quitting`。
- 多出两条日志：warn `probe client rejected`（连接线程建不起来，字段 `conn`、`io`）、warn `probe quit failed`（上一条）。`probe request failed` 的 `io` 字段只在写文件失败时有值，`req` 由作用域给出，不作字段。
- `shutdown` 不靠连接线程的读超时观察停止标志（那样空闲连接每 500 ms 醒一次，与「没有定时器」相抵）：它对每个打开的连接 `shutdown(Both)`，阻塞在读上的线程立刻读到结束。
- `tree::warnings` 是单独的纯函数：`NodeId` 在调试构建里拒绝不合语法的 ID，测试里造不出 `bad-id` 的记录，所以语法与重复检查拆出来直接测。`bad-id` 对同一个 ID 只报一次。
- `Tree::to_json` 是缩进的（快照和阅读用），线上用 `serde_json::to_value`（紧凑）。长度保留两位小数（`f32` 直接转 `f64` 会写成 `0.30000001192`）。
- 快照放在 `src/tests/snapshots/`（测试在 `src/tests/`），不是设计里写的 `src/snapshots/`。
- `#[gpui::test]` 里 GPUI 的测试上下文在一次 `update` 结束时会画脏窗口，所以 `capture` 第一个回调（`refresh`）之后不需要、也不能再手动 `draw`（那样记录会出现两遍）；整条会话的测试要 `cx.executor().allow_parking()`，因为连接线程从自己的线程唤醒前台任务。

`app` 的接入（第 4 步，本 PR 不含）：`Cargo.toml` 的可选依赖与 `ui-probe` feature、`Profile::Probe`、`startup::release` 末尾的 `Probe::start(cx, Config::new(dir))`（失败记 warn `probe start failed`，`reason` 取 `StartError::reason()`，`error` 取 `StartError::io()`）、`quit` 线程里先 `Probe::shutdown`、`tests/probe.rs`、`justfile` 的 `probe` 配方，见「对其他 crate 的要求」第 3 项。
