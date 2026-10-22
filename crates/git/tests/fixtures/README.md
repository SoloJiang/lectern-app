# git 夹具

`numstat_matches_git` 和 `rename_*`、`diff_trees_*` 测试用的对照数据：用真实的 git 构造两棵树，记下 git 对它们的 `--numstat` 和 `--raw` 输出；`worktree_*` 测试用的工作区夹具见下文「工作区夹具」，`head_reflog_*` 用的见「reflog 夹具」，`ref_reflog_*` 用的见「ref_reflog 夹具」，`patch_id_*` 用的见「patch_id 夹具」。测试只导入这些文件，不运行 git（[设计文档「测试」](../../../../docs/design/git.md#测试)）。

## 格式

每个夹具是一个目录，不是 tar 归档：工作区里没有解包 tar 的依赖，只为测试加一个不值得；目录里的文件也能在 PR 里直接审阅。

| 文件 | 内容 |
|---|---|
| `trees` | `old <树 OID>` 和 `new <树 OID>` 两行 |
| `old.tree`、`new.tree` | 两棵树的 `git ls-tree -r --full-tree` |
| `blobs/<OID>` | 两棵树用到的每个 blob 的内容（子模块只有 OID，没有内容） |
| `expected/git-version` | 生成输出的 `git --version` |
| `expected/<变体>/numstat` | `git diff --no-ext-diff --no-textconv -M --histogram --numstat -z <old> <new>` |
| `expected/<变体>/raw` | 同上，`--raw --no-abbrev -z` |

`<变体>` 是 `default`，或 `renameLimit=<n>`（加 `-c diff.renameLimit=<n>` 运行；测试导入时在仓库配置里写同样的值）。

`TestRepo::import_fixture` 写入 blob、按两份列表重建两棵树，并检查每个 OID 都与 git 记录的相同。

### 工作区夹具

`worktree_*` 测试用的夹具保存工作区本身，没有 `trees` 文件（`numstat_matches_git` 据此跳过它们）：

| 文件 | 内容 |
|---|---|
| `files/<路径>` | 工作区文件，逐字节保存；名字以 `.git` 开头的（`.gitattributes`）存为 `dot-git…`，否则它们会作用在本仓库里的夹具上 |
| `expected/git-version` | 生成输出的 `git --version` |
| `expected/<变体>/ls-files` | `git add -A` 之后的 `git ls-files -s -z`：git 按属性转换后存进对象库的 blob OID |

`<变体>` 是 `autocrlf=<值>`（`git add` 加 `-c core.autocrlf=<值>` 运行；测试在仓库配置里写同样的值）。`TestRepo::import_worktree_fixture` 把 `files/` 写进工作区，并把 `dot-git…` 还原成 `.git…`。

### reflog 夹具

`head_reflog_*` 测试用的夹具保存 git 写出的 HEAD reflog，也没有 `trees` 文件。测试把 `logs/HEAD` 复制进临时仓库的 git 目录，不导入提交和树：

| 文件 | 内容 |
|---|---|
| `logs/HEAD` | 脚本里仓库的 `.git/logs/HEAD`，逐字节保存 |
| `expected/git-version` | 生成输出的 `git --version` |
| `expected/entries` | `git log -g --date=unix --format='%gd %H %gs' HEAD`，`HEAD@{<秒>}` 换成 `<秒>`：git 自己从新到旧读出的时间、新值和消息 |

### ref_reflog 夹具

`ref_reflog_*` 测试用的夹具保存 git 在 `logs/refs/` 下写出的引用 reflog，分几个步骤各存一份快照。测试把一个步骤的 `logs/` 复制进临时仓库的公共目录，不导入提交和树：

| 文件 | 内容 |
|---|---|
| `<步骤>/logs/<引用名>` | 这一步时 `.git/logs/<引用名>`，逐字节保存 |
| `<步骤>/expected/<引用名>` | `git log -g --date=unix --format='%gd %H %gs' <引用名>`，`<名>@{<秒>}` 换成 `<秒>`：git 自己从新到旧读出的时间、新值和消息 |
| `<步骤>/absent` | 这一步没有 reflog 文件的引用名，每行一个 |
| `expected/git-version` | 生成输出的 `git --version` |

步骤：`branches` 是全部分支和远端跟踪分支的 reflog，`stash`、`stash-drop`、`stash-cleared` 只有 `refs/stash`。

### patch_id 夹具

`patch_id_*` 测试用的夹具保存同一个根提交上的若干提交的树，以及 git 对每个提交给出的补丁 ID。测试按列表重建每棵树、检查 OID 与 git 记录的相同，再用 `TestRepo` 按父子关系写出提交（提交 OID 与 git 的不同，补丁 ID 只取决于树）。这里的 `trees/` 是目录，不是树夹具的 `trees` 文件，`numstat_matches_git` 不会取它：

| 文件 | 内容 |
|---|---|
| `commits` | 每行 `<名字> <树 OID> <父提交的名字>`，根提交的父提交为 `-`，父提交在前 |
| `trees/<树 OID>` | `git ls-tree -r -z --full-tree <树>`：路径里有 tab、引号和反斜杠，用 `-z` 不经引号 |
| `blobs/<OID>` | 各棵树用到的每个 blob 的内容（子模块只有 OID） |
| `expected/git-version` | 生成输出的 `git --version` |
| `expected/patch-id` | 每行 `<名字> <补丁 ID>`：`git -c core.quotePath=true show --format= --no-ext-diff --no-textconv --no-renames --full-index <提交> \| git patch-id --stable` 的第一列（`common.sh` 的设置里有 `diff.algorithm=histogram`），git 不输出时为 `-`；`order -O` 是同一个提交加 `-O <顺序文件>`、按另一种文件顺序列出时的结果 |

根提交不记录（`Repo::patch_id` 对它为 None）。`binary` 一行记录了 git 的值，测试不比较：git 对二进制 diff 的补丁 ID 取决于输入文本里 `index` 行的缩写长度，本 crate 改用两侧的完整 OID（[设计文档「usage 用的读取」](../../../../docs/design/git.md#usage-用的读取)）。

## 生成

```sh
bash crates/git/tests/fixtures/scripts/generate-all.sh <临时目录>
```

每个夹具一个脚本（`scripts/<名字>.sh`，头部注明 git 版本），共用 `scripts/common.sh`。脚本在 `<临时目录>/<名字>` 里建一个临时仓库，用 `git add` 和 `git write-tree` 写出两棵树，不提交，所以不受提交签名和身份配置影响；影响这两种输出的设置都用 `-c` 显式给出，外部 diff 和 textconv 用 `--no-ext-diff --no-textconv` 关掉，机器上的 git 配置不改变结果。`reflog.sh`、`ref_reflog.sh`、`patch_id.sh` 要提交：身份和每一步的时间由环境变量固定（`GIT_COMMITTER_DATE` 也是 reflog 记录的时间），`GIT_CONFIG_GLOBAL=/dev/null`、`GIT_CONFIG_NOSYSTEM=1` 不读机器上的配置，所以每次生成的 OID 和时间相同。`ref_reflog.sh` 的远端 `origin` 的地址是 `.`（从仓库自己 fetch），reflog 消息里没有临时目录的路径。

| 夹具 | 内容 |
|---|---|
| `basic` | 新增、删除、修改、只改模式、类型变化、符号链接、属性和 NUL 判定的二进制、8000 字节之后才有 NUL 的文本、CRLF、末尾没有换行、文件变成目录、子模块、`linguist-generated` |
| `renames` | 精确重命名与 git 的同名优先、空文件、同名文件的 75 % 和矩阵的 50 %、符号链接与子模块只按精确匹配、低于 50 % 不配对、CRLF 与二进制的相似度；变体 `renameLimit=1` |
| `rename_many` | 40 个文件换名并小改、5 个精确重命名；变体 `renameLimit=39`（只剩精确重命名）、`renameLimit=40` |
| `real` | 本仓库历史里 6 个文件各自一次提交前后的版本（文档、Python、YAML、Rust），以及一个同一行重复六十多次的合成文件（histogram 的退化情形） |
| `worktree_conversions` | 工作区夹具：`text` / `eol` / `text=auto` / `-text`、`ident`、UTF-16LE 与带 BOM 的 UTF-16 `working-tree-encoding`，CRLF、混合换行、单独的 CR、带 NUL 的 `text=auto` 文件；变体 `autocrlf=false`、`true`、`input` |
| `reflog` | reflog 夹具：初始提交（旧值全 0）、提交、`checkout -b`（旧值等于新值）、切换分支、`reset --hard HEAD~1`、`commit --amend`、merge，共 9 条，时间从 1700000000 起 |
| `ref_reflog` | ref_reflog 夹具：`branch` 创建、提交、`branch -m` 改名（记录随文件移到新名字下）、`branch -f`；`checkout -b` 创建的分支；删除的分支（没有文件）；`fetch` 更新的三个远端跟踪分支；`refs/stash` 的三次 push、删掉中间一条（git 重写文件）、删光（文件随引用删除） |
| `patch_id` | patch_id 夹具：相隔 6 行和 7 行的改动（合并与不合并的 hunk）、文件首尾的上下文、末尾没有换行、补上末尾换行、CRLF、缩进启发滑动的插入；新增、删除、空文件、可执行文件、符号链接；只改模式、模式加内容；文件换成符号链接、文件换成目录；子模块的新增与移动；含空格、非 ASCII、tab、引号和反斜杠的路径；只差空白的两个改动；按两种文件顺序列出的同一个改动；二进制文件；空提交 |

git 升级导致输出变化时新增一份夹具，不覆盖旧的。生成于 git 2.43.0。
