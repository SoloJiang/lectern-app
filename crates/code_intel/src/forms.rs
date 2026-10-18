//! Every syntax form that introduces a name in a core language, and what
//! Lectern makes of it (docs/design/code_intel.md「定义形式」).
//!
//! The tags and locals queries and the walkers (`crate::walk`) implement the
//! rows; the syntax-kind masks (`site::accepted_kinds`) are derived from the
//! definition rows. The `forms_*` tests check that the table lists every
//! node kind of each grammar that has a naming field ([`NAMING_FIELDS`]),
//! that every listed kind exists in its grammar, that every definition and
//! binding row has a sample that extracts or binds, and that the design
//! document shows the same table.

use crate::locals::Namespace;
use crate::{DefKind, RefSyntax};

/// Fields whose node introduces or names something: a node kind with one of
/// them must have a row. Binding sites without such a field (parameter
/// lists, patterns) are listed explicitly.
#[cfg_attr(not(test), expect(dead_code, reason = "read by the forms_* tests"))]
pub(crate) const NAMING_FIELDS: &[&str] = &[
    "name",
    "declarator",
    "pattern",
    "alias",
    "label",
    "left",
    "property",
    "parameter",
    "key",
];

/// What a form is.
pub(crate) enum Role {
    /// A definition of one of `kinds` (by context, see the note), each with
    /// the reference syntaxes besides `Plain` that can name it; with
    /// `local`, also a local binding where the form is local.
    Def {
        kinds: &'static [(DefKind, &'static [RefSyntax])],
        #[cfg_attr(not(test), expect(dead_code, reason = "read by the forms_* tests"))]
        local: Option<Namespace>,
    },
    /// A local binding only.
    Local(
        #[cfg_attr(not(test), expect(dead_code, reason = "read by the forms_* tests"))] Namespace,
    ),
    /// Part of a form in another row (a declarator, a pattern).
    Part,
    /// A use of a name, not a definition.
    Use,
    /// Not modelled; the note says why.
    Ignored,
}

pub(crate) struct Form {
    pub langs: &'static [&'static str],
    #[cfg_attr(not(test), expect(dead_code, reason = "read by the forms_* tests"))]
    pub kind: &'static str,
    pub role: Role,
    #[cfg_attr(not(test), expect(dead_code, reason = "read by the forms_* tests"))]
    pub note: &'static str,
}

use Namespace::{Both, Type as TypeNs, Value};
use RefSyntax::{Call, Construct, Member, MemberCall, Type as TypeRef};

const ECMA: &[&str] = &["js", "ts", "tsx"];
const TS: &[&str] = &["ts", "tsx"];
const JS: &[&str] = &["js"];
const JSX: &[&str] = &["js", "tsx"];
const RUST: &[&str] = &["rust"];
const JAVA: &[&str] = &["java"];
const PYTHON: &[&str] = &["python"];
const GO: &[&str] = &["go"];

const NONE: &[RefSyntax] = &[];
const CALLS: &[RefSyntax] = &[Call, MemberCall];
const CALLS_NEW: &[RefSyntax] = &[Call, MemberCall, Construct];
const NEW_TYPE: &[RefSyntax] = &[Construct, TypeRef];
const TYPE: &[RefSyntax] = &[TypeRef];

const fn def(kinds: &'static [(DefKind, &'static [RefSyntax])]) -> Role {
    Role::Def { kinds, local: None }
}

const fn def_local(kinds: &'static [(DefKind, &'static [RefSyntax])], ns: Namespace) -> Role {
    Role::Def {
        kinds,
        local: Some(ns),
    }
}

const fn form(
    langs: &'static [&'static str],
    kind: &'static str,
    role: Role,
    note: &'static str,
) -> Form {
    Form {
        langs,
        kind,
        role,
        note,
    }
}

use DefKind::{
    Class, Constant, Enum, Field, Function, Interface, Macro, Method, Module, Struct, Trait,
    Type as TypeDef, Variable,
};

pub(crate) const FORMS: &[Form] = &[
    // ---- JavaScript, TypeScript, TSX ----
    form(
        ECMA,
        "array_pattern",
        Role::Part,
        "解构：顶层声明里两层以内的名字是定义，见 variable_declarator",
    ),
    form(
        ECMA,
        "arrow_function",
        Role::Local(Value),
        "`x => …` 的参数",
    ),
    form(
        ECMA,
        "assignment_expression",
        def(&[(Function, CALLS_NEW)]),
        "右边是函数时（`f = () => …`、`obj.f = function () {}`）；其余是赋值",
    ),
    form(
        ECMA,
        "assignment_pattern",
        Role::Part,
        "参数和解构的默认值：左边绑定，右边是用处",
    ),
    form(
        ECMA,
        "augmented_assignment_expression",
        Role::Ignored,
        "给已有的名字赋值",
    ),
    form(ECMA, "binary_expression", Role::Ignored, "表达式"),
    form(
        ECMA,
        "break_statement",
        Role::Ignored,
        "标签：单独的命名空间，不导航",
    ),
    form(ECMA, "catch_clause", Role::Local(Value), "`catch (e)`"),
    form(
        ECMA,
        "class",
        def(&[(Class, NEW_TYPE)]),
        "类表达式：名字只在类里可见，经索引找到",
    ),
    form(
        ECMA,
        "class_declaration",
        def_local(&[(Class, NEW_TYPE)], Both),
        "",
    ),
    form(ECMA, "continue_statement", Role::Ignored, "标签"),
    form(
        ECMA,
        "export_specifier",
        Role::Use,
        "`export { a as b }`：`a` 是用处，`b` 是导出名",
    ),
    form(
        JS,
        "field_definition",
        def(&[(Field, CALLS)]),
        "类字段，值可以是函数",
    ),
    form(
        ECMA,
        "for_in_statement",
        Role::Local(Value),
        "带 `const` / `let` / `var` 时；否则是赋值",
    ),
    form(
        ECMA,
        "formal_parameters",
        Role::Local(Value),
        "每个参数，含解构和默认值",
    ),
    form(
        ECMA,
        "function_declaration",
        def_local(&[(Function, CALLS_NEW)], Value),
        "",
    ),
    form(
        ECMA,
        "function_expression",
        def(&[(Function, CALLS_NEW)]),
        "名字只在函数里可见，经索引找到",
    ),
    form(
        ECMA,
        "generator_function",
        def(&[(Function, CALLS)]),
        "同函数表达式",
    ),
    form(
        ECMA,
        "generator_function_declaration",
        def_local(&[(Function, CALLS)], Value),
        "",
    ),
    form(
        ECMA,
        "import_clause",
        Role::Ignored,
        "import：经索引和解析扩展点解析，不是局部定义",
    ),
    form(ECMA, "import_specifier", Role::Ignored, "import，同上"),
    form(
        ECMA,
        "namespace_import",
        Role::Ignored,
        "`import * as ns`，同上",
    ),
    form(JSX, "jsx_closing_element", Role::Use, "组件名"),
    form(JSX, "jsx_opening_element", Role::Use, "组件名"),
    form(JSX, "jsx_self_closing_element", Role::Use, "组件名"),
    form(ECMA, "labeled_statement", Role::Ignored, "标签"),
    form(ECMA, "member_expression", Role::Use, "`a.b`"),
    form(
        ECMA,
        "method_definition",
        def(&[(Method, CALLS)]),
        "`constructor` 不是定义（`new C` 指向类）；计算出的名字不记",
    ),
    form(
        ECMA,
        "object_assignment_pattern",
        Role::Part,
        "解构的默认值",
    ),
    form(
        ECMA,
        "object_pattern",
        Role::Part,
        "解构：顶层声明里两层以内的名字是定义，见 variable_declarator",
    ),
    form(
        ECMA,
        "pair",
        def(&[(Function, CALLS_NEW)]),
        "值是函数时；其余是对象的键",
    ),
    form(
        ECMA,
        "pair_pattern",
        Role::Part,
        "解构：键是属性名，值是模式",
    ),
    form(ECMA, "regex", Role::Ignored, "正则的内容"),
    form(ECMA, "rest_pattern", Role::Part, "解构的 `...rest`"),
    form(
        ECMA,
        "shorthand_property_identifier_pattern",
        Role::Part,
        "解构的简写",
    ),
    form(
        ECMA,
        "variable_declarator",
        def_local(
            &[
                (Function, CALLS_NEW),
                (Class, NEW_TYPE),
                (Constant, CALLS_NEW),
                (Variable, CALLS_NEW),
            ],
            Value,
        ),
        "值是函数或类时是函数或类；顶层（含 `export`、`declare`）的 `const` 是常量，`let` / `var` 是变量，解构时模式两层以内绑定的每个名字都是；TS 的 namespace / module 体里 `export` 的也是（只认单个名字）；都是局部绑定",
    ),
    form(
        TS,
        "abstract_class_declaration",
        def_local(&[(Class, TYPE)], Both),
        "",
    ),
    form(TS, "abstract_method_signature", def(&[(Method, CALLS)]), ""),
    form(
        TS,
        "ambient_declaration",
        Role::Part,
        "`declare const` / `let` / `var` 见 variable_declarator，`declare function` 见 function_signature",
    ),
    form(TS, "conditional_type", Role::Use, "类型"),
    form(
        TS,
        "enum_assignment",
        def(&[(Constant, NONE)]),
        "有初始值的枚举成员",
    ),
    form(
        TS,
        "enum_body",
        def(&[(Constant, NONE)]),
        "没有初始值的枚举成员",
    ),
    form(TS, "enum_declaration", def_local(&[(Enum, TYPE)], Both), ""),
    form(
        TS,
        "function_signature",
        def(&[(Function, CALLS)]),
        "重载签名和 `declare function`",
    ),
    form(TS, "generic_type", Role::Use, "`Foo<T>`"),
    form(
        TS,
        "index_signature",
        Role::Ignored,
        "`[key: string]: T` 的键名只是说明",
    ),
    form(
        TS,
        "infer_type",
        Role::Ignored,
        "`infer U`：只在条件类型的一个分支里可见，没有建模，只给索引候选（不直接跳转）",
    ),
    form(
        TS,
        "interface_declaration",
        def_local(&[(Interface, TYPE)], TypeNs),
        "",
    ),
    form(
        TS,
        "internal_module",
        def(&[(Module, TYPE)]),
        "`namespace A.B` 记最后一段",
    ),
    form(
        TS,
        "mapped_type_clause",
        Role::Local(TypeNs),
        "`[K in keyof T]`，在映射类型里可见",
    ),
    form(TS, "method_signature", def(&[(Method, CALLS)]), ""),
    form(
        TS,
        "module",
        def(&[(Module, TYPE)]),
        "`module A`；`declare module 'x'` 的名字是字符串，不记",
    ),
    form(
        TS,
        "nested_identifier",
        Role::Part,
        "模块名 `A.B`，见 internal_module",
    ),
    form(TS, "nested_type_identifier", Role::Use, "`ns.T`"),
    form(
        TS,
        "optional_parameter",
        def_local(&[(Field, CALLS)], Value),
        "参数；带 `public` / `private` / `protected` / `readonly` / `override` 时也是字段（参数属性）",
    ),
    form(TS, "property_signature", def(&[(Field, CALLS)]), ""),
    form(
        TS,
        "public_field_definition",
        def(&[(Field, CALLS)]),
        "类字段",
    ),
    form(
        TS,
        "required_parameter",
        def_local(&[(Field, CALLS)], Value),
        "同 optional_parameter",
    ),
    form(
        TS,
        "type_alias_declaration",
        def_local(&[(TypeDef, TYPE)], TypeNs),
        "",
    ),
    form(
        TS,
        "type_parameter",
        Role::Local(TypeNs),
        "在声明它的类、接口、类型别名或函数里可见",
    ),
    form(TS, "type_predicate", Role::Use, "`x is T`"),
    // ---- Rust ----
    form(
        RUST,
        "assignment_expression",
        Role::Ignored,
        "给已有的绑定赋值",
    ),
    form(
        RUST,
        "associated_type",
        def(&[(TypeDef, TYPE)]),
        "trait 里的 `type Item;`",
    ),
    form(RUST, "binary_expression", Role::Ignored, "表达式"),
    form(RUST, "closure_parameters", Role::Local(Value), "闭包的参数"),
    form(
        RUST,
        "compound_assignment_expr",
        Role::Ignored,
        "给已有的绑定赋值",
    ),
    form(
        RUST,
        "const_item",
        def_local(&[(Constant, &[Call])], Value),
        "块里的是局部",
    ),
    form(
        RUST,
        "const_parameter",
        Role::Local(Value),
        "`const N: usize`，在声明它的项里可见",
    ),
    form(
        RUST,
        "enum_item",
        def_local(&[(Enum, TYPE)], TypeNs),
        "块里的是局部",
    ),
    form(
        RUST,
        "enum_variant",
        def(&[(Constant, &[Call, Construct, TypeRef])]),
        "元组变体被调用（`Io(e)`），记录变体被构造和匹配（`Move { x, y }`）",
    ),
    form(
        RUST,
        "extern_crate_declaration",
        Role::Ignored,
        "crate 的别名：经路径解析",
    ),
    form(RUST, "field_declaration", def(&[(Field, &[Member])]), ""),
    form(
        RUST,
        "field_pattern",
        Role::Part,
        "结构体模式里的字段：简写和改名绑定",
    ),
    form(RUST, "for_expression", Role::Local(Value), ""),
    form(
        RUST,
        "function_item",
        def_local(
            &[(Function, &[Call]), (Method, &[Call, MemberCall, Member])],
            Value,
        ),
        "`impl` 和 trait 里的是方法；块里的是局部",
    ),
    form(
        RUST,
        "function_signature_item",
        def(&[(Function, &[Call]), (Method, &[Call, MemberCall, Member])]),
        "trait 和 `extern` 块里的签名",
    ),
    form(
        RUST,
        "let_condition",
        Role::Local(Value),
        "`if let` / `while let`",
    ),
    form(RUST, "let_declaration", Role::Local(Value), ""),
    form(
        RUST,
        "lifetime_parameter",
        Role::Ignored,
        "生命周期不进入索引，不导航",
    ),
    form(RUST, "macro_definition", def(&[(Macro, &[Call])]), ""),
    form(RUST, "macro_rule", Role::Ignored, "宏的匹配模式"),
    form(RUST, "match_arm", Role::Local(Value), ""),
    form(RUST, "mod_item", def(&[(Module, NONE)]), "路径段是 `Plain`"),
    form(RUST, "parameter", Role::Local(Value), ""),
    form(RUST, "qualified_type", Role::Use, "`<T as Trait>`"),
    form(RUST, "range_pattern", Role::Use, "范围模式的端点"),
    form(RUST, "scoped_identifier", Role::Use, "路径"),
    form(RUST, "scoped_type_identifier", Role::Use, "路径"),
    form(
        RUST,
        "static_item",
        def_local(&[(Constant, &[Call])], Value),
        "块里的是局部",
    ),
    form(RUST, "struct_expression", Role::Use, "构造"),
    form(
        RUST,
        "struct_item",
        def_local(&[(Struct, &[Call, Construct, TypeRef])], Both),
        "元组结构体被调用；块里的是局部",
    ),
    form(RUST, "token_binding_pattern", Role::Ignored, "宏的元变量"),
    form(
        RUST,
        "trait_item",
        def_local(&[(Trait, TYPE)], TypeNs),
        "块里的是局部",
    ),
    form(RUST, "type_binding", Role::Use, "`Iterator<Item = T>`"),
    form(
        RUST,
        "type_item",
        def_local(&[(TypeDef, NEW_TYPE)], TypeNs),
        "块里的是局部",
    ),
    form(
        RUST,
        "type_parameter",
        Role::Local(TypeNs),
        "在声明它的项里可见",
    ),
    form(
        RUST,
        "union_item",
        def_local(&[(Struct, NEW_TYPE)], TypeNs),
        "块里的是局部",
    ),
    form(
        RUST,
        "use_as_clause",
        Role::Ignored,
        "`use` 的别名：经路径解析",
    ),
    form(
        RUST,
        "variadic_parameter",
        Role::Ignored,
        "C 的变长参数（`args: ...`，只在 nightly 上可用）：没有建模，只给索引候选",
    ),
    form(RUST, "where_predicate", Role::Use, "类型"),
    // ---- Java ----
    form(JAVA, "annotation", Role::Use, "注解"),
    form(
        JAVA,
        "annotation_type_declaration",
        def(&[(Interface, TYPE)]),
        "",
    ),
    form(
        JAVA,
        "annotation_type_element_declaration",
        def(&[(Method, CALLS)]),
        "`@interface` 的元素",
    ),
    form(
        JAVA,
        "assignment_expression",
        Role::Ignored,
        "给已有的变量赋值",
    ),
    form(JAVA, "binary_expression", Role::Ignored, "表达式"),
    form(JAVA, "catch_formal_parameter", Role::Local(Value), ""),
    form(
        JAVA,
        "class_declaration",
        def_local(&[(Class, NEW_TYPE)], Both),
        "块里的局部类是局部",
    ),
    form(
        JAVA,
        "compact_constructor_declaration",
        Role::Ignored,
        "紧凑构造器：`new R(…)` 指向记录",
    ),
    form(
        JAVA,
        "constant_declaration",
        def(&[(Constant, NONE)]),
        "接口里的常量",
    ),
    form(
        JAVA,
        "constructor_declaration",
        Role::Ignored,
        "构造器：`new C(…)` 指向类",
    ),
    form(JAVA, "element_value_pair", Role::Use, "注解里的元素名"),
    form(JAVA, "enhanced_for_statement", Role::Local(Value), ""),
    form(JAVA, "enum_constant", def(&[(Constant, NONE)]), ""),
    form(
        JAVA,
        "enum_declaration",
        def_local(&[(Enum, TYPE)], Both),
        "块里的是局部",
    ),
    form(JAVA, "field_declaration", def(&[(Field, NONE)]), ""),
    form(
        JAVA,
        "formal_parameter",
        def_local(&[(Field, CALLS)], Value),
        "方法、构造器和 lambda 的参数是局部；记录头里的组件是字段，调用语法指向它的访问器",
    ),
    form(
        JAVA,
        "inferred_parameters",
        Role::Local(Value),
        "`(a, b) -> …`",
    ),
    form(
        JAVA,
        "instanceof_expression",
        Role::Local(Value),
        "模式变量：可见范围取决于控制流，只给候选",
    ),
    form(
        JAVA,
        "interface_declaration",
        def_local(&[(Interface, TYPE)], Both),
        "块里的是局部",
    ),
    form(JAVA, "lambda_expression", Role::Local(Value), "`x -> …`"),
    form(JAVA, "local_variable_declaration", Role::Local(Value), ""),
    form(JAVA, "marker_annotation", Role::Use, "注解"),
    form(JAVA, "method_declaration", def(&[(Method, CALLS)]), ""),
    form(JAVA, "method_invocation", Role::Use, "调用"),
    form(
        JAVA,
        "module_declaration",
        Role::Ignored,
        "`module-info.java`",
    ),
    form(
        JAVA,
        "record_declaration",
        def_local(&[(Class, NEW_TYPE)], Both),
        "块里的是局部",
    ),
    form(
        JAVA,
        "record_pattern_component",
        Role::Local(Value),
        "模式变量：只给候选",
    ),
    form(JAVA, "resource", Role::Local(Value), "`try (var r = …)`"),
    form(JAVA, "scoped_identifier", Role::Use, "限定名"),
    form(
        JAVA,
        "spread_parameter",
        def_local(&[(Field, CALLS)], Value),
        "同 formal_parameter",
    ),
    form(
        JAVA,
        "type_parameter",
        Role::Local(TypeNs),
        "在声明它的类或方法里可见",
    ),
    form(
        JAVA,
        "type_pattern",
        Role::Local(Value),
        "模式变量：只给候选",
    ),
    form(JAVA, "variable_declarator", Role::Part, "见所在的声明"),
    // ---- Python ----
    form(
        PYTHON,
        "aliased_import",
        Role::Ignored,
        "import：经索引解析",
    ),
    form(
        PYTHON,
        "as_pattern",
        Role::Local(Value),
        "`with … as x`、`except … as x`",
    ),
    form(
        PYTHON,
        "assignment",
        def_local(&[(Constant, CALLS), (Field, CALLS)], Value),
        "模块顶层是常量，类体里是字段，解包（`A, B = …`、`first, *rest = …`）时两层以内的每个名字都是；都是局部绑定",
    ),
    form(PYTHON, "augmented_assignment", Role::Local(Value), ""),
    form(PYTHON, "binary_operator", Role::Ignored, "表达式"),
    form(PYTHON, "boolean_operator", Role::Ignored, "表达式"),
    form(
        PYTHON,
        "case_pattern",
        Role::Local(Value),
        "`match` 的捕获；类名、关键字、带点的值和键不绑定",
    ),
    form(
        PYTHON,
        "class_definition",
        def_local(&[(Class, CALLS)], Value),
        "",
    ),
    form(
        PYTHON,
        "default_parameter",
        Role::Local(Value),
        "默认值在外层作用域求值",
    ),
    form(
        PYTHON,
        "dict_pattern",
        Role::Part,
        "`match` 的映射模式，见 case_pattern",
    ),
    form(
        PYTHON,
        "except_clause",
        Role::Part,
        "`as` 由 as_pattern 绑定",
    ),
    form(PYTHON, "for_in_clause", Role::Local(Value), "推导式的变量"),
    form(PYTHON, "for_statement", Role::Local(Value), ""),
    form(
        PYTHON,
        "function_definition",
        def_local(&[(Function, CALLS), (Method, CALLS)], Value),
        "类体里的是方法",
    ),
    form(PYTHON, "future_import_statement", Role::Ignored, "import"),
    form(
        PYTHON,
        "global_statement",
        Role::Use,
        "作用域指令：名字是模块的，越过所有外层函数（「绑定规则」）",
    ),
    form(
        PYTHON,
        "import_from_statement",
        Role::Ignored,
        "import：经索引解析",
    ),
    form(
        PYTHON,
        "import_statement",
        Role::Ignored,
        "import：经索引解析",
    ),
    form(PYTHON, "keyword_argument", Role::Use, "调用的关键字参数名"),
    form(PYTHON, "lambda_parameters", Role::Local(Value), ""),
    form(PYTHON, "list_pattern", Role::Part, "解包，见 assignment"),
    form(
        PYTHON,
        "list_splat_pattern",
        Role::Part,
        "解包的 `*rest`，见 assignment",
    ),
    form(PYTHON, "named_expression", Role::Local(Value), "`:=`"),
    form(
        PYTHON,
        "nonlocal_statement",
        Role::Use,
        "作用域指令：名字是最近的有它的外层函数的，不找到模块",
    ),
    form(PYTHON, "pair", Role::Ignored, "字典的键"),
    form(PYTHON, "parameters", Role::Local(Value), "每个参数"),
    form(PYTHON, "pattern_list", Role::Part, "解包，见 assignment"),
    form(PYTHON, "tuple_pattern", Role::Part, "解包，见 assignment"),
    form(
        PYTHON,
        "type_alias_statement",
        def(&[(TypeDef, NONE)]),
        "`type X = …`",
    ),
    form(
        PYTHON,
        "type_parameter",
        Role::Local(Value),
        "`def f[T]()`，在声明它的函数或类里可见",
    ),
    form(
        PYTHON,
        "typed_default_parameter",
        Role::Local(Value),
        "类型和默认值在外层作用域求值",
    ),
    form(
        PYTHON,
        "typed_parameter",
        Role::Local(Value),
        "类型在外层作用域求值",
    ),
    // ---- Go ----
    form(
        GO,
        "assignment_statement",
        Role::Ignored,
        "给已有的变量赋值",
    ),
    form(GO, "binary_expression", Role::Ignored, "表达式"),
    form(
        GO,
        "const_spec",
        def_local(&[(Constant, NONE)], Value),
        "包级的是常量（`const A, B` 的每个名字）；函数里的是局部",
    ),
    form(GO, "expression_list", Role::Part, "`:=`、`range` 等的左边"),
    form(
        GO,
        "field_declaration",
        def(&[(Field, &[MemberCall])]),
        "函数类型的字段可以被调用",
    ),
    form(GO, "function_declaration", def(&[(Function, CALLS)]), ""),
    form(GO, "import_spec", Role::Ignored, "import：经路径解析"),
    form(GO, "keyed_element", Role::Use, "复合字面量的键"),
    form(GO, "labeled_statement", Role::Ignored, "标签"),
    form(GO, "map_type", Role::Use, "类型"),
    form(
        GO,
        "method_declaration",
        def(&[(Method, &[MemberCall])]),
        "",
    ),
    form(
        GO,
        "method_elem",
        def(&[(Method, &[MemberCall])]),
        "接口的方法",
    ),
    form(GO, "parameter_declaration", Role::Local(Value), ""),
    form(GO, "qualified_type", Role::Use, "`pkg.T`"),
    form(
        GO,
        "range_clause",
        Role::Local(Value),
        "只有 `:=`；`for i = range xs` 是赋值",
    ),
    form(
        GO,
        "receive_statement",
        Role::Local(Value),
        "只有 `:=`；`case v = <-ch` 是赋值",
    ),
    form(
        GO,
        "short_var_declaration",
        Role::Local(Value),
        "只有同一作用域里新的名字（函数体还要除去参数和结果）；已声明的名字是对它的使用",
    ),
    form(
        GO,
        "type_alias",
        def(&[(TypeDef, &[TypeRef, Call, MemberCall])]),
        "类型转换是调用语法",
    ),
    form(
        GO,
        "type_parameter_declaration",
        Role::Local(Both),
        "在声明它的函数或类型里可见；方法接收者里的类型参数（`(l *List[E])`）没有建模",
    ),
    form(
        GO,
        "type_spec",
        def(&[(TypeDef, &[TypeRef, Call, MemberCall])]),
        "函数里的类型也进入索引，没有局部绑定",
    ),
    form(
        GO,
        "type_switch_statement",
        Role::Local(Value),
        "`switch v := x.(type)`",
    ),
    form(
        GO,
        "var_spec",
        def_local(&[(Variable, CALLS)], Value),
        "包级的是变量；函数里的是局部",
    ),
    form(GO, "var_spec_list", Role::Part, "`var ( … )`"),
    form(GO, "variadic_parameter_declaration", Role::Local(Value), ""),
];

/// The forms of one language.
pub(crate) fn forms(lang: &str) -> impl Iterator<Item = &'static Form> + '_ {
    FORMS.iter().filter(move |f| f.langs.contains(&lang))
}

/// The definition kinds that a reference of `syntax` can name in `lang`,
/// from the definition rows; `Plain` is never narrowed.
pub(crate) fn kinds_for(lang: &str, syntax: RefSyntax) -> u16 {
    let mut bits = 0;
    for form in forms(lang) {
        if let Role::Def { kinds, .. } = &form.role {
            for (kind, by) in *kinds {
                if by.contains(&syntax) {
                    bits |= crate::def_kind_bit(*kind);
                }
            }
        }
    }
    bits
}
