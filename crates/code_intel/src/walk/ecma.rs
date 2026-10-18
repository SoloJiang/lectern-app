//! JavaScript, TypeScript and TSX: the queries of `lang.rs` for these
//! languages (ecmascript, then ts or js, then jsx, then
//! ecmascript/references.scm; ecmascript/locals.scm, plus ts/locals.scm) as
//! one walk.

use tree_sitter::{Language, Node};

use super::{Ctx, Frame, Walker, field_of, ids, up};
use crate::DefKind;
use crate::lang::{Dialect, Marker};

ids! {
    Ids {
        kinds {
            program = "program",
            method_definition = "method_definition",
            class = "class",
            class_declaration = "class_declaration",
            function_expression = "function_expression",
            function_declaration = "function_declaration",
            generator_function = "generator_function",
            generator_function_declaration = "generator_function_declaration",
            arrow_function = "arrow_function",
            formal_parameters = "formal_parameters",
            variable_declarator = "variable_declarator",
            lexical_declaration = "lexical_declaration",
            variable_declaration = "variable_declaration",
            assignment_expression = "assignment_expression",
            member_expression = "member_expression",
            pair = "pair",
            export_statement = "export_statement",
            export_specifier = "export_specifier",
            import_statement = "import_statement",
            number = "number",
            string = "string",
            undefined = "undefined",
            null = "null",
            new_expression = "new_expression",
            binary_expression = "binary_expression",
            call_expression = "call_expression",
            statement_block = "statement_block",
            for_statement = "for_statement",
            for_in_statement = "for_in_statement",
            catch_clause = "catch_clause",
            identifier = "identifier",
            property_identifier = "property_identifier",
            private_property_identifier = "private_property_identifier",
            shorthand_property_identifier = "shorthand_property_identifier",
        }
        tokens {
            t_export = "export",
            t_import = "import",
            t_const = "const",
            t_let = "let",
            t_var = "var",
        }
        fields {
            f_name = "name",
            f_parameters = "parameters",
            f_parameter = "parameter",
            f_value = "value",
            f_left = "left",
            f_right = "right",
            f_key = "key",
            f_kind = "kind",
            f_declaration = "declaration",
            f_property = "property",
        }
    }
}

ids! {
    TsIds {
        kinds {
            function_signature = "function_signature",
            method_signature = "method_signature",
            abstract_method_signature = "abstract_method_signature",
            abstract_class_declaration = "abstract_class_declaration",
            module = "module",
            internal_module = "internal_module",
            interface_declaration = "interface_declaration",
            type_alias_declaration = "type_alias_declaration",
            enum_declaration = "enum_declaration",
            enum_body = "enum_body",
            enum_assignment = "enum_assignment",
            public_field_definition = "public_field_definition",
            property_signature = "property_signature",
            type_identifier = "type_identifier",
            required_parameter = "required_parameter",
            optional_parameter = "optional_parameter",
            accessibility_modifier = "accessibility_modifier",
            override_modifier = "override_modifier",
            ambient_declaration = "ambient_declaration",
            nested_identifier = "nested_identifier",
        }
        tokens {
            t_readonly = "readonly",
        }
        fields {
            f_pattern = "pattern",
        }
    }
}

ids! {
    JsIds {
        kinds {
            field_definition = "field_definition",
        }
        tokens {}
        fields {}
    }
}

pub(super) struct Ecma {
    k: Ids,
    ts: Option<TsIds>,
    js: Option<JsIds>,
}

impl Ecma {
    pub(super) fn new(language: &Language, dialect: Dialect) -> Result<Ecma, String> {
        let ts = matches!(dialect, Dialect::TypeScript | Dialect::Tsx);
        Ok(Ecma {
            k: Ids::new(language)?,
            ts: ts.then(|| TsIds::new(language)).transpose()?,
            js: (dialect == Dialect::JavaScript)
                .then(|| JsIds::new(language))
                .transpose()?,
        })
    }
}

impl Walker for Ecma {
    fn visit(&self, cx: &mut Ctx<'_>, stack: &[Frame<'_>]) {
        let k = &self.k;
        let me = stack[stack.len() - 1];
        let kind = me.kind;
        if kind == k.statement_block || kind == k.for_statement || kind == k.for_in_statement {
            cx.scope(stack.len() - 1, true);
        } else if kind == k.function_expression
            || kind == k.arrow_function
            || kind == k.function_declaration
            || kind == k.generator_function
            || kind == k.generator_function_declaration
            || kind == k.method_definition
        {
            cx.scope(stack.len() - 1, false);
        } else if kind == k.export_statement {
            // (export_statement "export" @name) @reference.lectern.export
            self.tokens(cx, me.node, k.t_export, Marker::Export);
        } else if kind == k.import_statement {
            // (import_statement "import" @name) @reference.lectern.module
            self.tokens(cx, me.node, k.t_import, Marker::Module);
        }
        if self.local_definition(stack) {
            cx.local(me.node);
        }
        if kind == k.identifier {
            self.identifier(cx, stack);
        } else if kind == k.property_identifier {
            self.property(cx, stack, false);
        } else if kind == k.private_property_identifier {
            self.property(cx, stack, true);
        } else if kind == k.shorthand_property_identifier {
            cx.nonlocal(me.node);
        } else if let Some(ts) = &self.ts
            && kind == ts.type_identifier
        {
            self.type_identifier(cx, stack, ts);
        }
    }
}

impl Ecma {
    fn tokens(&self, cx: &mut Ctx<'_>, node: Node<'_>, token: u16, marker: Marker) {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind_id() == token {
                cx.marker(child, node, marker);
            }
        }
    }

    /// ecmascript/locals.scm and ts/locals.scm's `@local.definition`
    /// captures, on any node.
    fn local_definition(&self, stack: &[Frame<'_>]) -> bool {
        let k = &self.k;
        let me = stack[stack.len() - 1];
        let Some(parent) = up(stack, 1) else {
            return false;
        };
        let p = parent.kind;
        // (formal_parameters (_)): any named child.
        if p == k.formal_parameters {
            return true;
        }
        if me.field == k.f_name {
            if p == k.variable_declarator {
                return up(stack, 2).is_some_and(|d| {
                    d.kind == k.lexical_declaration || d.kind == k.variable_declaration
                });
            }
            if p == k.class_declaration {
                return true;
            }
            if (p == k.function_declaration || p == k.generator_function_declaration)
                && me.kind == k.identifier
            {
                return true;
            }
        }
        if me.field == k.f_left && p == k.for_in_statement {
            return parent.node.child_by_field_id(k.f_kind).is_some_and(|t| {
                let t = t.kind_id();
                t == k.t_var || t == k.t_let || t == k.t_const
            });
        }
        if me.field == k.f_parameter {
            if p == k.catch_clause {
                return true;
            }
            if p == k.arrow_function && me.kind == k.identifier {
                return true;
            }
        }
        if let Some(ts) = &self.ts
            && me.field == ts.f_pattern
            && (p == ts.required_parameter || p == ts.optional_parameter)
        {
            return true;
        }
        false
    }

    /// The `@doc` capture of a function value in E3, E5 and E6:
    /// `(arrow_function parameters: (formal_parameters) @doc)`,
    /// `(arrow_function parameter: (identifier) @doc)`,
    /// `(function_expression parameters: (formal_parameters) @doc)` and, for
    /// a declarator only, `(generator_function …)`.
    fn function_params<'t>(&self, value: Node<'t>, generator: bool) -> Option<Node<'t>> {
        let k = &self.k;
        let kind = value.kind_id();
        if kind == k.arrow_function {
            field_of(value, k.f_parameters, k.formal_parameters)
                .or_else(|| field_of(value, k.f_parameter, k.identifier))
        } else if kind == k.function_expression || generator && kind == k.generator_function {
            field_of(value, k.f_parameters, k.formal_parameters)
        } else {
            None
        }
    }

    /// E8 and E9: a declarator at the top level of the program, directly or
    /// in an export statement's `declaration`; Some(true) for `const`.
    fn top_level_declaration(&self, stack: &[Frame<'_>]) -> Option<bool> {
        let k = &self.k;
        let declaration = up(stack, 2)?;
        let constant = if declaration.kind == k.lexical_declaration {
            let token = declaration.node.child_by_field_id(k.f_kind)?.kind_id();
            if token == k.t_const {
                true
            } else if token == k.t_let {
                false
            } else {
                return None;
            }
        } else if declaration.kind == k.variable_declaration {
            false
        } else {
            return None;
        };
        // TS: `declare const` sits in an ambient declaration
        // (ts/tags.scm).
        let mut level = 3;
        let mut placed = declaration;
        if let Some(ts) = &self.ts
            && let Some(ambient) = up(stack, 3)
            && ambient.kind == ts.ambient_declaration
        {
            placed = ambient;
            level = 4;
        }
        let container = up(stack, level)?;
        let at_top = container.kind == k.program
            || (container.kind == k.export_statement
                && placed.field == k.f_declaration
                && up(stack, level + 1).is_some_and(|p| p.kind == k.program));
        at_top.then_some(constant)
    }

    /// TS: a parameter property, a constructor parameter with an
    /// accessibility modifier, `override` or `readonly` (ts/tags.scm).
    fn parameter_property(&self, parameter: tree_sitter::Node<'_>, ts: &TsIds) -> bool {
        let mut cursor = parameter.walk();
        parameter.children(&mut cursor).any(|c| {
            let c = c.kind_id();
            c == ts.accessibility_modifier || c == ts.override_modifier || c == ts.t_readonly
        })
    }

    fn identifier(&self, cx: &mut Ctx<'_>, stack: &[Frame<'_>]) {
        let k = &self.k;
        let me = stack[stack.len() - 1];
        if let Some(parent) = up(stack, 1) {
            let p = parent.kind;
            let name = me.field == k.f_name;
            // E1: (class name: (_)), (class_declaration name: (_)).
            if name && (p == k.class || p == k.class_declaration) {
                cx.def(me.node, parent.node, DefKind::Class, None);
                return;
            }
            // E2: named function expressions and declarations.
            if name
                && (p == k.function_expression
                    || p == k.function_declaration
                    || p == k.generator_function
                    || p == k.generator_function_declaration)
                && let Some(params) = field_of(parent.node, k.f_parameters, k.formal_parameters)
            {
                cx.def(me.node, parent.node, DefKind::Function, Some(params));
                return;
            }
            if name
                && p == k.variable_declarator
                && let Some(value) = parent.node.child_by_field_id(k.f_value)
            {
                // E3: a declarator whose value is a function.
                if let Some(params) = self.function_params(value, true) {
                    cx.def(me.node, parent.node, DefKind::Function, Some(params));
                    return;
                }
                // E4: a declarator whose value is a class.
                if value.kind_id() == k.class {
                    cx.def(me.node, parent.node, DefKind::Class, None);
                    return;
                }
            }
            // E5: (assignment_expression left: (identifier) right: function).
            if p == k.assignment_expression
                && me.field == k.f_left
                && let Some(params) = parent
                    .node
                    .child_by_field_id(k.f_right)
                    .and_then(|r| self.function_params(r, false))
            {
                cx.def(me.node, parent.node, DefKind::Function, Some(params));
                return;
            }
            // E7: (export_statement value: (assignment_expression
            //   left: (identifier) right: [literal …])) @definition.constant
            if p == k.assignment_expression
                && me.field == k.f_left
                && parent.field == k.f_value
                && let Some(statement) = up(stack, 2)
                && statement.kind == k.export_statement
                && parent.node.child_by_field_id(k.f_right).is_some_and(|r| {
                    let r = r.kind_id();
                    r == k.number
                        || r == k.string
                        || r == k.identifier
                        || r == k.undefined
                        || r == k.null
                        || r == k.new_expression
                        || r == k.binary_expression
                        || r == k.call_expression
                })
            {
                cx.def(me.node, statement.node, DefKind::Constant, None);
                return;
            }
            // E8, E9: top-level const, let and var declarators.
            if name
                && p == k.variable_declarator
                && let Some(constant) = self.top_level_declaration(stack)
            {
                let kind = if constant {
                    DefKind::Constant
                } else {
                    DefKind::Variable
                };
                cx.def(me.node, parent.node, kind, None);
                return;
            }
            // E10 (call, not `require`) and E12 (new) are `#is-not? local`
            // like the catch-all; a local name falls through to them.
            // E15: (export_specifier name: (identifier)) and E16:
            // (export_statement value: (identifier)) are markers.
            if (name && p == k.export_specifier)
                || (p == k.export_statement && me.field == k.f_value)
            {
                cx.marker(me.node, parent.node, Marker::ExportName);
                return;
            }
            if let Some(ts) = &self.ts {
                // T0: (function_signature name: params) @definition.function
                if name
                    && p == ts.function_signature
                    && let Some(params) = field_of(parent.node, k.f_parameters, k.formal_parameters)
                {
                    cx.def(me.node, parent.node, DefKind::Function, Some(params));
                    return;
                }
                // T4, T5: modules and namespaces; T8: enums.
                if name && (p == ts.module || p == ts.internal_module) {
                    cx.def(me.node, parent.node, DefKind::Module, None);
                    return;
                }
                if name && p == ts.enum_declaration {
                    cx.def(me.node, parent.node, DefKind::Enum, None);
                    return;
                }
                // Parameter properties: fields.
                if (p == ts.required_parameter || p == ts.optional_parameter)
                    && me.field == ts.f_pattern
                    && self.parameter_property(parent.node, ts)
                {
                    cx.def(me.node, parent.node, DefKind::Field, None);
                    return;
                }
            }
        }
        // E10, E12, T13, J1, X0, X1 and the catch-all: all `#is-not? local`.
        cx.nonlocal(me.node);
    }

    fn property(&self, cx: &mut Ctx<'_>, stack: &[Frame<'_>], private: bool) {
        let k = &self.k;
        let me = stack[stack.len() - 1];
        if let Some(parent) = up(stack, 1) {
            let p = parent.kind;
            let name = me.field == k.f_name;
            // E0: (method_definition name: params (#not-eq? @name "constructor"))
            if name
                && p == k.method_definition
                && cx.text(me.node) != b"constructor"
                && let Some(params) = field_of(parent.node, k.f_parameters, k.formal_parameters)
            {
                cx.def(me.node, parent.node, DefKind::Method, Some(params));
                return;
            }
            if !private {
                // E5: (assignment_expression left: (member_expression
                //   property: (property_identifier)) right: function)
                if p == k.member_expression
                    && me.field == k.f_property
                    && parent.field == k.f_left
                    && let Some(assignment) = up(stack, 2)
                    && assignment.kind == k.assignment_expression
                    && let Some(params) = assignment
                        .node
                        .child_by_field_id(k.f_right)
                        .and_then(|r| self.function_params(r, false))
                {
                    cx.def(me.node, assignment.node, DefKind::Function, Some(params));
                    return;
                }
                // E6: (pair key: (property_identifier) value: function)
                if p == k.pair
                    && me.field == k.f_key
                    && let Some(params) = parent
                        .node
                        .child_by_field_id(k.f_value)
                        .and_then(|v| self.function_params(v, false))
                {
                    cx.def(me.node, parent.node, DefKind::Function, Some(params));
                    return;
                }
            }
            if let Some(ts) = &self.ts {
                // T1, T2: method signatures.
                if name
                    && (p == ts.method_signature || p == ts.abstract_method_signature)
                    && let Some(params) = field_of(parent.node, k.f_parameters, k.formal_parameters)
                {
                    cx.def(me.node, parent.node, DefKind::Method, Some(params));
                    return;
                }
                if !private && name && p == ts.enum_body {
                    // T9: (enum_body name: (property_identifier) @name
                    //   @definition.constant)
                    cx.def(me.node, me.node, DefKind::Constant, None);
                    return;
                }
                if !private && name && p == ts.enum_assignment {
                    cx.def(me.node, parent.node, DefKind::Constant, None);
                    return;
                }
                // T11: public fields; T12: property signatures.
                if name
                    && (p == ts.public_field_definition || !private && p == ts.property_signature)
                {
                    cx.def(me.node, parent.node, DefKind::Field, None);
                    return;
                }
                // `namespace A.B`: the last segment.
                if !private
                    && p == ts.nested_identifier
                    && me.field == k.f_property
                    && let Some(module) = up(stack, 2)
                    && (module.kind == ts.module || module.kind == ts.internal_module)
                    && parent.field == k.f_name
                {
                    cx.def(me.node, module.node, DefKind::Module, None);
                    return;
                }
            }
            if let Some(js) = &self.js
                && p == js.field_definition
                && me.field == k.f_property
            {
                // J0: (field_definition property: …) @definition.field
                cx.def(me.node, parent.node, DefKind::Field, None);
                return;
            }
        }
        // Member calls, `new a.B`, `extends a.B` and the catch-all: no
        // locals predicate.
        cx.reference(me.node);
    }

    fn type_identifier(&self, cx: &mut Ctx<'_>, stack: &[Frame<'_>], ts: &TsIds) {
        let k = &self.k;
        let me = stack[stack.len() - 1];
        if let Some(parent) = up(stack, 1)
            && me.field == k.f_name
        {
            let p = parent.kind;
            let kind =
                if p == k.class || p == k.class_declaration || p == ts.abstract_class_declaration {
                    Some(DefKind::Class)
                } else if p == ts.interface_declaration {
                    Some(DefKind::Interface)
                } else if p == ts.type_alias_declaration {
                    Some(DefKind::Type)
                } else {
                    None
                };
            if let Some(kind) = kind {
                cx.def(me.node, parent.node, kind, None);
                return;
            }
        }
        // `implements` and the catch-all: no locals predicate.
        cx.reference(me.node);
    }
}
