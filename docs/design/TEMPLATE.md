# <crate> 设计

<!--
模块设计文档模板。复制为 docs/design/<crate>.md（<crate> 用目录名，如 review、usage_view），
按下面的小节填写，删掉不适用的小节和这段注释。
先提设计文档 PR，合并后再写代码（AGENTS.md「工作方式」）。
写接口和数据结构，不写实现细节；能用 Rust 签名说清楚的不用文字描述。
-->

对应 issue [#N](https://github.com/SoloJiang/lectern-app/issues/N)。一两句话说明这个 crate 解决什么问题，链接到交互规范或技术调研里的对应章节。

## 范围

- 做：本 crate 负责的事，按条列出。
- 不做：容易被误放进来、但属于别的 crate 的事，写明归哪个 crate。
- 本 issue 之外：留给后续 issue 的部分，写 issue 号。

## 依赖

| 依赖 | 用途 | 选用理由 |
|---|---|---|
| `lectern-xxx` | | |
| 外部 crate | | 许可证、体积、是否引入新的传递依赖 |

所在层与依赖方向见 crate 目录下的 AGENTS.md。新增外部依赖先跑 `just deny`；改变依赖选型的写 ADR。

## 接口

对外暴露的类型、trait 和函数，用 Rust 签名写：

```rust
pub struct Snapshot { /* 视图读取的不可变快照 */ }

pub enum Command { /* 视图发出的命令 */ }

pub trait Xxx {
    fn xxx(&self, input: &Input) -> Result<Output, Error>;
}
```

每个公开项说明：谁调用、在哪个线程调用、是否阻塞、错误时调用方怎么处理。

## 数据结构

核心类型的字段与不变量。从外部格式解析的（会话 JSONL、清单文件、git 对象）写明格式版本和兼容策略，样本放进 fixtures。

## 数据流与线程

- 数据从哪里来、经过哪些步骤、以什么快照交给视图（AGENTS.md 架构规则 4）。
- 哪些工作在 `executor` 的后台任务里做；UI 线程上不做 IO、解析和 git 操作。
- 派生数据的缓存键（git blob OID，架构规则 5）与失效条件。

## 性能

- 影响[性能预算](tech-research.md#性能预算)的哪几项，预期耗时与内存。
- 首帧之前是否需要做任何事（默认不做，首帧之后初始化）。
- 需要新增或调整的 `bench` 基准。

## 界面

视图层 crate 填写，其余删掉：

- 对应[交互规范](interaction.md)的哪个屏幕和哪一层（常驻 / 按需 / 收起）。
- 用到的 `ui` 控件与动效原语；不在视图里自定义颜色、字体和动画。
- 界面字符串的 Fluent 键（en 与 zh-CN 同时提供）。
- UI 探针的语义节点：角色与稳定 ID（`视图.区域.条目[业务键]`）。
- Action 与快捷键。

## 状态与异常

空状态、加载中、出错、无法识别数据格式时各自的表现，对照[交互规范「状态与异常」](interaction.md#状态与异常)。

## 测试

- 单元测试与 insta 快照覆盖哪些场景。
- 用到的 fixtures（放在 `crates/<crate>/tests/fixtures/`，真实数据先脱敏）。
- 视图：`#[gpui::test]` 的交互场景、语义树快照。
- 只能在 macOS 上跑的部分。

## 未决问题

需要人决定的问题单独列出，写清选项和倾向。

## 验收

可以逐条检查的完成条件，与 issue 的验收项一致。
