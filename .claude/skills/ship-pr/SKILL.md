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

## 2. 开 PR

- 标题用 `<范围>: <做了什么>`，正文写改了什么、为什么、怎么验证，末尾 `Closes #<issue>`。
- 叠在其他分支上的 PR，base 设为下面那个分支，正文写明依赖关系。

## 3. 等待审阅

- 只改文档的 PR 不需要等 Codex 的 👍，满足第 5 步的其余条件即可合并；设计文档 PR 还要等人审过。Codex 在文档 PR 上留下的意见照常处理。
- 其余 PR 等 Codex 审阅当前最新提交：PR 上的 reaction 是 👀 表示审阅中，👍 表示没有意见，有意见时留行内评论。

  ```sh
  gh api $R/issues/$N/reactions --jq '.[]|"\(.user.login) \(.content)"'
  gh api $R/pulls/$N/comments --jq '.[]|"\(.id) \(.user.login) \(.path):\(.line) \(.body)"'
  ```

- Codex 审阅失败，或推送新提交后长时间没有更新时，在 PR 上评论 `@codex review` 重新触发。
- 每次推送后，之前的 👍 失效，需要等 Codex 审阅新的提交。

## 4. 处理意见

逐条核实，不要直接照改：

- 成立：修复，推送，在讨论串里回复修复的提交和做法。
- 不成立或不在本 PR 范围：回复理由；不在范围但值得做的，开 issue 并在回复里给出链接。
- 回复后解决这个讨论串，只解决自己处理过的那一条：

  ```sh
  gh api $R/pulls/$N/comments/<comment_id>/replies -f body='...'
  gh api $R/pulls/$N/ccr/review_threads --jq '.[]|select(.resolved|not)'
  gh api -X POST $R/pulls/$N/ccr/comments/<comment_id>/resolve
  ```

  `ccr/…` 路由只在 Claude Code 的云端会话里可用。在其他环境（如 Codex、本地的 `gh`）里用 GitHub GraphQL：

  ```sh
  gh api graphql -f query='query($o:String!,$r:String!,$n:Int!){repository(owner:$o,name:$r){pullRequest(number:$n){reviewThreads(first:100){nodes{id isResolved comments(first:1){nodes{databaseId}}}}}}}' \
    -F o=SoloJiang -F r=lectern-app -F n=$N
  gh api graphql -f query='mutation($id:ID!){resolveReviewThread(input:{threadId:$id}){thread{isResolved}}}' -F id=<thread_id>
  ```

## 5. 合并

以下条件全部满足时才能合并：

1. Codex 对最新提交给了 👍（文档 PR 免）。
2. CI 全部通过：`gh api $R/commits/<sha>/check-runs --jq '.check_runs[]|[.name,.conclusion]|@tsv'`。
3. 没有合并冲突：`gh api $R/pulls/$N --jq .mergeable_state` 为 `clean`。
4. 没有未解决的讨论串（`ccr/review_threads` 或 GraphQL 的 `reviewThreads`）。

```sh
gh api -X PUT $R/pulls/$N/merge -f merge_method=squash -f sha=<最新提交>
```

- 叠放的 PR：先合底层，再把上层的 base 改为 `main`（`gh api -X PATCH $R/pulls/<上层> -f base=main`），合并 `origin/main` 解决冲突后重新走第 3 步。
- GitHub 的 stack 功能管理的 PR 不能用 API 合并或改 base，请人在网页上操作。
- 合并后删除 worktree 和本地分支。
