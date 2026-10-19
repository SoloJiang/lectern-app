//! 文本、位置、符号与候选（docs/design/code_intel.md「服务接口」）。

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::project::{BlobId, RelPath, WorktreeKey};

/// 一份确定的文本：用户看到的那一份内容。
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct TextRef {
    /// 文件夹项目为 None。
    pub worktree: Option<WorktreeKey>,
    pub path: RelPath,
    /// 这份文本的 blob OID（工作区一侧是规范内容的 OID，与 `git::Content::Blob`
    /// 相同；文件夹项目是原始字节的 SHA-1）。
    pub oid: BlobId,
    pub origin: TextOrigin,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum TextOrigin {
    /// 工作区里的当前内容。实现方经 `SafeRoot` 读，内容与 `oid` 对不上时查询返回
    /// `Stale`。
    Worktree,
    /// 对象库里的 blob，如 Diff 的旧一侧。实现方按 `oid` 从对象库读。
    Object,
}

/// 行和列都从 0 起；列是这一行里的字节偏移。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct Position {
    pub line: u32,
    pub column: u32,
}

/// 位置上的符号（`QueryResults::symbol`）。`Named` 查询只有名字。
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Symbol {
    pub name: Arc<str>,
    pub lang: LangName,
    pub syntax: RefSyntax,
    /// 光标就在一个定义的名字上时为这个定义：`References` 可以直接由它发起。
    pub defines: Option<DefinitionRef>,
}

/// 引用位置的语法种类，决定能接受哪些定义种类。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum RefSyntax {
    Plain,
    Call,
    MemberCall,
    Member,
    Type,
    Construct,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Candidate {
    pub definition: DefinitionRef,
    pub tier: CandidateTier,
    pub evidence: Evidence,
    /// 定义所在的文件在 `Focus::Review` 里。
    pub in_review: bool,
    pub source: CandidateSource,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct DefinitionRef {
    pub location: Location,
    pub name: Arc<str>,
    pub kind: DefKind,
    pub lang: LangName,
    /// 最近的外层定义的名字（方法所在的类、模块里的函数所在的模块）。
    pub scope: Option<Arc<str>>,
    /// locals 查询在非根作用域里给出的 `@local.definition`（局部变量、参数、嵌套的
    /// `const helper = () => …`）为 true，不论 tags 是否也捕获了它；只在所在文件的
    /// 这个作用域里可见，`References` 只扫描这个作用域。根作用域（程序、模块顶层）
    /// 的定义和只被 tags 捕获的定义为 false，按跨文件的方式查引用。只有来自解析的
    /// 值是确定的：来自索引的定义（跳转的索引候选、`Named`）一律为 false，意思是
    /// 不知道；`References` 不论它由谁给出，都按被查定义所在文件的解析重新确定。
    pub local: bool,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct Location {
    pub worktree: Option<WorktreeKey>,
    pub path: RelPath,
    /// 位置所在的内容来自工作区还是对象库。后续的 `References`、打开结果都按它读
    /// 内容：`Object` 按 `oid` 从对象库读，`Worktree` 读工作区并核对 OID。
    pub origin: TextOrigin,
    /// 位置所在的那份内容。视图打开前比较：文件已经变了时重新查询，不跳到错误的行。
    pub oid: BlobId,
    pub line: u32,
    pub column: u32,
    /// 定义节点的最后一行；引用为引用所在的行。
    pub end_line: u32,
}

/// 候选的六层，靠前的先列出（派生的顺序即声明顺序）。
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub enum CandidateTier {
    CurrentFile,
    Imported,
    SamePackage,
    SameSubproject,
    DependentSubproject,
    Elsewhere,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum Evidence {
    /// 有绑定依据：只有一个这样的候选时直接跳转。
    Bound(Binding),
    /// 只按名字找到。即使只有一个，也只预选、不跳转。
    NameOnly,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum Binding {
    /// locals 查询在当前文件的作用域里解析到。
    Local,
    /// 经 import 解析到的文件里的定义（解析扩展点给出）。
    Import,
    /// 按全限定名唯一确定的类型（Java，#18）。
    QualifiedName,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum CandidateSource {
    Index,
    Scanned,
    Resolver,
}

/// 定义的种类。`NameTable::kinds` 的位序号是这里的声明顺序（`kind as u16`）。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum DefKind {
    Function,
    Method,
    Class,
    Interface,
    Struct,
    Enum,
    Trait,
    Type,
    Module,
    Macro,
    Constant,
    Field,
    Variable,
    Other,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Reference {
    pub location: Location,
    pub syntax: RefSyntax,
    /// 引用所在的定义的名字（Callers 列表显示「谁在调用」）。
    pub enclosing: Option<Arc<str>>,
    pub in_review: bool,
}

/// 语言的稳定短名："ts"、"tsx"、"js"、"rust"、"java"、"python"、"go"；语言包用
/// 它在索引里登记的名字。
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct LangName(pub Arc<str>);
