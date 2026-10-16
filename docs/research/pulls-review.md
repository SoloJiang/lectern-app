# pulls.review 调研

调研对象是 [antfu/pulls.review](https://github.com/antfu/pulls.review) v0.3.3（提交 `974853a`，2026-10-13），MIT 许可。调研只读了源码、设计文档和提交历史，没有安装依赖或运行代码。文中路径都相对于该仓库根目录。

pulls.review 是一个审阅 GitHub PR 的网页应用。它把 PR 里的文件按意图分组，每组附一段摘要，审阅者按组阅读。分组可以用内置的路径规则生成，也可以调用模型生成。它没有后端：GitHub token 和模型 API key 由用户提供，存在浏览器里。

## 结论

1. **Lectern 不需要把模型分组当作主视图。** pulls.review 用模型推断「这些改动是为了什么」，Lectern 从会话日志直接读到 prompt 和 agent 的说明，意图信息是现成的。pulls.review 的提示词要求「按最终改动分组，不按提交分组」（`packages/core/src/analyze/adapters/llm/prompt.ts`），这和 Lectern 按轮次阅读的方向相反。
2. **会话简报可以采用它的分析流程。** 简报要列出「值得先看的几处」，这正是 pulls.review 的 `critical` 标记、文件备注和行备注所做的事。它的校验流程（结构校验、覆盖检查、一次纠错重试、按实际 diff 对账、未分类兜底）可以直接用于 Lectern 的任何模型输出。
3. **已看状态按内容记录，并标出「审过之后又被改过」的文件。** pulls.review 只用文件改动后的 blob OID 做键。Lectern 的键要包含审阅对象、路径和改动前后两个 blob OID，否则两处结果相同、来源不同的改动会共用一个已看标记。这在 agent 跨多轮修改同一文件时用处最大，应写进 `review` 的设计。
4. **以只读模式按需调用本地的 Claude Code 和 Codex。** pulls.review 的本地版本会启动用户已登录的 agent 命令行，用只读参数运行，结束即退出。这符合架构规则 3，也不需要 Lectern 保存模型 key。
5. **本地取 diff 的细节值得照抄**：包含未跟踪文件但不改用户的 index、单文件大小上限、工作区指纹。
6. **工程实践上有四处可借鉴**：设计文档的状态行和「完成标准」、lint 报错指向规则出处、用假的可执行文件测试外部进程、AGENTS.md 写明实现与设计不一致时怎么处理。

## 项目概况

pulls.review 是 pnpm workspace，包含四个包：

| 包 | 内容 |
|---|---|
| `packages/app` | Vite + Vue 单页应用，同时构建 github.com 上的嵌入面板 |
| `packages/core`（`@pulls.review/core`） | 与运行环境无关的核心：类型、patch 解析、GitHub 客户端、缓存、分析、本地 git |
| `packages/actions`（`@pulls.review/actions`） | 在 CI 里分析 PR 并发评论的命令行工具 |
| `packages/cli`（`pulls.review`） | 本地审阅服务器，基于 devframe |

数据流是 `DiffSource → AnalyzeAdapter → Cache → View`（`.agents/01-architecture.md`）。视图组件只接收一个 `DiffsStore`，不访问存储和网络。核心包必须能在纯 Node 下加载，不得引用 `localStorage`、`document`、`import.meta.env` 和 vue-i18n。界面文字由宿主注入，进度用结构化事件，错误用带稳定编码的诊断信息，由应用按编码翻译。

开发主要由 coding agent 完成。2026-09-16 到 10-13 共 184 个提交，其中 53 个来自维护者的 bot 账号，都以 squash 方式合并。

## 审阅界面

### 分组

分组的结构定义在 `packages/core/src/types/analyze.ts`：

- `key`、`label`、`summary`：标识、不超过四个词的标题、一到三句的摘要（写「为什么」而不是「改了什么」）。
- `category`：16 个取值，描述改动位于系统的哪一部分（ui、api、core、data、tests、docs、deps 等），不描述改动类型。
- `filePaths`：组内文件。
- `critical`：可选布尔值，含义是「涉及安全、可能丢数据、难以回退或容易出错」，提示词要求少用。
- `fileNotes`、`lineNotes`：可选的文件备注和行备注，行备注带 `side` 和行号。
- `children`：最多一层子分组，深度由类型本身限制。

所有分组排在同一个长页面里，用虚拟滚动渲染。分组导航是页头的标签或左侧的树，点击后滚动到对应分组，滚动时高亮当前分组。项目介绍里「一次读一组」的实际效果是跳转到分段。

### Diff 渲染

- 渲染用 `@pierre/diffs`，高亮用 Shiki，支持统一视图和双栏视图。容器宽度小于 640px 或文件只有新增或只有删除时，强制用统一视图（`packages/app/src/components/diff/FileDiff.vue`）。
- diff 距离视口 600px 时自动拉取完整文件再高亮。只高亮单个 hunk 时，如果 hunk 从字符串或注释中间开始，颜色会错。
- 生成文件和 lockfile 用同一份模式列表 `GENERATED_PATTERNS`（`packages/core/src/analyze/adapters/rule-based/rules.ts`）识别。这份列表同时决定规则分组、默认折叠，以及在模型输入里只留一行占位。
- 二进制文件显示「Binary file not shown」。超过 512 KB 的 patch 只保留文件头和增删行数，界面上可以手动加载全文。
- 没有忽略空白的选项，也不识别移动的代码块。

### 已看状态

- 已看标记的键是文件的 blob sha（`review:{sha}`，`packages/core/src/cache/review-marks.ts`），不是路径。文件内容不变时，新提交不会清掉标记。
- 审过之后内容又变了的文件，记在 `changedSinceReviewed` 里，界面显示橙色提示「previously reviewed but changed in later commits」和重新标记按钮。复选框有三种状态：已看、审后又改、未看。
- 标为已看的文件自动折叠。文件树里的目录复选框可以一次标记整个目录。
- 页头显示「X / N reviewed」，点开是全部文件的状态列表，可以跳转、全部标记、反选和重置。

### 评论与对话

- GitHub 上已有的审阅讨论串显示在对应行下方，已解决和已过期的讨论串隐藏，只显示数量。
- 可以单行或多行评论、回复、编辑、删除、解决讨论串，最后以 Comment、Approve 或 Request changes 提交。
- 对话窗口接着分析时的模型会话继续，可以提问，也可以通过 `update_grouping` 工具重新分组。重新分组没有撤销。

### 缺少的功能

- 没有全局快捷键，只有评论框里的 ⌘↩ 和 Esc。没有逐文件跳转，也没有「标记已看并跳到下一个」。
- 不显示模型调用的 token 和费用，成本在代码里写成 0。
- 本地版本一次只看一个仓库的一个目标（分支、比较、工作区、提交），没有多 worktree 总览。

## 分析流程

### 规则分组和模型分组

两种分组是独立的适配器，用户在页头切换。

- **规则分组**：每次加载都计算，不缓存。按顺序匹配 glob，第一个命中的规则生效：docs、tests、config、deps、generated，其余代码文件归入 code。规则的顺序也是分组的显示顺序。规则不识别 monorepo 子项目。
- **模型分组**：只在用户点击后运行，代码注释写明「付费或耗时的调用必须由用户显式点击」（`packages/app/src/stores/diffs-store.ts`）。模型失败时报错，保留之前的结果或规则分组，不会把规则分组的结果存成模型结果。

### 模型的输入

用户消息由 `buildAnalysisPrompt` 生成（`prompt.ts`），包含：

- PR 标题、链接、完整描述，多于一个提交时附上提交标题。
- 文件清单：按目录分组，每个文件一行，格式是 `文件名  状态字母  +增/-删  @@ 上下文1 / @@ 上下文2 / @@ 上下文3`。上下文取自 hunk 头里的函数名，最多三个。生成文件和二进制文件带 `[generated]`、`[binary]` 标签。
- 全部 diff，仅当总长度不超过 200,000 字符时附上。超过时模型通过 `read_diffs` 工具按需读取。
- 最后一行是 `Respond and categorize in <language>.`，其余部分保持英文，方便命中提示词缓存。

系统提示词的要点：按意图分组而不是按目录；一般 1 到 5 组；超过 15 个文件才用子分组；测试单独一组；按审阅优先级排序，核心改动在前，lockfile、生成文件、格式改动在后；每个路径必须恰好出现在一个分组里。

各字段的写作要求写在 valibot schema 的字段描述里，再转换成 `submit_grouping` 工具的 JSON Schema（`tools.ts`）。schema 本身就是模型看到的格式说明。

### 调用的上限

上限都按字符计，不按 token 计：

| 上限 | 值 |
|---|---|
| 内联 diff | 200,000 字符 |
| 每次 `read_diffs` 返回 | 40,000 字符，超出的路径提示「再次请求」 |
| 读取总量 | 超过 200,000 字符或到第 10 轮时，注入「读取预算已用完，现在提交」 |
| 模型只回文字、没调用工具 | 提醒一次「Call submit_grouping now.」 |
| 最大轮数 | 12 轮，超过即失败 |

代码里没有设置 temperature 或 seed，结果可重复只靠缓存。

### 校验与对账

模型提交的分组依次经过：

1. **结构校验**：valibot 校验 schema。
2. **覆盖检查**：`findCoverageIssues` 列出缺失、重复和不存在的路径。第一次不通过时作为工具错误返回给模型，要求修正后重新提交；第二次仍不通过时接受结果。
3. **对账**：`reconcile` 删除不存在的路径和重复路径（第一个分组保留），删除指向组外文件的备注，删除空分组。
4. **显示时再对账**：`resolveGroups`（`packages/app/src/components/diff/group-utils.ts`）用旧路径匹配重命名的文件，已消失的文件标为 missing，没被任何分组包含的文件放进末尾的 Uncategorized 分组。行备注只在 diff 里确实显示了该行时才挂到行上，否则降为文件备注。

这套流程保证模型出错时不会有文件从界面上消失。

### 调用本地 agent 命令行

本地版本可以把分析交给用户已登录的 agent 命令行（`packages/cli/src/agents/`，`plans/11-local-agents.md`）。每个适配器启动一个无界面进程，prompt 从标准输入传入，输出按 JSON 行解析：

| agent | 只读参数 |
|---|---|
| Claude Code | `claude -p --output-format stream-json --permission-mode dontAsk --allowedTools Read Grep Glob --disallowedTools Bash Edit Write … --json-schema <schema>` |
| Codex | `codex exec --json --sandbox read-only -c approval_policy="never"` |
| opencode | `opencode run --format json --agent plan` |
| pi | `pi --tools read,grep,find,ls` |

- diff 写到一个 patch 文件里，系统提示词要求 agent 读这个文件，只在本地目标时打开仓库文件，并且「不修改任何东西」。
- 分析 GitHub PR 时，agent 在一个空的临时目录里运行，看不到本地仓库。
- 覆盖检查不通过时，在同一个会话里续接一次并附上修正要求，第二次不通过即报错。
- 对话通过 agent 自己的会话 ID 续接。会话丢失时返回 `agent-session-lost`。
- 不传任何凭据，agent 使用用户 shell 里已有的登录状态。
- 没有轮数和时间上限，由 agent 自己控制，用户可以手动停止。

### 测试

- 模型循环用 pi-ai 的 `fauxProvider()` 按脚本回复，不调用真实接口。覆盖了读取后提交、覆盖错误后重交、第 10 轮提醒、12 轮失败、中止和进度事件。
- 三个真实 PR 作为夹具，对完整提示词和规则分组结果做快照。
- 合成夹具覆盖二进制、超大文件、重命名、过期分析、零文件等情况，同时驱动 Storybook。
- 本地 git 来源的测试在临时目录里建真实仓库。
- agent 适配器的测试用 `packages/cli/test/fake-agent.mjs`：一个放在 PATH 上的假命令，记录参数、标准输入和工作目录，回放 JSON 行夹具。

## 数据来源与缓存

### 按能力定义的来源接口

`DiffSource`（`packages/core/src/types/source.ts`）只有 `key` 和 `fetch` 两个必需成员，其余都是可选能力：`fingerprint`（快速判断是否过期）、`loadFile`（加载完整文件）、`viewer`、`reviews`（读写审阅评论）、`sharing`（共享分析结果）、`auth`。界面按来源具备哪些能力决定显示哪些功能，不判断来源的种类。实现有 GitHub PR、比较、提交，粘贴的 patch，本地 git。`serializeRef` 是生成缓存键的唯一入口。

### GitHub 来源

- 一次 `fetch` 并行请求 PR 信息、文件列表（分页，GitHub 上限 3000 个文件）和提交列表（上限 250 个）。
- 文件没有 `patch` 字段时，拉一次完整的 `.diff` 重新解析；GitHub 对超大 PR 返回 406 时，取两个版本的 blob 在本地计算 diff。
- `fingerprint` 就是 `head.sha`。base 分支前进而 PR 没有新提交时，head 不变，但合并基点和显示的改动可能已经变了，这种情况下缓存的分析仍被当作最新。
- 没有 ETag、条件请求、重试和退避，遇到限流直接报错。

### 本地 git 来源

`packages/core/src/local/source.ts` 和 `git.ts`：

- 调用 `git` 不经过 shell，设置 `LC_ALL=C`，diff 参数固定：`-c core.quotePath=false diff --no-color --no-ext-diff --src-prefix=a/ --dst-prefix=b/ --full-index -M`。`--full-index` 输出完整的 blob OID（SHA-1 仓库 40 位，SHA-256 仓库 64 位），所以哈希算法相同时，本地和 GitHub 上的已看标记可以互相匹配。参数里没有 `--no-textconv`：仓库为某类文件配置了 textconv 时，`git diff` 仍会执行那个外部命令。
- 包含未跟踪文件的方法是：用 `git rev-parse --git-path index` 找到当前 worktree 的 index（linked worktree 的 index 不在 `.git/index`），复制到临时文件，复制失败时（如还没有提交的新仓库）从空 index 开始；再通过 `GIT_INDEX_FILE` 对副本执行 `git add --intent-to-add`，然后做 diff。用户的 index 不受影响。
- 单个文件的 patch 超过 512 KB 时，只保留文件头和增删行数，标记 `truncated`。
- 工作区的指纹是 `worktree:` 加上 sha256(基准 + diff 文本)，任何编辑或 HEAD 移动都会改变它。
- `loadFile` 拒绝解析到仓库外的路径。
- 本地缓存放在 `git rev-parse --git-common-dir` 下的 `pulls-review` 目录，同一仓库的所有 worktree 共用。

### 缓存

- 浏览器端用 IndexedDB，每条记录拆成元数据和正文两部分，列表和淘汰只读元数据。按 LRU 淘汰，上限 200 条、200 MB。同一个键的写入串行执行。
- 只缓存付费结果（模型分组），规则分组每次重新计算。
- 新提交到来时保留旧的模型结果，用 `resolveGroups` 对账，不作废。
- 打开时先显示缓存，再比较 `fingerprint`，不一致时显示「有新提交」提示，可选下次自动刷新。

## 共享分析评论

pulls.review 把模型分组结果写成 PR 评论，其他审阅者打开时直接加载，不必再调用模型。

格式（`packages/core/src/providers/github/shared-analysis-comment.ts`）：

- 第一行是标记 `<!-- pulls.review data -->`。
- 一个固定指向 `https://pulls.review/gh/{owner}/{repo}/{n}?from={login}` 的链接。
- 一个 `<details>` 块，写明模型、时间和 head sha 前七位，里面是 JSON 代码块 `{ headSha, result }`。
- 评论超过 60,000 字符（GitHub 上限 65,536）时，改用 lz-string 压缩后的 base64；仍然超长则报 `commentTooLarge`。
- 解析失败的评论直接忽略，不报错。
- 格式本身没有版本号，靠结果里的 `schemaVersion` 和 schema 校验兼容旧数据，新字段都是可选的。

写入和读取：

- 每个作者只有一条评论，有就更新，没有就新建。
- GitHub Action 发现该作者的评论已经对应当前 head sha 时，直接退出，不调用模型。
- 读取只请求评论列表的第一页（100 条）。head sha 不一致的结果标为「outdated」，仍然可以加载。
- 对话记录不共享。

Action 推荐用 `pull_request_target` 触发，理由是它只通过 API 读 diff，从不检出 PR 的代码，所以带密钥运行是安全的，fork 来的 PR 也能收到评论。仓库自己的工作流检出并构建的是默认分支，权限只有 `contents: read` 和 `pull-requests: write`，每个 PR 一个并发组。文档没有讨论两个风险：diff 里的文字会进入模型，模型输出会被公开发布，存在提示词注入；Action 默认运行 npm 上的 `@latest`。

## 开发方式

### Agent 上下文

- `CLAUDE.md` 是指向 `AGENTS.md` 的软链接，`.claude` 是指向 `.agents` 的软链接，各家 agent 读同一份内容。
- `AGENTS.md` 约 30 行，只做索引，并写明优先级：`.agents/` 下的文档「是地图，不是标准：两者不一致时以 `src/` 和测试为准，并修正文档」。MUST、SHOULD 只用于真正的约束（层次边界、接口契约、输出格式），不用于代码风格。
- `.agents/01-architecture.md`（397 行）是长期有效的架构契约。
- 尽管有「修正文档」的规定，文档仍然和代码不一致：架构文档的「暂不做」列表说 devframe 命令行「尚未实现」，而同一文档的其他部分和 plan 09 说已经实现；plan 01 链接到两个不存在的文件；plan 06 说测试和它覆盖的功能放在一组，实际提示词要求测试单独一组。

### 计划文档

`plans/` 下有 11 份编号的计划，共 1,991 行，对应 Lectern 的 `docs/design/<crate>.md`。

- 每份开头有状态行，例如 `**built** (#34-#38, #43-#45)`、`**in progress**. Depends on Plan 04`。
- 常见章节是 Why、Scope、Design、Testing、Out of scope，Out of scope 每份都有。
- plan 10 把工作拆成步骤，每步一个 PR，每步有可以 grep 验证的「Done when」，例如「`'https://api.github.com'` 只出现一次」。
- plan 11 在状态行后写明「实际验证过什么」，包括没验证到的部分：「沙箱里访问不到 Claude Code 的接口，夹具按格式手写」。
- plan 09 在实现后补了「实现与计划的差异」一节。
- 大的改动先提一个只含计划的 PR，再按步骤逐个实现。实现 PR 在同一个提交里更新计划和架构文档。计划里引用 PR 编号，提交信息不引用计划。

### Lint 与测试

- ESLint 开启 `antislop`：限制注释长度、禁止破折号和行话、禁止只包一层的函数和类型别名、禁止注释掉的代码、限制认知复杂度、禁止 `any`。配置里的注释说「agent 在类型麻烦时就会用 any」。
- AGENTS.md 里的规则同时写成 lint 规则，报错信息写明「见 AGENTS.md」，两者在同一个 PR 里加入。
- 全仓库只有 3 处 `eslint-disable`，其中两处在 `--` 后写了理由。
- 外部 skill 用锁文件按内容哈希固定，安装目录不进 git。项目没有自己写的 skill，项目知识都在 `.agents/` 和 `plans/` 里。
- CI 只跑 lint 和类型检查，测试和构建不在 CI 里运行。也没有审阅门槛，维护者直接合并。

## 对 Lectern 的建议

| 建议 | 落在哪里 | 相关 issue |
|---|---|---|
| 已看标记的键包含审阅对象（会话或比较范围）、路径和改动前后两个 blob OID；改动前后的文件模式（如可执行位）和改动状态也计入。同一审阅对象和路径的改动前或改动后 OID、模式、状态任一变化时（如 base 前进后只有改动前的一侧变了，或只改了可执行位），标为「审过之后又被改过」，界面显示为第三种状态并提供重新标记。OID 按仓库的哈希算法取完整长度，不假定 40 位 | `review` 设计文档 | [#15](https://github.com/SoloJiang/lectern-app/issues/15) |
| 标为已看后折叠该文件，与快捷键 V 的跳转配合 | `review_view` | [#15](https://github.com/SoloJiang/lectern-app/issues/15) |
| 维护一份噪声文件模式列表（生成文件、lockfile、快照），同时用于默认折叠、规则分组和模型输入 | `review` 或 `git` 设计文档 | [#15](https://github.com/SoloJiang/lectern-app/issues/15)、[#21](https://github.com/SoloJiang/lectern-app/issues/21) |
| 全量高亮：hunk 用完整文件的高亮结果切片，不单独高亮 hunk；完整文件的高亮按 blob OID 加语言和 grammar / 高亮查询的版本缓存，同样的内容在不同语言的文件里不会命中错误的结果，grammar 升级后旧结果失效 | `git` 设计文档 | [#14](https://github.com/SoloJiang/lectern-app/issues/14) |
| 工作区 diff 包含未跟踪文件，但不写用户的 index；单文件设大小上限并标记截断；工作区指纹用基准树 OID 加 diff 内容的哈希；不执行仓库配置的 textconv 和外部 diff 程序（调用 git 命令行时加 `--no-ext-diff --no-textconv`） | `git` 设计文档 | [#14](https://github.com/SoloJiang/lectern-app/issues/14) |
| 识别不了会话日志、或改动不属于任何会话时，用有序 glob 规则表分组，取代平铺的文件列表；规则用 `globset` 实现，首帧后计算 | `review`、`project_home` | [#15](https://github.com/SoloJiang/lectern-app/issues/15)、[#30](https://github.com/SoloJiang/lectern-app/issues/30) |
| 会话简报里「值得先看的几处」采用 `critical` 加文件备注、行备注的结构；行备注只挂在 diff 里显示的行上 | `review` 设计文档 | [#15](https://github.com/SoloJiang/lectern-app/issues/15) |
| 任何模型输出都走同一套校验：结构校验、覆盖检查、一次纠错重试、对账、未分类兜底 | `review` 设计文档 | [#15](https://github.com/SoloJiang/lectern-app/issues/15) |
| 调用 Claude Code、Codex 生成简报时，用只读参数启动无界面进程，在单独的会话里运行，不续接正在审阅的会话，避免在该会话里多出审阅轮次。这个辅助会话不能出现在会话列表里：可用时用不保存会话的调用方式（如 Claude Code 的 `--no-session-persistence`），否则记录它的会话 ID，会话发现时过滤掉。pulls.review 不限制这类进程的轮数和时长，Lectern 要设超时，用户离开或取消时终止子进程并回收。只读参数只禁止写入，不限制读取，被审阅的内容可能诱导 agent 读取仓库外的文件，所以进程要在系统级沙箱里运行，只允许读取本次分析的输入、被审阅的仓库和该 agent 登录所需的凭据（凭据文件或钥匙串条目，按 agent 逐个列出），并且不加载用户的其他配置和 MCP 服务器。沙箱的生成属于 macOS 专属能力，由 `platform` 提供接口，`agents` 和 `executor` 负责进程的启动与生命周期 | `agents`、`executor`、`platform` 设计文档 | [#13](https://github.com/SoloJiang/lectern-app/issues/13) |
| 模型结果的缓存键是完整分析请求的哈希：规范化后的提示词（含标题、描述、prompt 摘录、输出语言）、排序后的（路径，改动前 blob OID，改动后 blob OID）列表、schema 版本和模型 ID。diff 超过内联上限时模型通过工具读取 diff，提示词里没有 diff 正文，所以两侧 OID 都要进键。agent 能读仓库里未改动的文件时，键还要包含被审阅 worktree 的指纹（基准树 OID 加工作区 diff 的哈希），否则未改动文件的变化不会让结果失效；只缓存模型结果，规则结果每次重算 | `cache` 设计文档 | [#17](https://github.com/SoloJiang/lectern-app/issues/17) |
| 缓存放在 `~/Library/Caches` 下，按仓库的 git 公共目录的绝对路径区分（`git rev-parse --path-format=absolute --git-common-dir`，或解析后取规范路径；默认输出常常是相对路径 `.git`，不同仓库会撞在一起），同一仓库的 worktree 共用，且不在用户的 `.git` 里写文件（pulls.review 写在 `.git` 公共目录下） | `cache` 设计文档 | [#17](https://github.com/SoloJiang/lectern-app/issues/17) |
| 来源接口按能力定义：必需的取 diff，加上可选的指纹、加载完整文件、审阅评论、共享结果；视图按能力决定功能 | `forge` 设计文档 | [#25](https://github.com/SoloJiang/lectern-app/issues/25) |
| MR 视图请求 GitHub 时处理限流和条件请求，pulls.review 没有做。MR 的指纹同时包含 head 和 base（或合并基点），以及进入分析的 MR 元数据（标题、描述）的哈希；任一变化时缓存的结果按过期处理 | `forge` 设计文档 | [#25](https://github.com/SoloJiang/lectern-app/issues/25) |
| 设计文档模板增加状态行（计划中、进行中、已实现并列出 PR）、每步的「完成标准」、「实际验证过什么」（区分 Linux CI 和 macOS 实测）、实现后的「与设计的差异」 | `docs/design/TEMPLATE.md` | 无 |
| AGENTS.md 写明实现以已合并的设计文档为准：实现中需要改设计时，在同一个 PR 里修改设计文档（改架构的另写 ADR）并写明原因，不让代码先偏离设计再补文档。pulls.review 反过来以代码为准，它的文档因此持续落后于代码，而 Lectern 要求设计先经审阅。CI 检查文档间的链接都能打开 | `AGENTS.md`、`just check` | 无 |
| clippy 的 `disallowed-methods`、`disallowed-types` 在 `reason` 里写明对应的架构规则或 ADR；开启 `allow_attributes_without_reason` | `clippy.toml`、`just check` | 无 |
| 测试启动外部进程的代码时，在 PATH 上放一个假命令，记录参数和标准输入，回放夹具 | `agents`、`forge`、`code_intel` | [#13](https://github.com/SoloJiang/lectern-app/issues/13)、[#25](https://github.com/SoloJiang/lectern-app/issues/25) |

### 不采用的部分

- **把 token 存在 localStorage**：Lectern 复用 `gh`、`glab` 的登录状态（[ADR 0008](../adr/0008.md)），需要存凭据时用 `platform` crate 里的钥匙串。
- **进程内的 TypeScript agent 运行时和浏览器端推理**：违反架构规则 3。模型循环里的提醒、轮数上限、读取上限可以在 Rust 里重写，或者直接交给 agent 命令行。
- **把整份 diff 和对话记录存成 JSON 缓存**：Lectern 可以从 git 对象和会话日志重建这些数据，缓存只存派生结果，按 blob OID 存在磁盘上的 SQLite 里，按字节 LRU 淘汰（[ADR 0017](../adr/0017.md)）。
- **按字符估算 token**：Lectern 从会话日志读取真实的 token 用量（[ADR 0014](../adr/0014.md)），模型调用也应记录真实用量。
- **只跑 lint 的 CI、浮动版本的复用工作流、不设审阅门槛**：Lectern 现有的 `just ci`、cargo-deny 和 Codex 审阅更严格，保持不变。
- **以最终 diff 为唯一阅读顺序**：Lectern 保留按轮次阅读。pulls.review 的按提交查看（`?commit=<sha>`）是它最接近轮次视图的功能。

## 未决问题

以下问题涉及产品方向，需要人决定。

1. 会话简报是否调用模型？可选的做法有：只用规则（风险路径、没被更新的调用方、Drifted、噪声文件）；规则加可选的模型分析，由用户点击触发；或者打开会话时自动分析。调用模型会消耗用户 Claude Code 或 Codex 的额度。
2. MR 视图是否读取 pulls.review 写在 PR 里的共享分析评论？读取只需要解析一种评论格式，可以让已经用 pulls.review Action 的仓库在 Lectern 里直接看到分组。Lectern 是否也把自己的分析写回 PR，是另一个问题。
