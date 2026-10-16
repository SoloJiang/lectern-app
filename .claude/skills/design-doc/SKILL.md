---
name: design-doc
description: Write the module design doc docs/design/<crate>.md for a Lectern crate before any code, from docs/design/TEMPLATE.md, and open it as a docs-only PR for human review. Use when starting an issue that adds or changes a crate's interface or data structures, or when a crate's AGENTS.md says its design doc is not written yet.
---

# 写模块设计文档

Lectern 先写设计、设计文档 PR 合并后再写代码（AGENTS.md「工作方式」）。产出是一份 `docs/design/<crate>.md`，以及一个只改文档的 PR：其中包含这份设计文档和第 4 步对 `crates/<crate>/AGENTS.md` 的更新。

## 步骤

1. 新建 worktree 和分支，并进入新的 worktree：`git fetch origin main && git worktree add -b design/<crate> ../lectern-design-<crate> origin/main && cd ../lectern-design-<crate>`。后面的步骤都在这个目录里执行：上下文从最新的 main 读，文档也写在这里，不在原来的 checkout 里。
2. 在新的 worktree 里读上下文，按顺序：
   - 根目录 `AGENTS.md`，以及 `crates/<crate>/AGENTS.md`（职责、边界、层）。
   - 对应的 issue（`gh api repos/SoloJiang/lectern-app/issues/<n>`）。
   - [交互规范](../../../docs/design/interaction.md)和[技术调研](../../../docs/design/tech-research.md)里相关的章节，以及 crate AGENTS.md 列出的 ADR。
   - 依赖的其他 crate 的设计文档（已经写了的）。
3. `docs/design/<crate>.md` 不存在时，复制[模板](../../../docs/design/TEMPLATE.md)为这个文件（<crate> 用目录名），按小节填写，删掉不适用的小节和模板开头的注释。已经存在时直接修改现有文档，不要用模板覆盖。
   - 接口写 Rust 签名，标明调用线程和是否阻塞。
   - 解析外部格式的，写明格式来源和版本，并说明 fixtures 放什么。
   - 列出受影响的性能预算和需要的基准。
   - 需要产品决定的问题（定位、目标用户、界面结构的大改）放进「未决问题」，写明选项和默认选项；技术问题自己决定。改变架构、依赖选型或性能预算的，另写 ADR（技能 `adr`）。
4. 更新 `crates/<crate>/AGENTS.md` 的「设计文档」一行，改成指向新文档的链接。
5. 提 PR：标题 `docs(<crate>): design`，正文写 `Refs #<n>`，列出未决问题。设计文档 PR 按合并规则处理：等 Codex 的 👍、CI 通过、讨论串全部解决后合并（技能 `ship-pr`）。需要产品决定的未决问题同时告诉人。设计文档可以带着未决问题合并，但涉及这些问题的实现要等人答复后再做；技术问题按文档里的默认选项实现。
6. 合并后再按设计写代码：先 `git fetch origin main`，从更新后的 `origin/main` 另开 worktree、分支和 PR（这样才包含刚合并的设计文档和 crate AGENTS.md），与设计文档 PR 共用同一个 issue（设计 PR 写 `Refs #<n>`，实现 PR 写 `Closes #<n>`；issue 涉及多个 crate 时，只有最后完成的实现 PR 写 `Closes`）。实现中发现设计要变，先改文档。

## 检查

- 文档里的链接都能打开（相对路径从 `docs/design/` 起算）。
- 没有与 AGENTS.md 架构规则冲突的设计：领域层不碰 GPUI、视图不做 IO、派生数据以 blob OID 为键、macOS 专属能力只在 `platform`。
- 视图层的文档写了界面字符串的 Fluent 键和 UI 探针的语义节点。
