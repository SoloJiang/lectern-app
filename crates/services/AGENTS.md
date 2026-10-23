# services（`lectern-services`）

插件实现和依赖的服务接口：`SessionSource`、`ForgeProvider`、`NavigationProvider`、`ProjectDetector` 等。插件之间只经这些接口交互，不依赖彼此（架构规则 7、[ADR 0025](../../docs/adr/0025.md)）。

- 层：kernel 层。只依赖 kernel 层，不依赖 GPUI；任何层都可以依赖本 crate。
- 设计文档：`docs/design/services.md`，尚未编写。先按[模板](../../docs/design/TEMPLATE.md)编写，设计文档 PR 合并后再写代码。
- `project` 模块（`dyn Projects`、项目快照与 `ProjectEvent`）的接口在 [project 设计](../../docs/design/project.md)「接口」「数据结构」里定义，由 `project` 实现。
- `session` 模块（`dyn SessionSource`、会话列表、记录、轮次索引与 `SessionsChanged`）的接口在 [agents 设计](../../docs/design/agents.md)「接口」「数据结构」里定义，由 `agents` 实现。
- `code_intel` 模块（`dyn CodeIntel`、查询与结果、名字表、`CodeIntelEvent`，以及语义导航的 `NavigationProvider`）的接口在 [code_intel 设计](../../docs/design/code_intel.md)「接口」里定义，由 `code_intel` 实现；`NavigationProvider` 由语义导航的插件（#26）实现。`project` 模块的 `BlobId` 和 `FileList::oid` 也是 code_intel 设计「对其他 crate 的要求」加的。
- `reader` 模块（`dyn Reader`、`ReadTarget`、`Anchor`、`ObjectShare` 与 `ReaderEvent`）的接口在 [file_reader 设计](../../docs/design/file_reader.md)「接口」里定义，由 `file_reader` 实现。`project` 模块的 `DirTable`、`DirId`、`finder_cmp`、`ActiveTree::dirs`、`ChangedParts::dirs` 和 `code_intel` 模块的 `DEPENDENCY_EXTENSIONS` 是 file_reader 设计「对其他 crate 的要求」加的；目录表由 `project` 建。
- `changes` 模块（改动来源 `ChangeSource`、`ChangeList` 及其数据类型）和 `review` 模块（`dyn Review`、审阅快照、命令、`ReviewEvent`，以及经 `Review::register_brief_rules` 登记的简报规则提供者 `BriefRules`，不是 kernel 服务）的接口在 [review 设计](../../docs/design/review.md)「接口」里定义：`ChangeSource` 由 `git`（本地 worktree）和 `forge`（MR，#25）实现，也不是 kernel 服务；`Review` 由 `review` 实现。
- `usage` 模块（`dyn Usage`、用量快照与查询、额度快照、项目合并的命令、Analyze 计量的 `begin_analysis` / `meter`、`UsageEvent`，以及经 `Usage::register_mr_source` 登记的 MR 来源 `MrSource`，不是 kernel 服务）的接口在 [usage 设计](../../docs/design/usage.md)「接口」里定义：`Usage` 由 `usage` 实现，`MrSource` 由 `forge`（#25）实现。
- `workspace` 模块（`dyn Contributions`、贡献 `Contribution` 与它的种类、布局 `ScreenLayout`、侧栏条目、导航命令 `WorkspaceCommand`、`Route` 与 `Target`、`WorkspaceEvent`、里程碑，以及经 `Contributions::register_leave_guard` 登记的离开守卫 `LeaveGuard`，不是 kernel 服务）的接口在 [workspace 设计](../../docs/design/workspace.md)「服务接口」里定义，由 `workspace` 实现；`Target` 复用 `project`、`review`、`reader` 模块的键和目标。
- issue：[#8](https://github.com/SoloJiang/lectern-app/issues/8)、[#17](https://github.com/SoloJiang/lectern-app/issues/17)（`code_intel` 模块）、[#10](https://github.com/SoloJiang/lectern-app/issues/10)（`reader` 模块、目录表）、[#15](https://github.com/SoloJiang/lectern-app/issues/15)（`changes`、`review` 模块）、[#31](https://github.com/SoloJiang/lectern-app/issues/31)（`usage` 模块）、[#215](https://github.com/SoloJiang/lectern-app/issues/215)（`workspace` 模块）
- ADR：[0010](../../docs/adr/0010.md)、[0018](../../docs/adr/0018.md)、[0019](../../docs/adr/0019.md)、[0020](../../docs/adr/0020.md)、[0022](../../docs/adr/0022.md)（`usage` 模块）

## 边界

- 只有 trait、数据类型和服务 ID，没有实现。例外：`project` 模块的 `FileList` 带构造器和二分查找，`BlobId` 带字节与十六进制的转换（project 设计「文件列表」），`DirTable` 带构造器（`new`，反序列化也经它重建）和查找，`finder_cmp` 是纯函数（file_reader 设计「目录表」）；`reader` 模块的 `ObjectShare` 包着实现方传入的注销回调（`new`；drop 时调用），回调的内容属于 `file_reader`；`code_intel` 模块的 `NameTable` 带构造器（`NameTableBuilder`）和查找，`SemanticReply` 与 `ProviderRegistration` 包着实现方传入的回调（`new`；`send` 调用回调，登记 drop 时调用回调），回调的内容属于 `code_intel`；`session` 模块的 `Cursor` 带外层编码（`to_bytes` / `from_bytes`）和供提供者使用的构造器，`ContentRef` 带构造器，`SessionList` 带 `get` / `in_project`（agents 设计「数据结构」）；`changes` 模块的 `ChangePath` 带 `as_str`，`ChangeList::handle` 是实现方放进去的不透明句柄（不序列化，比较时总是相等）；`review` 模块的 `BriefReply`、`BriefRulesRegistration` 与 `OverviewInterest` 包着实现方传入的回调（review 设计「接口」）。`usage` 模块的 `Tokens::total` 求四类之和，`UsageInterest` 与 `MrSourceRegistration` 包着实现方传入的注销回调（usage 设计「接口」）。`workspace` 模块的 `Registration` 包着实现方传入的提交回调（`kernel::Staged`）和注销回调，`Payload` 与 `SharedPayload` 是类型擦除的载荷（`Box<dyn Any>` / `Arc<dyn Any>`），内容由 `ui::contrib` 定义、`workspace` 取回（workspace 设计「服务接口」）。它们都是数据结构自身的方法。
- 同一组接口同时用于内置插件和独立进程插件（[ADR 0018](../../docs/adr/0018.md)），类型要能通过 JSON-RPC 序列化。trait 和包着回调的 `SemanticReply`、`ProviderRegistration`、`ObjectShare`、`BriefReply`、`BriefRulesRegistration`、`OverviewInterest`、`UsageInterest`、`MrSourceRegistration`、`Registration`，以及载荷 `Payload`、`SharedPayload` 和含它们的 `Contribution`、`SidebarEntry` 不序列化：进程插件的代理在宿主进程里持有它们（进程插件不贡献界面，workspace 设计「范围」）。
- 接口变动会影响所有实现它的插件，改动前在设计文档里列出受影响的 crate。
- 改动来源的接口（ADR 0019）也定义在这里，按能力划分：取 diff 是必需的；快速判断是否过期的指纹、加载完整文件、读写审阅评论、读取共享分析结果都是可选能力。本地 git 由 `git` 实现，GitHub / GitLab 由 `forge` 实现，视图按来源具备的能力决定显示哪些功能。

## 测试

不依赖 GPUI，在任何平台上都能编译和测试：

```sh
cargo check -p lectern-services     # 只改本 crate 时最快的反馈
just test -p lectern-services
```

- 单元测试写在模块旁的 `#[cfg(test)] mod tests`；结构化输出（解析结果、归因、diff）用 insta 快照验收，快照改动要在 PR 里逐个看过。
- 真实数据放 `tests/fixtures/`，先脱敏；格式随上游版本变化的，每个版本留一份样本。
