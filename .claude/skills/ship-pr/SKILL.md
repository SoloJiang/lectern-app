---
name: ship-pr
description: Take a Lectern change from branch to squash-merged PR under the repository's merge rules - run the checks, open the PR, get Codex's review, answer and resolve every thread, then merge. Use when a change is ready for review, when asked to merge or follow up on a PR, or when Codex leaves comments.
---

# 提交并合并 PR

合并规则见根目录 AGENTS.md「合并规则」，以下是执行步骤。命令里的 `R` 是 `repos/SoloJiang/lectern-app`，`N` 是 PR 号。

## 1. 提交前

- 一个 PR 只做一个 issue，改动尽量在一个 crate 内。
- 本地跑 `just ci`。在 Linux 上它只覆盖不依赖 GPUI 的 crate；改了视图层或 `app` 的，结果以 macOS CI 为准。
- 新增界面字符串时，en 与 zh-CN 都要提供；新增依赖时，`just deny` 要通过。
- 实现 PR 改了某个 crate 的接口、数据结构或验收条件时，diff 里要有 `docs/design/<crate>.md` 的对应修改、更新后的状态行，以及「实际验证」「与设计的差异」两节；影响到 ADR 的同时更新 ADR。

## 2. 开 PR

- 标题用 `<范围>: <做了什么>`，正文写改了什么、为什么、怎么验证，末尾写 issue：实现 PR 和不需要设计的 PR 用 `Closes #<issue>`；设计文档 PR 用 `Refs #<issue>`，issue 留给后续的实现 PR 关闭。一个 issue 涉及多个 crate 时，只有最后完成的那个实现 PR 用 `Closes`，其余用 `Refs`（AGENTS.md「工作方式」）。
- 叠在其他分支上的 PR，base 设为下面那个分支，正文写明依赖关系。

## 3. 等待审阅

- 只改文档的 PR 不需要等 Codex，满足第 5 步的其余条件即可合并；设计文档 PR 除外。Codex 在文档 PR 上留下的意见照常处理。
- 其余 PR 等 Codex 审阅当前最新提交：PR 上的 reaction 是 👀 表示审阅中，有意见时留行内评论，没有意见时通常留 👍，但 👍 并不稳定，有时审阅完成也没有。「Codex 已审阅最新提交」以 Codex 汇总评论里的审阅状态为准：`sha` 等于 PR 的 head、`status` 为 `completed`。

  ```sh
  gh api $R/pulls/$N --jq .head.sha
  gh api --paginate $R/issues/$N/comments \
    --jq '.[]|select(.user.login=="chatgpt-codex-connector[bot]")|.body|select(contains("codex-review-state:v1"))
          |capture("\"code_review\":(&#123;|\\{)\"commit_sha\":\"(?<sha>[0-9a-f]+)\",\"has_any_findings\":(?<findings>[a-z]+),\"has_unresolved_findings\":(?<unresolved>[a-z]+),\"status\":\"(?<status>[a-z]+)\"")'
  gh api --paginate $R/issues/$N/reactions --jq '.[]|"\(.user.login) \(.content)"'
  gh api --paginate $R/pulls/$N/comments --jq '.[]|"\(.id) \(.user.login) \(.path):\(.line) \(.body)"'
  ```

- Codex 审阅失败，或推送新提交后长时间没有更新时，在 PR 上评论 `@codex review` 重新触发。不要只为了拿到 👍 重新触发：重新审阅同一个提交常常会找出新的边界情况。
- 每次推送都会触发一次新的审阅，所以一轮意见攒齐、一起修完再推送，不要一条一推。

## 4. 处理意见

先核实，再按级别和代价决定，不要直接照改。「意见成立」不等于「现在值得改」：

- P0、P1（数据、安全边界、架构规则、产品原则、常用流程的错误结果）：修复。
- P2：只在改动小、属于本 PR 范围、不让文档或代码变复杂时修。少见的边界情况、在上一轮修复之上继续追加的细化、超出威胁模型的攻击，回复理由后解决：设计文档里记为「已知限制」（一句话，不展开算法），或回复「实现阶段处理」，值得跟进的开 issue 并给出链接。
- 不成立：回复理由。
- 同一根因的几条意见合并成一次修复。设计文档和 ADR 只写决策和关键约束，不为了回应边界情况把规则写成完整算法；同一类意见反复出现时，修根因（如收窄支持范围、明确威胁模型），不逐条打补丁。
- 一轮的意见全部处理完再推送一次。
- 回复后解决这个讨论串，只解决自己处理过的那一条：

  ```sh
  gh api $R/pulls/$N/comments/<comment_id>/replies -f body='...'
  gh api $R/pulls/$N/ccr/review_threads --jq '.[]|select(.resolved|not)'
  gh api -X POST $R/pulls/$N/ccr/comments/<comment_id>/resolve
  ```

  `ccr/…` 路由只在 Claude Code 的云端会话里可用。在其他环境（如 Codex、本地的 `gh`）里用 GitHub GraphQL：

  ```sh
  gh api graphql --paginate \
    -f query='query($o:String!,$r:String!,$n:Int!,$endCursor:String){repository(owner:$o,name:$r){pullRequest(number:$n){reviewThreads(first:100,after:$endCursor){nodes{id isResolved comments(first:1){nodes{databaseId}}} pageInfo{hasNextPage endCursor}}}}}' \
    -F o=SoloJiang -F r=lectern-app -F n=$N \
    --jq '.data.repository.pullRequest.reviewThreads.nodes[]|select(.isResolved|not)'
  gh api graphql -f query='mutation($id:ID!){resolveReviewThread(input:{threadId:$id}){thread{isResolved}}}' -F id=<thread_id>
  ```

## 5. 合并

以下条件全部满足时才能合并：

1. Codex 已审阅最新提交：汇总评论里的 `sha` 等于 head、`status` 为 `completed`；它在这次和之前留下的意见都已处理（修复，或回复理由）。不要求 👍。设计文档以外的只改文档的 PR 整条免除，不需要 Codex 的汇总评论；设计文档 PR 不免。
2. CI 全部通过：`gh api --paginate $R/commits/<sha>/check-runs --jq '.check_runs[]|[.name,.status,.conclusion]|@tsv'` 的结果里必须有 `.github/workflows/ci.yml` 定义的两个任务 `Checks (Linux, GPUI-free crates)` 和 `Build and test (macOS arm64)`，而且每一行都是 `completed` 和 `success`。结果为空或缺任何一个任务都不算通过：推送后 GitHub 登记检查需要几秒到几十秒。列表类的接口默认每页 30 条，都要带 `--paginate`。
3. 没有合并冲突：`gh api $R/pulls/$N --jq .mergeable_state` 为 `clean`。
4. 没有未解决的讨论串（`ccr/review_threads` 或 GraphQL 的 `reviewThreads`）。

```sh
gh api -X PUT $R/pulls/$N/merge -f merge_method=squash -f sha=<最新提交>
```

- 叠放的 PR：先合底层，再把上层的 base 改为 `main`（`gh api -X PATCH $R/pulls/<上层> -f base=main`），合并 `origin/main` 解决冲突后重新走第 3 步。
- GitHub 的 stack 功能管理的 PR 不能用 API 合并或改 base，请人在网页上操作。
- 合并后先 `cd` 到另一个保留的 checkout（如主 checkout），再 `git worktree remove` 删除这个 worktree 和本地分支；不要在要删除的目录里执行删除。
