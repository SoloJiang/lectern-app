# doc_check（`lectern-doc-check`）

文档链接检查：仓库里所有 Markdown 文件的相对链接必须能打开，`#锚点` 必须对应目标文件里的某个标题。`just check` 运行它，见 [CI 与约束检查「文档链接检查」](../../docs/design/ci.md#文档链接检查)。

- 层：tool 层，只用于开发与 CI，可以依赖任何层。只有 `app` 可以依赖本 crate，本 crate 不进入发布构建。
- 设计文档：[ci.md「文档链接检查」](../../docs/design/ci.md#文档链接检查)。
- ADR：[0021](../../docs/adr/0021.md)（解析依赖的选型）

## 边界

- 先用 pulldown-cmark 按 CommonMark 把整篇 Markdown 渲染成 HTML（记下每段输出对应的源码位置），再用 html5gum 按 HTML 标准对渲染结果整体分词一次，不自己写正则或扫描器：链接（Markdown 链接和原始 HTML 的 `href` / `src`）、标题、`id` 与 `<a name>` 锚点、注释、字符引用都以分词结果为准。`script`、`style` 等原始文本元素（包括未闭合的、自闭合写法的，以及 `script` 的转义状态）里的内容都是文本，其中的链接和标题不算；`plaintext` 之后的全部内容都是文本。分词器不做树构建，跟不上外来内容（`<svg>`、`<math>` 里的 CDATA，以及不按原始文本解析的 `style`、`script`），所以文档里的内联 SVG 和 MathML 直接报错，图示用图片文件。
- 以 `/` 开头的链接（不是 `//`）按 GitHub 的规则相对仓库根目录解析。指向目录、带片段的链接（`docs/#section`）检查这个目录里 GitHub 会显示的 `README.md`。
- 目标必须受 git 跟踪，而且沿途每一级都要受跟踪：经过符号链接时，链接本身和它指向的每一级路径都要受跟踪。作为输入的 Markdown 文件是符号链接时同样按它给出的路径逐级检查，读不出来的输入报错，不跳过（工作区里已删除、尚未提交的文件除外）。
- git 输出的路径按原始字节处理，不要求是 UTF-8。
- 锚点按 GitHub 的规则从标题生成，区分大小写；原始 HTML 里的 `id` 和 `<a name>` 也算锚点；原始 HTML 写的 `<h2>` 没有自动锚点（GitHub 只给 Markdown 标题生成）。链接不能指向仓库之外。
- 只读文件，不改文件，不访问网络。

## 测试

不依赖 GPUI，在任何平台上都能编译和测试：

```sh
just test -p lectern-doc-check
cargo run -q -p lectern-doc-check   # 检查整个仓库
```

- 每种 Markdown 写法（引用式链接、容器里的标题、缩进代码块、跨行代码片段、带括号的路径、查询参数等）在单元测试里各有一条用例。
