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
            export_clause = "export_clause",
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
            switch_body = "switch_body",
            with_statement = "with_statement",
            identifier = "identifier",
            property_identifier = "property_identifier",
            private_property_identifier = "private_property_identifier",
            shorthand_property_identifier = "shorthand_property_identifier",
            shorthand_property_identifier_pattern = "shorthand_property_identifier_pattern",
            object_pattern = "object_pattern",
            array_pattern = "array_pattern",
            pair_pattern = "pair_pattern",
            assignment_pattern = "assignment_pattern",
            object_assignment_pattern = "object_assignment_pattern",
            rest_pattern = "rest_pattern",
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
            f_body = "body",
        }
    }
}

ids! {
    TsIds {
        kinds {
            function_signature = "function_signature",
            method_signature = "method_signature",
            abstract_method_signature = "abstract_method_signature",
            call_signature = "call_signature",
            construct_signature = "construct_signature",
            function_type = "function_type",
            constructor_type = "constructor_type",
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
            t_assign = "=",
            t_global = "global",
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

ids! {
    JsxIds {
        kinds {
            jsx_opening_element = "jsx_opening_element",
            jsx_closing_element = "jsx_closing_element",
            jsx_self_closing_element = "jsx_self_closing_element",
        }
        tokens {}
        fields {}
    }
}

pub(super) struct Ecma {
    k: Ids,
    ts: Option<TsIds>,
    js: Option<JsIds>,
    jsx: Option<JsxIds>,
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
            jsx: matches!(dialect, Dialect::JavaScript | Dialect::Tsx)
                .then(|| JsxIds::new(language))
                .transpose()?,
        })
    }
}

impl Walker for Ecma {
    fn visit(&self, cx: &mut Ctx<'_>, stack: &[Frame<'_>]) {
        let k = &self.k;
        let me = stack[stack.len() - 1];
        let kind = me.kind;
        // ecmascript/locals.scm and ts/locals.scm: scopes. Blocks inherit;
        // functions, and TS signatures and function types, do not.
        if kind == k.statement_block {
            if self.namespace_body(stack) {
                cx.member_scope(stack, true);
            } else {
                cx.scope(stack, true);
            }
        } else if kind == k.for_statement
            || kind == k.for_in_statement
            || kind == k.catch_clause
            || kind == k.switch_body
        {
            cx.scope(stack, true);
        } else if kind == k.function_expression
            || kind == k.arrow_function
            || kind == k.function_declaration
            || kind == k.generator_function
            || kind == k.generator_function_declaration
            || kind == k.method_definition
            || self.ts.as_ref().is_some_and(|ts| {
                kind == ts.function_signature
                    || kind == ts.method_signature
                    || kind == ts.abstract_method_signature
                    || kind == ts.call_signature
                    || kind == ts.construct_signature
                    || kind == ts.function_type
                    || kind == ts.constructor_type
            })
        {
            cx.scope(stack, false);
        } else if kind == k.export_statement && self.in_program(stack, 1) {
            // (program (export_statement "export" @name)
            //   @reference.lectern.export)
            self.tokens(cx, me.node, k.t_export, Marker::Export);
        } else if kind == k.import_statement && self.in_program(stack, 1) {
            // (program (import_statement "import" @name)
            //   @reference.lectern.module)
            self.tokens(cx, me.node, k.t_import, Marker::Module);
        } else if let Some(ts) = &self.ts
            && kind == ts.ambient_declaration
            && self.in_program(stack, 1)
        {
            // (program (ambient_declaration "global" @name))
            //   @reference.lectern.global
            self.tokens(cx, me.node, ts.t_global, Marker::Global);
        }
        // (with_statement body: (_) @local.scope): a second scope on a
        // block body, which changes nothing for tree-sitter-tags.
        if me.field == k.f_body && up(stack, 1).is_some_and(|p| p.kind == k.with_statement) {
            cx.scope(stack, true);
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
        } else if kind == k.shorthand_property_identifier_pattern {
            // E8, E9 in a destructuring pattern; no other pattern names it.
            if let Some(level) = self.declarator(stack)
                && let Some(constant) = self.top_level_declaration(stack, level)
            {
                self.top_level_def(cx, stack, level, constant);
            }
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

    /// Whether the frame `level` above the last is the program.
    fn in_program(&self, stack: &[Frame<'_>], level: usize) -> bool {
        up(stack, level).is_some_and(|p| p.kind == self.k.program)
    }

    /// TS: a namespace or module body, or the block of `declare global`
    /// (ts/locals.scm's "namespace" scopes).
    fn namespace_body(&self, stack: &[Frame<'_>]) -> bool {
        let Some(ts) = &self.ts else {
            return false;
        };
        let me = stack[stack.len() - 1];
        up(stack, 1).is_some_and(|p| {
            (me.field == self.k.f_body && (p.kind == ts.internal_module || p.kind == ts.module))
                || p.kind == ts.ambient_declaration
        })
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

    /// E8 and E9: the level of the variable declarator whose `name` binds
    /// the name being visited, as tags.scm spells it: the name itself, or a
    /// name in an object or array pattern, one more pattern deep at most.
    fn declarator(&self, stack: &[Frame<'_>]) -> Option<usize> {
        let k = &self.k;
        let me = stack[stack.len() - 1];
        let parent = up(stack, 1)?;
        if parent.kind == k.variable_declarator {
            return (me.field == k.f_name).then_some(1);
        }
        let mut level = self.pattern_leaf(stack, 0)?;
        let pattern = up(stack, level)?;
        let mut above = up(stack, level + 1)?;
        // An inner pattern: a pair's value in an object pattern, or an
        // element of an array pattern.
        if above.kind == k.pair_pattern && pattern.field == k.f_value {
            let outer = up(stack, level + 2)?;
            if outer.kind != k.object_pattern {
                return None;
            }
            level += 2;
            above = up(stack, level + 1)?;
        } else if above.kind == k.array_pattern {
            level += 1;
            above = up(stack, level + 1)?;
        }
        let outer = up(stack, level)?;
        (above.kind == k.variable_declarator && outer.field == k.f_name).then_some(level + 1)
    }

    /// The level of the object or array pattern that has the frame `at`
    /// levels above the last as a leaf: `{a}`, `{b: a}`, `{b: a = 1}`,
    /// `{a = 1}`, `{...a}`, `[a]`, `[a = 1]` and `[...a]`.
    fn pattern_leaf(&self, stack: &[Frame<'_>], at: usize) -> Option<usize> {
        let k = &self.k;
        let me = up(stack, at)?;
        let parent = up(stack, at + 1)?;
        let p = parent.kind;
        if me.kind == k.shorthand_property_identifier_pattern {
            if p == k.object_pattern {
                return Some(at + 1);
            }
            let object = up(stack, at + 2)?;
            return (p == k.object_assignment_pattern
                && me.field == k.f_left
                && object.kind == k.object_pattern)
                .then_some(at + 2);
        }
        if me.kind != k.identifier {
            return None;
        }
        if p == k.array_pattern {
            return Some(at + 1);
        }
        let grand = up(stack, at + 2)?;
        let g = grand.kind;
        if p == k.rest_pattern && (g == k.object_pattern || g == k.array_pattern) {
            return Some(at + 2);
        }
        if p == k.pair_pattern && me.field == k.f_value && g == k.object_pattern {
            return Some(at + 2);
        }
        if p == k.assignment_pattern && me.field == k.f_left {
            if g == k.array_pattern {
                return Some(at + 2);
            }
            let object = up(stack, at + 3)?;
            if g == k.pair_pattern && parent.field == k.f_value && object.kind == k.object_pattern {
                return Some(at + 3);
            }
        }
        None
    }

    /// E8 and E9: a declarator `level` frames above the name, at the top
    /// level of the program, directly or in an export statement's
    /// `declaration`; Some(true) for `const`.
    fn top_level_declaration(&self, stack: &[Frame<'_>], level: usize) -> Option<bool> {
        let k = &self.k;
        let declaration = up(stack, level + 1)?;
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
        // TS: exported in a namespace or module body, a plain name only
        // (ts/tags.scm).
        if let Some(ts) = &self.ts
            && level == 1
            && declaration.field == k.f_declaration
            && up(stack, level + 2).is_some_and(|e| e.kind == k.export_statement)
            && let Some(body) = up(stack, level + 3)
            && body.kind == k.statement_block
            && body.field == k.f_body
            && up(stack, level + 4)
                .is_some_and(|m| m.kind == ts.internal_module || m.kind == ts.module)
        {
            return Some(constant);
        }
        // TS: `declare const` sits in an ambient declaration
        // (ts/tags.scm).
        let mut level = level + 2;
        let mut placed = declaration;
        if let Some(ts) = &self.ts
            && let Some(ambient) = up(stack, level)
            && ambient.kind == ts.ambient_declaration
        {
            placed = ambient;
            level += 1;
        }
        let container = up(stack, level)?;
        let at_top = container.kind == k.program
            || (container.kind == k.export_statement
                && placed.field == k.f_declaration
                && up(stack, level + 1).is_some_and(|p| p.kind == k.program));
        at_top.then_some(constant)
    }

    fn top_level_def(&self, cx: &mut Ctx<'_>, stack: &[Frame<'_>], level: usize, constant: bool) {
        let me = stack[stack.len() - 1];
        let Some(declarator) = up(stack, level) else {
            return;
        };
        let kind = if constant {
            DefKind::Constant
        } else {
            DefKind::Variable
        };
        cx.def(me.node, declarator.node, kind, None);
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
            // E8, E9: top-level const, let and var declarators, also
            // destructured.
            if let Some(level) = self.declarator(stack)
                && let Some(constant) = self.top_level_declaration(stack, level)
            {
                self.top_level_def(cx, stack, level, constant);
                return;
            }
            // E10 (call, not `require`) and E12 (new) are `#is-not? local`
            // like the catch-all; a local name falls through to them.
            // E15: (program (export_statement (export_clause
            // (export_specifier name: (identifier))))) and E16: (program
            // (export_statement value: (identifier))) are markers.
            if (name
                && p == k.export_specifier
                && up(stack, 2).is_some_and(|c| c.kind == k.export_clause)
                && up(stack, 3).is_some_and(|s| s.kind == k.export_statement)
                && self.in_program(stack, 4))
                || (p == k.export_statement && me.field == k.f_value && self.in_program(stack, 2))
            {
                cx.marker(me.node, parent.node, Marker::ExportName);
                return;
            }
            if let Some(ts) = &self.ts {
                // (program (export_statement "=" (identifier) @name))
                //   @reference.lectern.export-name
                if p == k.export_statement
                    && me.field == 0
                    && me
                        .node
                        .prev_sibling()
                        .is_some_and(|t| t.kind_id() == ts.t_assign)
                    && self.in_program(stack, 2)
                {
                    cx.marker(me.node, parent.node, Marker::ExportName);
                    return;
                }
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
        // jsx/tags.scm: a lowercase element name is an intrinsic element,
        // `@ignore`d.
        if let Some(jsx) = &self.jsx
            && let Some(parent) = up(stack, 1)
            && me.field == k.f_name
            && (parent.kind == jsx.jsx_opening_element
                || parent.kind == jsx.jsx_closing_element
                || parent.kind == jsx.jsx_self_closing_element)
            && cx.text(me.node).first().is_some_and(u8::is_ascii_lowercase)
        {
            return;
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
