# doc_check（`lectern-doc-check`）

文档链接检查：仓库里所有 Markdown 文件的相对链接必须能打开，`#锚点` 必须对应目标文件里的某个标题。`just check` 运行它，见 [CI 与约束检查「文档链接检查」](../../docs/design/ci.md#文档链接检查)。

- 层：tool 层，只用于开发与 CI，可以依赖任何层。只有 `app` 可以依赖本 crate，本 crate 不进入发布构建。
- 设计文档：[ci.md「文档链接检查」](../../docs/design/ci.md#文档链接检查)。
- ADR：[0021](../../docs/adr/0021.md)（解析依赖的选型）

## 边界

- 先用 pulldown-cmark 按 CommonMark 把整篇 Markdown 渲染成 HTML（记下每段输出对应的源码位置），再用 html5gum 按 HTML 标准对渲染结果整体分词一次，不自己写正则或扫描器：链接（Markdown 链接和原始 HTML 的 `href` / `src`）、标题、`id` 与 `<a name>` 锚点、注释、字符引用都以分词结果为准。`script`、`style` 等原始文本元素（包括未闭合的、自闭合写法的，以及 `script` 的转义状态）里的内容都是文本，其中的链接和标题不算；`plaintext` 之后的全部内容都是文本。分词器不做树构建，跟不上需要树构建才能确定解析方式或含义的原始 HTML 元素：外来内容（`svg`、`math`）、可能吞掉后文的原始文本和 RCDATA 元素（`script`、`style`、`iframe`、`textarea`、`title` 等）、内容不渲染的 `template`、会忽略部分开始标签的 `select`、加载哪个 `source` / `track` 取决于树结构和可能被忽略的结束标签的 `audio`、`video`，嵌套时开始标签连同 `id` 被丢弃的 `form`，正文里开始标签被忽略或合并的 `html`、`head`、`body` 和原始 HTML 表格（表格部件在表格外被忽略，表格里的其他内容会被挪到表格前面），以及 `object`、`frame`。只检查源文件里的原始 HTML，Markdown 生成的表格、列表不受影响。文档里出现这些元素直接报错，图示用图片文件，录屏用链接，表格用 Markdown 表格；被拒绝的元素上的 `id` 不算锚点。`href` 只认 `a`、`area`、`link` 上的，`src` 只认 `img`、`embed` 和 `type=image` 的 `input` 上的；`source`、`track` 的 `src` 只在 `audio`、`video` 里加载，而这两个元素不支持，所以不读（`picture` 里的 `source` 用 `srcset`）。Markdown 标题只在 pulldown-cmark 生成的结束标签处结束；目标是不是 Markdown 按链接里写的名字判断（`alias.md` 指向 `real.txt` 时仍按 Markdown 检查片段）。
- 以 `/` 开头的链接（不是 `//`）按 GitHub 的规则相对仓库根目录解析。指向目录、带片段的链接（`docs/#section`）检查这个目录里 GitHub 会显示的 `README.md`；目录里没有 Markdown README 时没有锚点，这类链接算失败。只有片段的链接（`#section`）按当前文件本身的名字判断是否是 Markdown。
- 目标必须受 git 跟踪，而且沿途每一级都要受跟踪：经过符号链接时，链接本身和它指向的每一级路径都要受跟踪。作为输入的 Markdown 文件是符号链接时同样按它给出的路径逐级检查，读不出来的输入报错，不跳过（工作区里已删除、尚未提交的文件除外）。
- git 输出的路径按原始字节处理，不要求是 UTF-8。
- 锚点按 GitHub 的规则从标题生成，区分大小写；原始 HTML 里的 `id` 和 `<a name>` 也算锚点；原始 HTML 写的 `<h2>` 没有自动锚点（GitHub 只给 Markdown 标题生成）。链接不能指向仓库之外，`..` 一旦越过仓库根目录就算越界，即使后面又回到同一个目录（`../<检出目录名>/README.md`）。
- 只读文件，不改文件，不访问网络。

## 测试

不依赖 GPUI，在任何平台上都能编译和测试：

```sh
just test -p lectern-doc-check
cargo run -q -p lectern-doc-check   # 检查整个仓库
```

- 每种 Markdown 写法（引用式链接、容器里的标题、缩进代码块、跨行代码片段、带括号的路径、查询参数等）在单元测试里各有一条用例。
