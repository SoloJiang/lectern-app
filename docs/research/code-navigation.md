# 代码导航与文件索引调研

2026-10-17。调研对象是浏览文件和代码跳转的速度：要达到什么延迟、文件和符号索引用什么算法与数据结构、排序和降级用什么产品规则。资料来自各项目的源码、文档和作者给出的基准；Lectern 自己的数字来自一组粗略的微基准（见「实测说明」），标为「本文实测」，没有实测依据的标为「估算」。

## 背景与目标

产品要求（原话）：「从产品层面，我希望在浏览文件或进行代码跳转的时候要足够的快，所以在文件索引上你需要调研下是否要有特殊的算法和产品逻辑设计」。

现有设计已经定下的前提：

- 文件列表来自 git index，不遍历目录（[project 设计「文件列表」](../design/project.md#文件列表)）；派生数据以 blob OID 为键缓存（架构规则 5），存进 SQLite 的 `cache`（[cache 设计](../design/cache.md)）；`fs_watch` 交付按 index 过滤、合并过的批次（[fs_watch 设计「批次」](../design/fs_watch.md#批次)）；工作区文件只经 `SafeRoot` 读取，不用 mmap（[platform 设计「读取上限」](../design/platform.md#读取上限)）。
- 导航分四层：语法索引、Java 解析器、依赖源码、可选的 jdtls / SCIP；结果只通过上下文面板和 ⌘K 呈现，界面不显示索引和语言服务器的状态（[技术调研「代码导航与编辑」](../design/tech-research.md#代码导航与编辑)）。
- 预算：冷启动 20 ms、空窗口空闲 60 MB、monorepo 空闲 100 MB、打开文件到首屏高亮一帧（[ADR 0017](../adr/0017.md)、[性能预算](../design/tech-research.md#性能预算)）。

### 结论

1. **需要专门设计，难点不在匹配算法本身。** 成熟的模糊匹配库在 14 万条路径上每次按键只要几毫秒。真正决定体验的是四件事：路径语料在内存里的布局、贴合审阅场景的排序规则、索引按什么顺序建、索引没建完时怎么回答。
2. **⌘K 的文件匹配用 [frizbee](https://github.com/saghen/frizbee)（MIT）。** 它是 SIMD 化的 Smith-Waterman 匹配器，没有运行时依赖。本文实测比 nucleo-matcher 快 3–4 倍：14 万条路径单线程 0.8–6 ms，98 万条 4 线程 3.5–23 ms。并行由 Lectern 在 `executor` 上分片完成，不调用库里自己开线程的接口。Zed 的 `fuzzy` crate 是 GPL-3.0，不能用。
3. **排序在匹配分数之上加审阅语境。** 文件名命中排在路径命中之前；本次审阅改到的文件、agent 碰过的文件、最近打开的文件、同一子项目、离当前文件近的路径依次加分；生成文件和 lockfile 降权。空查询直接列出本次审阅的改动文件和最近打开的文件。
4. **符号索引按 blob 存，查询按 worktree 过滤。** 每个 blob 用 tree-sitter tags 抽取一次定义和引用名，写进 `cache` 仓库分库里的自定义表；同一内容在所有 worktree 里只索引一次。按名字查找用 SQLite 的 B-tree 索引（本文实测 < 0.1 ms），⌘K 的符号模糊搜索对去重后的名字表跑同一个 frizbee。暂不引入 `fst`，也不建三元组全文索引。
5. **建索引有优先级，并且不拖住「静止」。** 顺序是：当前文件 → 本次审阅的改动文件和它们的导入 → 当前子项目 → 当前 worktree 其余部分 → 其他 worktree 中内容不同的文件。按本文实测的 tags 吞吐（单核约 4 MB/s）估算，10 万文件的仓库要一分钟左右，超过 ADR 0017「首帧后 30 秒内静止」的判定，需要一条新 ADR 规定后台索引与静止的关系。
6. **索引不完整时照常回答。** 依次查当前文件、按导入规则解析的文件、已建好的索引，最后在当前子项目里限时搜索标识符；结果陆续追加到抽屉里，不显示「索引中」。
7. **10 万条以内的路径列表维持现有布局；100 万条要换布局。** 98 万条路径按现有 `FileList` 布局约 62 MB，单这一项就占掉 monorepo 预算的大半。按目录去重约 30 MB，前缀压缩约 15 MB。

## 1. 各交互的延迟目标

### 来源给出的阈值

- Nielsen 沿用 Miller（1968）的三个界限：0.1 秒内用户感觉是自己的操作直接产生了结果，1 秒内思路不被打断，10 秒是注意力的上限（[Nielsen Norman Group](https://www.nngroup.com/articles/response-times-3-important-limits/)）。
- Google 的 RAIL 模型：用户输入引发的变化在 100 ms 内完成，所以输入事件要在 50 ms 内处理完；动画每帧的工作不超过 10 ms（[web.dev RAIL](https://web.dev/articles/rail)）。
- Lectern 的帧预算更紧：「滚动与输入稳定 120 fps」，即每帧 8.3 ms；打开文件到首屏高亮 < 16 ms；上下文面板滑入动画 380 ms（[交互规范「动效」](../design/interaction.md#动效)）；100 ms 内完成的操作不显示加载态（[交互规范「状态与异常」](../design/interaction.md#状态与异常)）。

### 快的工具做到的程度

| 工具 | 场景 | 数字 | 来源 |
|---|---|---|---|
| nucleo（Helix） | 约 300 万条，模式 `///.` | 约一帧（1/30 秒）；fzf 约 1 秒；Ryzen 5950X | [nucleo README](https://github.com/helix-editor/nucleo) |
| nucleo | Linux 内核文件列表 | 2.1–9.5 ms；skim 16.9–35.5 ms | 同上 |
| frizbee | Chromium 1,406,941 条路径，查询 `linux` | 单线程 22.4 ms（nucleo 90.5 ms，fzf 120.8 ms）；8 线程 3.5 ms（nucleo 36.8 ms，fzf 16.4 ms）；Ryzen 9950X3D | [frizbee BENCHMARKS.md](https://github.com/saghen/frizbee/blob/main/BENCHMARKS.md) |
| fff | 50 万文件的 Chromium | 每次查询 < 10 ms；ripgrep 每次启动 3–9 s | [fff.nvim](https://github.com/dmtrKovalenko/fff.nvim) |
| Helix 文件选择器 | 打开时 | 边遍历边推送，前 30 ms 同步推送，其余转到后台 | [helix-term ui/mod.rs](https://github.com/helix-editor/helix/blob/master/helix-term/src/ui/mod.rs) |
| IntelliJ | 建索引期间 | 进入 dumb mode，只保留不依赖索引的功能（文本编辑、版本控制） | [IntelliJ SDK：Indexing and PSI Stubs](https://github.com/JetBrains/intellij-sdk-docs/blob/main/topics/basics/indexing_and_psi_stubs.md) |
| GitHub 代码导航 | 基于搜索的跳转 | 只支持少于 100,000 个文件的仓库 | [GitHub Docs：Navigating code on GitHub](https://github.com/github/docs/blob/main/content/repositories/working-with-files/using-files/navigating-code-on-github.md) |
| Sourcegraph 基于搜索的跳转 | 未建索引的搜索 | 5 秒没结果就改用已建索引的搜索 | [Sourcegraph 的定义查找实现](https://github.com/sourcegraph/sourcegraph-public-snapshot/blob/main/client/shared/src/codeintel/legacy-extensions/search/providers.ts) |

IntelliJ 的 dumb mode 正是 Lectern 要避免的形态：索引没好时功能直接不可用。Lectern 的规则是靠前的层随时可用、靠后的层就绪后原地更新结果（[技术调研「代码导航与编辑」](../design/tech-research.md#代码导航与编辑)）。

### 建议的延迟预算

预算在 M 系列 Mac 上测量，系统文件缓存已热。

| 交互 | 预算 | 理由 |
|---|---|---|
| 打开 ⌘K | 1 帧内出现面板和空查询列表 | 面板是按需层，出现慢于一帧就会被看成卡顿 |
| ⌘K 每次按键到结果更新 | ≤ 16 ms（20 万条路径以内）；更大的仓库 ≤ 50 ms，期间保留上一轮结果 | RAIL 的 50 ms 处理时间；本文实测 14 万条单线程 ≤ 6 ms |
| 打开 Files | 1 帧（目录表已建好）；文件列表未就绪时按 100 ms 规则显示骨架 | 与项目主页共用文件列表 |
| 展开目录 | 1 帧（子项 2,000 个以内） | 只做内存里的区间查找，见第 8 节 |
| 跳转到定义：同文件或索引命中 | 候选列表 ≤ 50 ms；只有一个候选时，目标文件首屏高亮 ≤ 100 ms | 0.1 秒界限；多个候选时列表要在面板滑入动画结束前就绪 |
| 跳转到定义：索引尚未覆盖 | 第一批结果 ≤ 300 ms，限时搜索 ≤ 1 s | 1 秒界限，见第 5 节「索引不完整时」 |
| 查找引用、调用方 | 第一批 ≤ 200 ms；10 万文件的仓库全部结果 ≤ 2 s，陆续追加 | 抽屉打开即有内容，其余追加 |
| 打开文件到首屏高亮 | < 16 ms（现有预算） | [性能预算](../design/tech-research.md#性能预算) |
| 后退、前进 | 1 帧（最近访问的位置命中缓存） | 回到刚看过的位置不应重新解析 |
| 索引覆盖范围（后台，估算） | 审阅集合：打开项目后 ≤ 2 s；当前子项目 ≤ 10 s；10 万文件的整个仓库 ≤ 2 分钟 | 由第 4、5 节的吞吐推出，P2 实测后校准 |

## 2. 文件查找：模糊路径匹配

### 算法与实现

| 实现 | 打分 | 输入法 | 并行与增量 | 许可 |
|---|---|---|---|---|
| fzf v2 | 改进的 Smith-Waterman：命中 16 分，间隔起点 −3、延续 −1，单词边界 +8、空白边界 +10，驼峰 +7，连续 +4，首字符加成 ×2；O(nm)，模式超过 1000 个字符或矩阵超出缓冲时退回 O(n) 的 v1（[algo.go](https://github.com/junegunn/fzf/blob/master/src/algo/algo.go)） | 字符 | 多 goroutine 分片 | MIT（Go，不直接可用） |
| nucleo / nucleo-matcher | 与 fzf 相同的打分，多维护一个矩阵，更常找到最优对齐；长输入退回贪心；每个匹配器约 135 KB 临时内存（[nucleo README](https://github.com/helix-editor/nucleo)、[matcher 源码](https://github.com/helix-editor/nucleo/blob/master/matcher/src/lib.rs)） | UTF-32，按字素切分，支持 Unicode 规范化 | 高层 `nucleo` 自带 rayon 线程池；`reparse(…, append)` 声明新查询以旧查询为前缀，状态为 `Update` 而不是 `Rescore`（[pattern.rs](https://github.com/helix-editor/nucleo/blob/master/src/pattern.rs)） | MPL-2.0（`deny.toml` 允许） |
| Zed `fuzzy` | 字符集合（char bag）先做快速排除；按 CPU 数均分候选；结果按分数、worktree、到参考路径的距离排序（[paths.rs](https://github.com/zed-industries/zed/blob/main/crates/fuzzy/src/paths.rs)）；2025 年起第一处命中落在文件名里的结果优先（[PR #27937](https://github.com/zed-industries/zed/pull/27937)、[#28093](https://github.com/zed-industries/zed/pull/28093)） | 字符 | 后台任务分片，共享取消标志 | GPL-3.0-or-later（[Cargo.toml](https://github.com/zed-industries/zed/blob/main/crates/fuzzy/Cargo.toml)），不可用，也不得照抄 |
| Sublime（Forrest Smith 的逆向） | 递归匹配：连续 +15，分隔符后 +30，驼峰 +30，首字母 +15，前导字母每个 −5（最多 −15），未命中字母每个 −1；递归最多 10 层（[lib_fts](https://github.com/forrestthewoods/lib_fts/blob/master/code/fts_fuzzy_match.h)） | 字节 | 无 | 公有领域 |
| VS Code Quick Open | 动态规划逐字符打分：命中 +1，连续前 3 个各 +6、之后各 +3，同大小写 +1，开头 +8，`/` 后 +5，其他分隔符后 +4，驼峰 +2；再按文件名（label）和路径（description）分层：路径完全相同 2^18，文件名前缀 2^17 起，文件名命中 2^16 起，只命中路径的不加门槛分（[fuzzyScorer.ts](https://github.com/microsoft/vscode/blob/main/src/vs/base/common/fuzzyScorer.ts)） | UTF-16 | 文件枚举交给 ripgrep，打分在渲染进程 | MIT（TypeScript） |
| frizbee | 带仿射间隔的 Smith-Waterman，SIMD 按行并行；先做前缀过滤；分数小于 256 时用 u8 让 SIMD 宽度翻倍；可设 `max_typos`；超过 1024 字节的输入退回贪心（[README](https://github.com/saghen/frizbee)） | 直接匹配 UTF-8 字节，不做 Unicode 规范化（`a` 不匹配 `á`；`İ`、`ß` 的大小写折叠有缺口） | `match_list_parallel` 每次调用新开线程，结果 k 路归并 | MIT，无运行时依赖 |

各家的打分规则大同小异：连续命中、边界后的命中、驼峰位置加分，间隔扣分。差别在两处：路径是否分层（VS Code、Zed 都让文件名命中排在路径命中前面），以及吞吐。

### 本文实测

语料是本机 cargo 缓存和 `/usr` 下的 140,565 条路径（平均 56 字节，文件名平均 15 字节，平均 6 层）；98 万条是同一语料加 7 个不同前缀复制而成。nucleo-matcher 的计时包含每次把字符串转成 UTF-32，高层 `nucleo` 会预先转换，所以这一列偏慢。

| 路径数 | 查询 | frizbee 单线程 | frizbee 4 线程 | nucleo-matcher 单线程 |
|---|---|---|---|---|
| 14 万 | 6 个查询（含无结果的） | 0.8–6.1 ms | 0.6–3.8 ms | 6.7–26.0 ms |
| 98 万 | 同上 | 10.1–48.0 ms | 3.5–23.1 ms | 43.2–188.5 ms |

逐字输入 `srcmodrs`、每次只对上一轮的命中重新打分时，14 万条每次按键 3.8–11.6 ms，98 万条 33–114 ms（单线程，含对全部命中排序）。前两个字符几乎命中全部路径，耗时主要花在排序上，所以要只取前 k 个，不做全排序。

### 路径语料的内存

| 布局 | 14 万条 | 98 万条 |
|---|---|---|
| 原始字节 | 7.7 MB | 57.1 MB |
| 现有 `FileList`（连续缓冲 + 每条 4 字节偏移 + 1 字节类型） | 8.4 MB | 62.1 MB |
| 按目录去重（目录串各存一次，每个文件存目录编号和文件名） | 4.2 MB（19,288 个目录） | 29.6 MB（135,016 个目录） |
| 前缀压缩（排序后只存与上一条不同的后缀） | 2.1 MB | 14.8 MB |
| `fst` 集合 | 1.0 MB | 7.2 MB（构建 1.09 s） |

10 万条路径约 6 MB，在 project 设计给文件列表的 12 MB 之内（[project 设计「性能」](../design/project.md#性能)）。100 万条时现有布局约 62 MB，加上 git 的 48 MB 份额（[git 设计「性能」](../design/git.md#性能)）就超过 monorepo 的 100 MB。前缀压缩和 `fst` 都不能直接交给匹配器，匹配时要按块解压，每次按键多一次约 57 MB 的内存拷贝（估算每次几毫秒）。

### 选择

用 frizbee，钉死版本（`=0.13.0`）：

- 本文实测是 nucleo-matcher 的 3–4 倍；作者在 140 万条路径上的数字与此一致。它是 MIT 许可，没有运行时依赖，2026-08 仍在发布。
- 并行由 Lectern 在 `executor` 上做：把路径切成若干片，每片一个任务调 `match_list`，片之间检查取消标志，各片取前 k 个再归并。不调 `match_list_parallel`，因为它每次调用都新开操作系统线程，而后台线程只由 `executor` 管理（[executor 设计](../design/executor.md)）。同理不用高层 `nucleo`，它自带 rayon 线程池。
- 弱点是不做 Unicode 规范化。macOS 上输入的文件名可能是 NFD，git index 里的路径在开启 `core.precomposeUnicode` 时是 NFC；查询串和路径在进入匹配器前统一成 NFC 即可。
- 备选是 nucleo-matcher（MPL-2.0）：Unicode 处理更完整，但最后一次发布在 2024-02，速度慢 3–4 倍。

## 3. 排序的产品规则

匹配器只回答「像不像」。审阅场景里，用户要找的文件大多是本次审阅涉及的文件或刚看过的文件，排序要用上这些信息。

### 分层

1. 路径或文件名与查询完全相同。
2. 文件名命中：查询（或其中最后一段）连续地落在文件名里。VS Code 和 Zed 都这样做（见上表）。
3. 只命中路径。

查询里带 `/` 时按完整路径匹配，不分 2、3 层。

### 层内加分

| 信号 | 来源 | 建议 |
|---|---|---|
| 本次审阅改到的文件 | `review` 的改动集 | 加分最多；它们也是空查询的第一组 |
| agent 在本会话里写过、读过的文件 | `agents` 的工具调用事件（[ADR 0020](../adr/0020.md)） | 写过的加分高于读过的 |
| 最近打开（frecency） | Lectern 自己记录的打开历史 | 按 zoxide 的做法：每次打开 +1，1 小时内 ×4，1 天内 ×2，1 周内 ×0.5，更早 ×0.25；总分超过上限时整体缩小（[zoxide 算法](https://github.com/ajeetdsouza/zoxide/wiki/Algorithm)） |
| 同一子项目 | `project` 的 `locate` | 当前文件或当前审阅所在的子项目 |
| 与当前文件的路径距离 | 去掉公共前缀后两边剩余的段数 | Zed 的 `distance_to_relative_ancestor` 同理；作为同分时的次序 |
| 测试与源文件配对 | 命名规则 | 当前文件的对应测试（或被测文件）加分；规则与上下文面板的 Tests 共用（[#20](https://github.com/SoloJiang/lectern-app/issues/20)） |
| 噪声文件 | `review` 的噪声文件列表、`.gitattributes` 的 `linguist-generated` | 生成文件、lockfile、快照降权，不隐藏 |

规则：

- 语境加分随查询变长而减小。查询只有一两个字符时匹配分数区分不了候选，主要靠语境；查询越长，越以匹配分数为准，避免一个最近打开过的文件压过用户已经打全名的文件。
- 加分上限低于层与层之间的差距，第 3 层的结果不会因为加分排进第 2 层。
- 具体权重用夹具定：从 Lectern 自己的仓库和 `cloudprint-miniapps` 形状里取查询和期望的第一名，写成快照测试。

测试与源文件的配对规则（首发）：Java `src/main/java/…/X.java` ↔ `src/test/java/…/XTest.java`；TS/JS `foo.ts` ↔ `foo.test.ts`、`foo.spec.ts`、`__tests__/foo.ts`；Go `foo.go` ↔ `foo_test.go`；Python `foo.py` ↔ `test_foo.py`；Rust 的同文件 `mod tests` 和 crate 下的 `tests/`。

### 空查询

不列全部文件。按顺序显示：本次审阅改到的文件（按改动叙事的顺序，最多 8 个）、最近打开的文件（最多 8 个）、当前文件的对应测试或源文件。没有打开审阅时只显示后两组。

## 4. 符号索引的结构

### 方案对比

| 方案 | 查找 | 构建与增量 | 体积 | 与 blob OID 键的契合 |
|---|---|---|---|---|
| ctags 表 | 按名字排序的文本行，二分查找；重复名字的选择不确定（[tags(5)](https://github.com/universal-ctags/ctags/blob/master/man/tags.5.rst.in)） | 整份重写，没有增量格式 | 小 | 差：一份表对应一个目录树 |
| tree-sitter tags 查询 | 不是存储，是抽取：`@definition.*`、`@reference.*`、`@name`、`@doc`（[tree-sitter 代码导航](https://github.com/tree-sitter/tree-sitter/blob/master/docs/src/4-code-navigation.md)）；GitHub 的基于搜索的导航用它 | 每个文件独立，结果只取决于文件内容 | 定义约 1,560 个/MB 源码（本文实测，Rust） | 好：输入只有文件内容 |
| SQLite B-tree | 精确和前缀查找 < 0.1 ms（本文实测） | 每个 blob 的行一次插入或删除 | 9.5 万个定义 6.1 MB（本文实测） | 好：行以 blob 为键；仓库分库已按公共目录区分 |
| SQLite FTS5 trigram | 子串匹配 0.3–0.6 ms，全表 `LIKE` 6–7.5 ms（本文实测）；查询少于 3 个字符时用不上 | 随表增量 | 名字索引让文件从 6.1 MB 涨到 11.2 MB（本文实测） | 同上 |
| `fst`（mmap） | 精确 0.22 µs，前缀 0.01–0.06 ms，子序列（模糊）7–9 ms（本文实测，2.9 万个名字） | 不可修改，只能重建或合并；rust-analyzer 对每个库建一份、对工作区的每个文件建一份，查询时联合遍历（[symbol_index.rs](https://github.com/rust-lang/rust-analyzer/blob/master/crates/ide-db/src/symbol_index.rs)） | 2.9 万个名字 0.32 MB，另加位置表 0.76 MB | 一般：要按 worktree 合并出段，更新要重建段 |
| 三元组索引（zoekt、Google Code Search） | 子串和正则，取最稀有的两个三元组求交集 | 可以增量 | 约为语料的 3.5 倍，查询需要约 1.2 倍语料的内存（[zoekt 设计](https://github.com/sourcegraph/zoekt/blob/main/doc/design.md)） | 一般：zoekt 按分支位图共享同一份 blob |
| IntelliJ stub 与 file-based index | 按键查 PSI 元素或文件 | 每个文件独立；索引器的输出必须只取决于输入内容，否则依赖变化时不会重建（[stub 索引](https://github.com/JetBrains/intellij-sdk-docs/blob/main/topics/basics/indexing_and_psi_stubs/stub_indexes.md)、[file-based 索引](https://github.com/JetBrains/intellij-sdk-docs/blob/main/topics/basics/indexing_and_psi_stubs/file_based_indexes.md)） | stub 只含对外可见的声明 | 好：与「以内容为键」同一个前提 |
| rust-analyzer（salsa） | 按需计算的查询 | 改函数体不让全局派生数据失效；每个文件的 `ItemTree` 只摘要声明（[架构文档](https://github.com/rust-lang/rust-analyzer/blob/master/docs/book/src/contributing/architecture.md)） | 进程内 | 需要完整的语言前端，超出 Lectern 的范围 |
| stack graphs（GitHub） | 名字解析规则按文件独立地构建，查询时拼接 | 增量、按文件 | — | 好，但仓库已于 2025-09-09 归档，GitHub 不再维护（[github/stack-graphs](https://github.com/github/stack-graphs)） |
| SCIP | 预先生成的精确索引，按文档组织，压缩后约为原大小的 10%–20%（[SCIP 设计](https://github.com/sourcegraph/scip/blob/main/docs/DESIGN.md)） | 由外部索引器（scip-java 等）生成，Lectern 只导入 | — | 按提交对应，不按 blob；适合作第 4 层 |

### 选择

- **抽取**：tree-sitter tags 查询，每个 blob 一次，输出定义（名字、种类、行列、所在作用域）和引用名。Java 解析器（第 2 层）同样只读一个文件，按 blob 缓存 package、import 和顶层类型。两者都满足 IntelliJ 的那条规则：结果只取决于文件内容。
- **存储**：`cache` 的仓库分库里加自定义表，这是 cache 设计已经预留的做法（[cache 设计「范围」](../design/cache.md#范围)）。要点：
  - 一张 blob 表：OID、语言、grammar 和查询的版本、最后一次出现在任何 worktree 里的时间。
  - 定义表按小写名字建 B-tree 索引；引用表只存「名字 + blob」，每个 blob 每个名字一行，不存位置。位置在打开抽屉时对候选文件现场抽取。
  - 语料里引用约是定义的 2.9 倍（本文实测），不存位置可以把引用表压到与定义表同一量级。
- **按 worktree 过滤**：`git` 的 `TrackedPaths` 每条带 OID（[git 设计「index」](../design/git.md#index)），未暂存的修改由工作区比较算出 OID。当前 worktree 在内存里保留一份「blob → 路径」的映射（估算每条 12–32 字节，10 万文件 1–3 MB）。查询先按名字取出所有 blob，再只留当前 worktree 里有的。11 个 worktree 共用同一份索引，只有内容不同的文件各索引一次。
- **⌘K 的符号搜索**：第一次打开 ⌘K 时，从定义表取出当前 worktree 的去重名字，放进一个连续数组，用与文件查找相同的 frizbee 匹配。2.9 万个名字的子序列匹配，`fst` 要 7–9 ms，按第 2 节的吞吐估算 frizbee 在 1 ms 以内。
- **不引入 `fst`**：精确和前缀查找 SQLite 已经够快，模糊查找 frizbee 更快，而 `fst` 不可修改，增量更新要另做分段合并。等实测发现 SQLite 查找超出预算再考虑。
- **不建三元组索引**：名字的子串搜索由模糊匹配覆盖；对文件内容建三元组索引约为语料的 3.5 倍，不属于审阅闭环的核心。全文搜索按需扫描文件，见第 5 节。

规模估算（Rust 语料的密度，其他语言待实测）：`cloudprint-miniapps` 形状几 MB 的源码，索引 1 MB 左右；10 万文件按平均 8 KB 计约 800 MB 源码，定义表约 80 MB，引用表约 100–150 MB，合计在仓库分库的 512 MiB 预算之内，但会挤占 hunk 缓存，需要单独的份额（见「对现有设计的影响」）。

## 5. 不阻塞首帧的建索引策略

### 吞吐

本文实测：tree-sitter 解析 Rust 约 8.4 MB/s，加上 tags 查询约 4.3 MB/s（单线程）。4,600 个文件、60.6 MB 共抽出 94,709 个定义和 271,740 个引用。最慢的是一个 1.08 MB 的生成文件，用了 465 ms。

估算：`cloudprint-miniapps` 形状单核 1–2 秒；10 万文件约 190 个核·秒，3 个后台核约一分钟。M 系列的单核可能快一倍，这一点没有实测。技术调研写的「打开项目后数秒」只对中小仓库成立。

### 顺序

后台队列按优先级分层，高层的任务总是先做：

1. 当前打开的文件，以及抽屉里正在看的候选文件：按需、交互优先级。
2. 本次审阅改到的文件、agent 本会话碰过的文件，以及这些文件导入的文件。
3. 当前子项目。
4. 当前 worktree 的其余文件。
5. 其他 worktree 里 OID 不在索引中的文件；内容相同的文件已经有结果。

### 调度与取消

- 任务在 `executor` 的 `Background` 优先级上运行，每批约 64 个文件或 50 ms，批之间检查取消标志并让出，交互任务插队。macOS 上后台线程用低 QoS（由 `platform` 提供），用户操作时不与渲染争 CPU。
- 单个文件的解析带截止时间，用 tree-sitter 的进度回调取消（与 [ADR 0018](../adr/0018.md) 对 UI 路径 8 ms 截止时间的做法相同）。超时的文件记下 OID，不再自动重试，只在被打开时按需抽取。超过大小上限（建议 1 MB）的文件只在被打开时抽取。
- 切换 worktree 或分支时队列整体重排；已完成的结果按 OID 留在缓存里，不浪费。

### 增量更新

- `fs_watch` 的 `WorktreeBatch` 给出变化的路径：重新算 OID，OID 变了才抽取，旧 blob 的行留给淘汰。
- `rescan` 或 `meta.index` 为真时，重读 `TrackedPaths`，只对新出现的 OID 排队。
- agent 连续写文件时，fs_watch 已经合并批次（`Coalesce::WORKTREE`）；索引任务不再另加等待。

### 持久化与重新打开

- 以 OID 为键的结果在 SQLite 里，重新打开项目时只需找出缺的 OID：用 `TrackedPaths` 的 OID 列表批量查 blob 表，10 万条估算几百毫秒，在后台进行。
- 「blob → 路径」映射和 ⌘K 名字表不持久化，打开项目后从 `TrackedPaths` 和定义表重建。

### 索引不完整时

跳转到定义依次尝试，前一步有确定结果就停：

1. 当前文件：局部变量和本文件的定义（tags 的 locals 查询；当前文件因为高亮已经解析过）。
2. 当前文件导入的文件：按语言规则解析出路径，现场抽取（估算每 10 KB 约 1–2 ms）。
3. 已建好的索引。
4. 限时搜索：在当前子项目里还没索引的文件中，按整词查找这个标识符（字节串搜索，GB/s 量级），只对命中的文件抽取定义；限时 1 s，结果陆续追加。

呈现：结果直接进入抽屉，不显示「索引中」。后到的结果追加在已有结果之后，不重排用户正在看的条目；与第 4 层一样，结果与之前不同时才原地更新。是否在限时搜索没有覆盖全部文件时给出提示，见「未决问题」。

查找引用和调用方：用引用表找出候选 blob，现场抽取位置；未索引的部分同样走限时搜索。

### 常驻内存

- SQLite 只用 `cache` 已有的连接和每连接 1 MiB 页缓存（[cache 设计「连接与内存」](../design/cache.md#连接与内存)）。不开 SQLite 的 mmap（[ADR 0027](../adr/0027.md)），索引在磁盘上，不进常驻内存。
- 内存里常驻的只有「blob → 路径」映射和后台队列。⌘K 名字表在 ⌘K 关闭 30 秒后释放。
- 解析树只保留当前文件的；最近看过的文件保留高亮结果和文本，放进按字节计的 LRU。
- 内存基准要分别测「索引进行中」和「索引完成后静止」两种状态。

### 与「静止」的冲突

kernel 设计把「建索引」列为要持有 `BusyGuard` 的初始化工作（[kernel 设计「静止」](../design/kernel.md#静止)），而 ADR 0017 规定首帧后 30 秒内没有静止就算失败。10 万文件的仓库要索引一分钟左右，两者不能同时成立。可选的做法：

- (a) 第 1、2 层优先级的工作照常持有守卫；第 3–5 层是可以随时暂停的后台工作，不持有守卫，另由 kernel 报告「后台工作进行中」。内存基准在静止后先暂停后台工作再采样，并另设一项「索引完成后」的内存基准。
- (b) 默认只自动索引到第 3 层，其余在用户第一次跳转或打开 ⌘K 时才开始。

倾向 (a)：用户第一次跳转时就能用全仓库的结果。这改变了 ADR 0017 的测量方式，需要新 ADR。

## 6. 没有语言服务器时的定义准确率

### 来源的做法

- **Sourcegraph 基于搜索的跳转**：先在当前仓库的当前提交里做符号搜索，没有结果才搜其他仓库；丢掉在其他文件里定义的私有符号；按语言的规则过滤（文件扩展名，部分语言按文件开头的 import）；按路径段的 Jaccard 相似度排序，同路径的排第一（[providers.ts](https://github.com/sourcegraph/sourcegraph-public-snapshot/blob/main/client/shared/src/codeintel/legacy-extensions/search/providers.ts)）。它的文档承认 `Get` 这类常见名字更容易跳错。
- **GitHub 基于搜索的导航**：用 tree-sitter tags 抽取定义和引用，按名字把引用连到定义（[GitHub Docs](https://github.com/github/docs/blob/main/content/repositories/working-with-files/using-files/navigating-code-on-github.md)）。为了按作用域精确解析，它曾经做了 stack graphs，现已归档。

### Lectern 的候选排序

同名的候选按下面的顺序分层，同层内按路径距离排序：

1. 当前文件里的局部定义和本文件定义。
2. 当前文件的 import 指向的文件：Java 按 import 和全限定名，TS 按相对路径、tsconfig `paths` 和工作区包名（[#19](https://github.com/SoloJiang/lectern-app/issues/19)），Rust 按 `use` 和 `mod` 路径。
3. 同包或同目录：Java 的同一 package，其他语言的同一目录。
4. 同一子项目。
5. 当前子项目依赖的子项目（清单里的依赖坐标）。
6. 仓库其余部分。

过滤和消歧：

- 只留同语言的候选，丢掉其他文件里的私有符号。
- 按调用位置的语法种类筛选：调用处找函数或方法，类型位置和 `new` 后面找类型。
- Java 同名方法按所在类和参数个数消歧（[技术调研「代码导航与编辑」](../design/tech-research.md#代码导航与编辑)）。
- 本次审阅改到的文件里的定义在同层内优先：审阅时被点的符号多半与本次改动有关。

第一层只有一个候选时直接跳转，否则在抽屉里列出全部候选，第一项预选。

### 何时升级到 jdtls 或 SCIP

- 仓库里有与 HEAD 对应的 SCIP 索引文件（例如 CI 用 scip-java 生成）：直接导入，不需要启动进程。
- Java 的方法调用经过上述分层后，第一层仍有多个候选（重载、接口的多个实现），或用户执行「查找实现」：按需启动 jdtls（可选插件，空闲即关，[ADR 0018](../adr/0018.md)），结果与前面不同时原地更新抽屉。
- 其他语言暂不接语言服务器。

## 7. 预测与预先计算

| 做法 | 收益 | 代价 | 建议 |
|---|---|---|---|
| 打开会话时，预先解析并高亮本次审阅改到的文件 | 审阅时打开完整文件、展开上下文都命中缓存 | 与改动集大小成正比，50 个文件估算几十毫秒 CPU；结果按 blob 存磁盘 | 做；与索引的第 2 层同一批 |
| 预先高亮「下一个未看文件」 | 按 V 之后立即显示 | 一个文件 | 做 |
| 光标或指针在符号上停留约 150 ms 后预取定义候选 | 按 Space 或点击时抽屉内容已就绪 | 一次 SQLite 查询，候选文件读入页缓存 | 做；只查不渲染 |
| 预取唯一候选的目标文件高亮 | 跳转一帧内完成 | 一个文件的解析 | 只对第一候选做 |
| 保留最近看过文件的高亮结果和文本 | 后退、前进一帧内完成 | 按字节的 LRU，建议 8 MB | 做；解析树不保留 |
| 打开文件时预解析它导入的文件 | 跳转到导入的符号更快 | 每个导入一次抽取 | 只对当前文件做，不递归 |
| 启动时建 ⌘K 名字表 | 第一次 ⌘K 少几十毫秒 | 常驻内存（10 万文件估算 4–8 MB） | 不做；第一次打开 ⌘K 时建，空查询列表不需要它 |
| 后台索引整个仓库 | 全仓库跳转 | 一分钟级的 CPU；电池 | 做，低 QoS、可暂停，见第 5 节和「未决问题」 |

所有预测性工作都在后台优先级上运行，用户输入时让出；不在首帧之前做任何一项（[ADR 0017](../adr/0017.md)）。

## 8. 文件树浏览

### 从 git index 懒构建

- `FileList` 按字节序排序，一个目录下的文件是连续的一段，`range_under` 二分查找就能取到（[project 设计「文件列表」](../design/project.md#文件列表)）。
- 但直接展开根目录要扫描整个列表来合并下一级目录名，10 万条估算几毫秒，100 万条超过一帧。所以在文件列表就绪后，于后台建一张目录表：每个目录记下它在 `FileList` 里的区间、子目录的起点和个数。一遍扫描完成，估算 10 万条 5–10 ms、约 1 MB，随 `FileList` 的每一代重建。
- 展开目录只读目录表和区间，耗时与子项数成正比。

### 虚拟化渲染

- 用 GPUI 的 `uniform_list`，它只渲染可见的行，行高取第一行的测量值（[uniform_list.rs](https://github.com/zed-industries/zed/blob/main/crates/gpui/src/elements/uniform_list.rs)）。视图持有的快照是「当前展开状态下的可见行」：每行一个目录表或文件列表的下标加缩进层级。展开或折叠只插入或删除这段子项对应的行。
- 不把整个仓库建成 gpui-component 的 `TreeItem` 树。0.7.3 的 `TreeItem` 每个节点有两个 `SharedString`、一个子节点 `Vec` 和一个 `Rc<RefCell>` 状态，`TreeState` 把展开的子树展平成条目（gpui-base 0.7.3 `tree.rs`）。10 万个节点估算超过 10 MB，并且要在主线程上构造。

### 排序与折叠

- 目录在前，文件在后；同类按 Finder 的规则排序（不区分大小写、数字按数值比较）。Finder 的比较来自系统 API（`localizedStandardCompare`），要么经 `platform` 调用，要么在 Rust 里实现同样的规则，由 Files 的设计文档决定。
- 只有一个子目录的目录链折叠成一行（如 Java 的 `src/main/java/com/example`）。Zed 有同样的设置（`auto_fold_dirs`），并修过它在大目录上的性能问题（[PR #8556](https://github.com/zed-industries/zed/pull/8556)）。
- 本次审阅改到的文件带一个系统色圆点，所在目录显示同样的圆点，折叠时也能看到改动在哪里。
- 当前在看的文件在树里自动定位并展开到它。

### 被忽略的目录

- 默认不显示。用户打开「显示被忽略的文件」后，被忽略的目录作为可展开的行出现；展开时才用 `SafeRoot::list` 列出这一级（[ADR 0028](../adr/0028.md)），每次最多 2,000 项，其余显示为「还有 N 项」。
- 被忽略的文件不进索引、不被监听、不进 ⌘K。Zed 也是默认不扫描被忽略的目录，展开后才加载（[PR #2622](https://github.com/zed-industries/zed/pull/2622)），文件查找只匹配已经展开过的被忽略条目（[PR #3748](https://github.com/zed-industries/zed/pull/3748)）。

## 对 Lectern 的建议

### 算法与数据结构

| 部分 | 建议 | 落在哪里 |
|---|---|---|
| 文件模糊匹配 | frizbee `=0.13.0`；在 `executor` 上分片调用 `match_list`，片间检查取消，取前 k 个后归并；查询以上一轮为前缀时只重打分上一轮的命中；路径和查询先统一为 NFC | 新的匹配模块（随 ⌘K 的设计决定放在 `code_intel` 还是单独的 crate） |
| 路径分层 | 完全相同 > 文件名命中 > 路径命中；文件名命中对 `FileList` 里的文件名切片单独跑一遍 | 同上 |
| 路径语料 | 20 万条以内沿用 `FileList`；更大的仓库改为按目录去重或前缀压缩，先在 bench 里用 100 万条的合成仓库测 | `project` |
| 目录表 | 文件列表就绪后在后台建，供 Files 展开 | `project` |
| 符号抽取 | tree-sitter tags，按 blob 一次；Java 的 package、import 同样按 blob | `code_intel` |
| 符号存储 | `cache` 仓库分库里的自定义表：blob 表、定义表（小写名字 B-tree）、引用表（名字 + blob） | `code_intel`、`cache` |
| 符号查询 | 名字 → blob → 当前 worktree 的路径；⌘K 用去重名字数组 + frizbee | `code_intel` |
| 不采用 | `fst`、FTS5 trigram、内容三元组索引、高层 `nucleo`、Zed `fuzzy` | — |

### 产品规则

- ⌘K 排序：分层，层内按「审阅改动 > agent 写过 > 最近打开 > agent 读过 > 同一子项目 > 路径距离」加分，加分随查询变长减小；噪声文件降权。
- ⌘K 空查询：本次审阅的改动文件、最近打开的文件、当前文件的测试或源文件。
- 跳转：候选按「本文件 > import > 同包 > 同子项目 > 依赖的子项目 > 其余」分层；唯一候选直接跳，多个候选在抽屉里列出。
- 索引未完成：按第 5 节的顺序回答，结果追加，不显示状态。
- 文件树：目录表加虚拟列表，Finder 排序，单子目录折叠，改动圆点，被忽略的目录按需列出。

### 预算

延迟预算见第 1 节。内存和磁盘（估算，实现时用 bench 校准）：

| 项 | `cloudprint-miniapps` 形状 | 10 万文件 |
|---|---|---|
| 路径列表（`project` 已有） | < 0.1 MB | 约 6 MB |
| 目录表 | < 0.1 MB | 约 1 MB |
| ⌘K 匹配期间的临时内存（命中下标、每片的临时矩阵） | < 1 MB | 约 2 MB，关闭后释放 |
| blob → 路径映射 | < 0.1 MB | 1–3 MB |
| ⌘K 名字表 | < 0.1 MB | 4–8 MB，⌘K 关闭 30 秒后释放 |
| 高亮与文本 LRU | 上限 8 MB | 上限 8 MB |
| `code_intel` 空闲常驻合计 | ≤ 10 MB | ≤ 20 MB |
| 磁盘上的符号表 | 约 1 MB | 约 200–250 MB，在仓库分库内单独计份额 |

## 对现有设计的影响

| 文档或 issue | 变化 |
|---|---|
| 新 ADR：代码导航的依赖与预算 | 引入 frizbee `=0.13.0`（MIT）和 tree-sitter-tags；写明不用高层 `nucleo`、Zed `fuzzy`、`fst` 的理由；把第 1 节的延迟预算加进[性能预算](../design/tech-research.md#性能预算)表（顶层预算的改动要写 ADR，见 [bench 设计「预算表」](../design/bench.md#预算表)），加入 `bench` 的 ⌘K、跳转和后退基准 |
| 新 ADR：后台索引与静止 | 第 5 节的冲突：哪些后台工作不持有 `BusyGuard`，内存基准在什么状态下采样；修订 ADR 0017 的测量方式和 [kernel 设计「静止」](../design/kernel.md#静止) |
| [技术调研「代码导航与编辑」](../design/tech-research.md#代码导航与编辑) | 第 1 层的「`notify` 增量更新」改为经 `fs_watch`（fs_watch 不用 `notify`）；「打开项目后数秒」改为按第 5 节的优先级分段；补充「索引不完整时」的回答顺序 |
| [技术调研「性能预算」](../design/tech-research.md#性能预算)、`crates/file_reader/AGENTS.md` | 「打开文件：mmap 读取」与 platform 设计「不用 mmap」（并发截短会触发 SIGBUS）矛盾，应以 platform 为准，改为经 `SafeRoot` 读取 |
| [cache 设计](../design/cache.md) | 增加「自定义表」一节：符号表的结构、在仓库分库内的单独份额（避免挤掉 hunk 缓存）、按「最后一次出现在 worktree 里」淘汰 blob 的规则 |
| [project 设计](../design/project.md) | 目录表；文件列表超过 20 万条时的布局与内存预算；`bench` 增加 100 万条路径的合成仓库 |
| [交互规范](../design/interaction.md) | ⌘K 的分层排序、空查询内容、结果分组；Files 的折叠、改动圆点、被忽略文件的开关；后退、前进的快捷键 |
| [#17](https://github.com/SoloJiang/lectern-app/issues/17) | 按 blob 抽取和存储、建索引的优先级、增量更新、回答顺序，写进 `docs/design/code_intel.md` |
| [#18](https://github.com/SoloJiang/lectern-app/issues/18)、[#19](https://github.com/SoloJiang/lectern-app/issues/19) | Java 解析器和跨子项目解析的结果同样按 blob 缓存；它们提供第 6 节第 2、5 层的候选 |
| [#20](https://github.com/SoloJiang/lectern-app/issues/20) | 抽屉的结果追加、不重排的规则；测试与源文件配对规则与 ⌘K 共用 |
| [#26](https://github.com/SoloJiang/lectern-app/issues/26) | 第 6 节的升级条件 |
| [#10](https://github.com/SoloJiang/lectern-app/issues/10) | Files 的目录表与虚拟列表；⌘K 目前没有对应的 issue，建议新开一个，范围是文件和符号的查找与排序 |
| 最近打开记录 | 这是用户数据，不能随缓存删除，需要一个归属（例如与已看状态同一个数据库） |

## 未决问题（需要产品决定）

1. **⌘K 的排序优先级。** 默认：本次审阅改到的文件 > agent 写过的文件 > 最近打开的文件 > agent 读过的文件。需要确认 agent 只读过的文件是否加分，以及最近打开的记录是否跨启动保存（只存在本机，按项目）。默认保存。
2. **⌘K 空查询显示什么。** 默认：本次审阅的改动文件（最多 8 个）、最近打开的文件（最多 8 个）、当前文件的测试或源文件。另一种做法是只显示最近打开的文件。
3. **⌘K 的结果按类型分组还是混排，是否支持前缀。** 默认：按文件、符号、会话、命令分组；支持 `@` 只搜符号、`>` 只搜命令。
4. **索引没建完时是否提示。** (a) 不提示，符合现有「不显示索引状态」的规则（默认）；(b) 只在限时搜索没有覆盖全部文件、结果可能不全时，在结果末尾显示一行「仍在搜索」。
5. **首发语言范围。** 默认：符号抽取覆盖 ADR 0018 核心里的 TypeScript / JavaScript、Rust、Java、Python、Go；按 import 解析先做 Java 和 TS（#18、#19），其余语言只用同文件、同目录、同子项目的分层。
6. **大仓库和电池供电时的索引范围。** 默认：接电源时索引整个仓库；用电池时只自动索引审阅集合和当前子项目，其余等接上电源或用户跳转时再继续。
7. **Files 在 monorepo 里的起点，以及被忽略文件的可见范围。** 默认：从仓库根开始，自动展开到当前子项目；被忽略的文件只在 Files 里打开开关后显示，⌘K 不搜索它们。
8. **后退、前进的快捷键。** ⌘[ 已用于「返回 Overview」。默认用 Xcode 的 ⌃⌘← / ⌃⌘→。

## 实测说明

- 机器：Linux 容器，Intel Xeon 2.1 GHz，4 个 vCPU，release 构建（thin LTO）。不是 M 系列 Mac，数字只用于比较方案之间的量级，不能当作 Lectern 的预算实测。
- 版本：frizbee 0.13.0、nucleo-matcher 0.3.1、fst 0.4.7、tree-sitter 0.27.1、tree-sitter-tags 0.27.1、tree-sitter-rust 0.24.2、rusqlite 0.40.2（自带 SQLite，WAL，`cache_size = -1024`，`mmap_size = 0`）。
- 路径语料：本机 cargo 缓存和 `/usr` 下的 140,565 条路径；98 万条是它的 7 份复制，各加不同前缀。查询：`mod`、`srclib`、`cargotoml`、`rsfmt`、`tokiort`、`zzqx`。每项取 3–5 次的最好值。
- 符号语料：cargo 缓存里按路径排序的前 4,600 个 Rust 文件（60.6 MB，跳过超过 2 MB 的文件），Rust 的 `tags.scm`。SQLite 精确查找的平均 76 µs 包括取回结果行，抽样里有 `new` 这类常见名字，平均每次 168 行；`fst` 的 0.22 µs 只查到位置表的偏移，两者不能直接比较。
- 基准代码不在仓库里；需要时在 `bench` 里按 [bench 设计](../design/bench.md)重写。

## 来源

项目源码与文档：

- [helix-editor/nucleo](https://github.com/helix-editor/nucleo)（README、[`src/lib.rs`](https://github.com/helix-editor/nucleo/blob/master/src/lib.rs)、[`src/pattern.rs`](https://github.com/helix-editor/nucleo/blob/master/src/pattern.rs)、[`matcher/src/lib.rs`](https://github.com/helix-editor/nucleo/blob/master/matcher/src/lib.rs)）
- [saghen/frizbee](https://github.com/saghen/frizbee)、[BENCHMARKS.md](https://github.com/saghen/frizbee/blob/main/BENCHMARKS.md)
- [junegunn/fzf `src/algo/algo.go`](https://github.com/junegunn/fzf/blob/master/src/algo/algo.go)
- [forrestthewoods/lib_fts `fts_fuzzy_match.h`](https://github.com/forrestthewoods/lib_fts/blob/master/code/fts_fuzzy_match.h)
- [microsoft/vscode `fuzzyScorer.ts`](https://github.com/microsoft/vscode/blob/main/src/vs/base/common/fuzzyScorer.ts)
- Zed：[`crates/fuzzy/Cargo.toml`](https://github.com/zed-industries/zed/blob/main/crates/fuzzy/Cargo.toml)、[`crates/fuzzy/src/paths.rs`](https://github.com/zed-industries/zed/blob/main/crates/fuzzy/src/paths.rs)、[`uniform_list.rs`](https://github.com/zed-industries/zed/blob/main/crates/gpui/src/elements/uniform_list.rs)、PR [#2622](https://github.com/zed-industries/zed/pull/2622)、[#3748](https://github.com/zed-industries/zed/pull/3748)、[#8556](https://github.com/zed-industries/zed/pull/8556)、[#27937](https://github.com/zed-industries/zed/pull/27937)、[#28093](https://github.com/zed-industries/zed/pull/28093)
- [helix-term `ui/mod.rs`](https://github.com/helix-editor/helix/blob/master/helix-term/src/ui/mod.rs)
- [dmtrKovalenko/fff.nvim](https://github.com/dmtrKovalenko/fff.nvim)
- [zoxide 算法](https://github.com/ajeetdsouza/zoxide/wiki/Algorithm)
- [BurntSushi/fst](https://github.com/BurntSushi/fst)
- [rust-analyzer 架构文档](https://github.com/rust-lang/rust-analyzer/blob/master/docs/book/src/contributing/architecture.md)、[`ide-db/src/symbol_index.rs`](https://github.com/rust-lang/rust-analyzer/blob/master/crates/ide-db/src/symbol_index.rs)
- [tree-sitter 代码导航文档](https://github.com/tree-sitter/tree-sitter/blob/master/docs/src/4-code-navigation.md)
- [universal-ctags tags(5)](https://github.com/universal-ctags/ctags/blob/master/man/tags.5.rst.in)
- [sourcegraph/zoekt 设计文档](https://github.com/sourcegraph/zoekt/blob/main/doc/design.md)、[README](https://github.com/sourcegraph/zoekt)
- [sourcegraph/scip](https://github.com/sourcegraph/scip)、[SCIP 设计文档](https://github.com/sourcegraph/scip/blob/main/docs/DESIGN.md)
- [Sourcegraph 基于搜索的定义查找（`providers.ts`）](https://github.com/sourcegraph/sourcegraph-public-snapshot/blob/main/client/shared/src/codeintel/legacy-extensions/search/providers.ts)、[sourcegraph-basic-code-intel](https://github.com/sourcegraph/sourcegraph-basic-code-intel)（2024-03-18 归档）
- [github/stack-graphs](https://github.com/github/stack-graphs)（2025-09-09 归档）
- [GitHub Docs：Navigating code on GitHub](https://github.com/github/docs/blob/main/content/repositories/working-with-files/using-files/navigating-code-on-github.md)
- IntelliJ Platform SDK：[Indexing and PSI Stubs](https://github.com/JetBrains/intellij-sdk-docs/blob/main/topics/basics/indexing_and_psi_stubs.md)、[Stub Indexes](https://github.com/JetBrains/intellij-sdk-docs/blob/main/topics/basics/indexing_and_psi_stubs/stub_indexes.md)、[File-Based Indexes](https://github.com/JetBrains/intellij-sdk-docs/blob/main/topics/basics/indexing_and_psi_stubs/file_based_indexes.md)
- crates.io 上的版本与许可：[frizbee](https://crates.io/crates/frizbee)、[nucleo](https://crates.io/crates/nucleo)、[nucleo-matcher](https://crates.io/crates/nucleo-matcher)、[fst](https://crates.io/crates/fst)、[tree-sitter-tags](https://crates.io/crates/tree-sitter-tags)

人机交互阈值：

- [Nielsen Norman Group：Response Times, The 3 Important Limits](https://www.nngroup.com/articles/response-times-3-important-limits/)
- [web.dev：RAIL 模型](https://web.dev/articles/rail)

调研环境访问不到以下站点，上面这两条的内容取自搜索引擎摘要；另有几项资料没能读到，结论不依赖它们：BurntSushi 的 fst 博客（burntsushi.net）、Russ Cox 的三元组索引文章（swtch.com）、Forrest Smith 的 Sublime 逆向文章原文（改读其 GitHub 源码）、sqlite.org 的 FTS5 文档（改用本文实测）、github.blog 的 stack graphs 介绍、rust-analyzer 博客「Three Architectures for a Responsive IDE」、docs.rs、JetBrains 与 Sourcegraph 的文档站（改读 GitHub 上的源文件）。
