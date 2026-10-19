---
name: ship-pr
description: Take a Lectern change from branch to squash-merged PR under the repository's merge rules - run the checks, self-review before pushing with an independent read-only reviewer agent, open the PR, handle findings and human threads, then merge. Use when a change is ready for review or when asked to merge or follow up on a PR.
---

# 提交并合并 PR

合并规则见根目录 AGENTS.md「合并规则」，以下是执行步骤。命令里的 `R` 是 `repos/SoloJiang/lectern-app`，`N` 是 PR 号。

## 1. 提交前

- 一个 PR 只做一个 issue，改动尽量在一个 crate 内。
- 本地跑 `just ci`。在 Linux 上它只覆盖不依赖 GPUI 的 crate；改了视图层或 `app` 的，结果以 macOS CI 为准。
- 新增界面字符串时，en 与 zh-CN 都要提供；新增依赖时，`just deny` 要通过。
- 实现 PR 改了某个 crate 的接口、数据结构或验收条件时，diff 里要有 `docs/design/<crate>.md` 的对应修改、更新后的状态行，以及「实际验证」「与设计的差异」两节；影响到 ADR 的同时更新 ADR。

## 2. 推送前自审

实现 agent 只在本地提交，不推送。负责推进的 agent 审完再推送到 PR 分支：

1. 用这次的提交建一个 detached worktree（`git worktree add --detach <目录> <sha>`），审完删除。
2. 派一个独立的只读审阅 agent：它没有参与编写，只读和搜索，不构建、不提交、不推送。审阅范围是相对 base（通常是 `origin/main`）的全部改动；修复轮只审上一轮之后的 diff，但要检查修复有没有在其他路径留下同一个根因。
3. 审阅标准是 AGENTS.md「Review guidelines」：只报 P0、P1，每条写文件和行号、能复现的失败场景和根因。除了这次改动本身，还要查四类问题：
   - 架构规则；
   - 交互路径上的性能预算；
   - 设计文档与代码是否一致；
   - 同一根因在其他路径上是否重复出现。
4. 按第 4 步分级处理。有 P0、P1 时交回实现 agent 修根因，修复后的提交回到本步重审，直到没有 P0、P1。
5. 推送，并在 PR 上评论自审记录：审过的提交、发现的问题与处理结果、没有修的 P2 及理由。PR 还没开时，记录写进 PR 正文。

只改文档的 PR 不需要自审；设计文档和 ADR 的 PR 除外。

## 3. 开 PR

- 标题用 `<范围>: <做了什么>`，正文写改了什么、为什么、怎么验证，末尾写 issue：实现 PR 和不需要设计的 PR 用 `Closes #<issue>`；设计文档 PR 用 `Refs #<issue>`，issue 留给后续的实现 PR 关闭。一个 issue 涉及多个 crate 时，只有最后完成的那个实现 PR 用 `Closes`，其余用 `Refs`（AGENTS.md「工作方式」）。
- 叠在其他分支上的 PR，base 设为下面那个分支，正文写明依赖关系。
- 本仓库不再使用 Codex 审阅；PR 上出现的 Codex 评论不需要处理，也不要评论 `@codex review`。

## 4. 处理意见

意见来自自审和人。先核实，再按级别和代价决定，不要直接照改。「意见成立」不等于「现在值得改」：

- P0、P1（数据、安全边界、架构规则、产品原则、常用流程的错误结果）：修复。
- P2：只在改动小、属于本 PR 范围、不让文档或代码变复杂时修。少见的边界情况、在上一轮修复之上继续追加的细化、超出威胁模型的攻击，在自审记录或回复里写明理由：设计文档里记为「已知限制」（一句话，不展开算法），或写「实现阶段处理」，值得跟进的开 issue 并给出链接。
- 不成立：写明理由。
- 同一根因的几条意见合并成一次修复。设计文档和 ADR 只写决策和关键约束，不为了回应边界情况把规则写成完整算法；同一类意见反复出现时，修根因（如收窄支持范围、明确威胁模型），不逐条打补丁。
- 修复后的提交推送前重新自审（第 2 步）。
- 人留下的讨论串，回复后解决，只解决自己处理过的那一条：

  ```sh
  gh api $R/pulls/$N/comments/<comment_id>/replies -f body='...'
  gh api $R/pulls/$N/ccr/review_threads --jq '.[]|select(.resolved|not)'
  gh api -X POST $R/pulls/$N/ccr/comments/<comment_id>/resolve
  ```

  `ccr/…` 路由只在 Claude Code 的云端会话里可用。在其他环境（如本地的 `gh`）里用 GitHub GraphQL：

  ```sh
  gh api graphql --paginate \
    -f query='query($o:String!,$r:String!,$n:Int!,$endCursor:String){repository(owner:$o,name:$r){pullRequest(number:$n){reviewThreads(first:100,after:$endCursor){nodes{id isResolved comments(first:1){nodes{databaseId}}} pageInfo{hasNextPage endCursor}}}}}' \
    -F o=SoloJiang -F r=lectern-app -F n=$N \
    --jq '.data.repository.pullRequest.reviewThreads.nodes[]|select(.isResolved|not)'
  gh api graphql -f query='mutation($id:ID!){resolveReviewThread(input:{threadId:$id}){thread{isResolved}}}' -F id=<thread_id>
  ```

## 5. 合并

以下条件全部满足时才能合并：

1. 最新提交通过了自审：PR 上的自审记录写明的提交等于 head，没有未处理的 P0、P1。只改文档的 PR 整条免除；设计文档和 ADR 的 PR 不免。
2. CI 全部通过：`gh api --paginate $R/commits/<sha>/check-runs --jq '.check_runs[]|[.name,.status,.conclusion]|@tsv'` 的结果里必须有 `.github/workflows/ci.yml` 定义的三个任务 `Checks (Linux, GPUI-free crates)`、`Build and test (macOS arm64)` 和 `Benchmarks (macOS arm64)`，而且每一行都是 `completed` 和 `success`。结果为空或缺任何一个任务都不算通过：推送后 GitHub 登记检查需要几秒到几十秒。列表类的接口默认每页 30 条，都要带 `--paginate`。
3. 没有合并冲突：`gh api $R/pulls/$N --jq .mergeable_state` 为 `clean`。
4. 人留下的讨论串都已解决（`ccr/review_threads` 或 GraphQL 的 `reviewThreads`）。

```sh
gh api -X PUT $R/pulls/$N/merge -f merge_method=squash -f sha=<最新提交>
```

- 叠放的 PR：先合底层，再把上层的 base 改为 `main`（`gh api -X PATCH $R/pulls/<上层> -f base=main`），合并 `origin/main` 解决冲突，冲突的解决改了代码时重新走第 2 步。
- GitHub 的 stack 功能管理的 PR 不能用 API 合并或改 base，请人在网页上操作。
- 合并后先 `cd` 到另一个保留的 checkout（如主 checkout），再 `git worktree remove` 删除这个 worktree 和本地分支；不要在要删除的目录里执行删除。
