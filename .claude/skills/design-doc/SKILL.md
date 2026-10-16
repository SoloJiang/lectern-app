---
name: design-doc
description: Write the module design doc docs/design/<crate>.md for a Lectern crate before any code, from docs/design/TEMPLATE.md, and open it as a docs-only PR for human review. Use when starting an issue that adds or changes a crate's interface or data structures, or when a crate's AGENTS.md says its design doc is not written yet.
---

# 写模块设计文档

Lectern 的规则是先写设计、人审过再写代码（AGENTS.md「工作方式」）。本技能产出一份 `docs/design/<crate>.md` 和一个只含这份文档的 PR。

## 步骤

1. 读上下文，按顺序：
   - 根目录 `AGENTS.md`，以及 `crates/<crate>/AGENTS.md`（职责、边界、层）。
   - 对应的 issue（`gh api repos/SoloJiang/lectern-app/issues/<n>`）。
   - [交互规范](../../../docs/design/interaction.md)和[技术调研](../../../docs/design/tech-research.md)里相关的章节，以及 crate AGENTS.md 列出的 ADR。
   - 依赖的其他 crate 的设计文档（已经写了的）。
2. 新建 worktree 和分支：`git worktree add -b design/<crate> ../lectern-design-<crate> origin/main`。
3. 复制 [模板](../../../docs/design/TEMPLATE.md) 为 `docs/design/<crate>.md`，<crate> 用目录名。按小节填写，删掉不适用的小节和模板开头的注释。
   - 接口写 Rust 签名，标明调用线程和是否阻塞。
   - 解析外部格式的，写明格式来源和版本，并说明 fixtures 放什么。
   - 列出受影响的性能预算和需要的基准。
   - 拿不准、需要人决定的放进「未决问题」，不要自己定。改变架构、依赖选型或性能预算的，另写 ADR（技能 `adr`）。
4. 更新 `crates/<crate>/AGENTS.md` 的「设计文档」一行，改成指向新文档的链接。
5. 提 PR：标题 `docs(<crate>): design`，正文写 `Refs #<n>`，列出未决问题。只改文档的 PR 不需要 Codex 审阅，等人审。
6. 人审通过并合并后，再按设计写代码；实现中发现设计要变，先改文档。

## 检查

- 文档里的链接都能打开（相对路径从 `docs/design/` 起算）。
- 没有与 AGENTS.md 架构规则冲突的设计：领域层不碰 GPUI、视图不做 IO、派生数据以 blob OID 为键、macOS 专属能力只在 `platform`。
- 视图层的文档写了界面字符串的 Fluent 键和 UI 探针的语义节点。
