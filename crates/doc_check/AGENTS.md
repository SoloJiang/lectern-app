# doc_check（`lectern-doc-check`）

文档链接检查：仓库里所有 Markdown 文件的相对链接必须能打开，`#锚点` 必须对应目标文件里的某个标题。`just check` 运行它，见 [CI 与约束检查「文档链接检查」](../../docs/design/ci.md#文档链接检查)。

- 层：tool 层，只用于开发与 CI，可以依赖任何层。只有 `app` 可以依赖本 crate，本 crate 不进入发布构建。
- 设计文档：[ci.md「文档链接检查」](../../docs/design/ci.md#文档链接检查)。

## 边界

- Markdown 用 pulldown-cmark 按 CommonMark 解析，Markdown 里的原始 HTML 用 html5gum 按 HTML 标准分词，不自己写正则或扫描器：链接、标题、代码块、代码片段、HTML 注释、字符引用、`script` / `style` 的内容都以解析器为准。一个 HTML 块整体分词，跨行的标签和注释按一个处理。
- 目标必须受 git 跟踪；目标是符号链接时，解析后的真实路径也必须受跟踪。作为输入的 Markdown 文件是符号链接时同样如此，读不出来的输入报错，不跳过（工作区里已删除、尚未提交的文件除外）。
- git 输出的路径按原始字节处理，不要求是 UTF-8。
- 锚点按 GitHub 的规则生成，区分大小写；链接不能指向仓库之外。
- 只读文件，不改文件，不访问网络。

## 测试

不依赖 GPUI，在任何平台上都能编译和测试：

```sh
just test -p lectern-doc-check
cargo run -q -p lectern-doc-check   # 检查整个仓库
```

- 每种 Markdown 写法（引用式链接、容器里的标题、缩进代码块、跨行代码片段、带括号的路径、查询参数等）在单元测试里各有一条用例。
