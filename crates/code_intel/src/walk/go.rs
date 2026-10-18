//! Go: queries/go/tags.scm and queries/go/locals.scm as one walk.

use tree_sitter::Language;

use super::{Ctx, Frame, Walker, field_of, ids, up};
use crate::DefKind;
use crate::lang::Marker;

ids! {
    Ids {
        kinds {
            source_file = "source_file",
            function_declaration = "function_declaration",
            method_declaration = "method_declaration",
            method_elem = "method_elem",
            func_literal = "func_literal",
            type_spec = "type_spec",
            type_alias = "type_alias",
            const_declaration = "const_declaration",
            const_spec = "const_spec",
            var_declaration = "var_declaration",
            var_spec = "var_spec",
            var_spec_list = "var_spec_list",
            field_declaration = "field_declaration",
            parameter_list = "parameter_list",
            parameter_declaration = "parameter_declaration",
            variadic_parameter_declaration = "variadic_parameter_declaration",
            pointer_type = "pointer_type",
            generic_type = "generic_type",
            identifier = "identifier",
            type_identifier = "type_identifier",
            field_identifier = "field_identifier",
            block = "block",
            for_statement = "for_statement",
            if_statement = "if_statement",
            expression_switch_statement = "expression_switch_statement",
            type_switch_statement = "type_switch_statement",
            expression_case = "expression_case",
            type_case = "type_case",
            default_case = "default_case",
            communication_case = "communication_case",
            function_type = "function_type",
            statement_list = "statement_list",
            short_var_declaration = "short_var_declaration",
            range_clause = "range_clause",
            receive_statement = "receive_statement",
            expression_list = "expression_list",
        }
        tokens {}
        fields {
            f_name = "name",
            f_parameters = "parameters",
            f_receiver = "receiver",
            f_type = "type",
            f_left = "left",
            f_alias = "alias",
        }
    }
}

pub(super) struct Go(Ids);

impl Go {
    pub(super) fn new(language: &Language) -> Result<Go, String> {
        Ids::new(language).map(Go)
    }
}

impl Walker for Go {
    fn visit(&self, cx: &mut Ctx<'_>, stack: &[Frame<'_>]) {
        let k = &self.0;
        let me = stack[stack.len() - 1];
        let kind = me.kind;
        // locals.scm: scopes.
        if kind == k.block
            || kind == k.for_statement
            || kind == k.if_statement
            || kind == k.expression_switch_statement
            || kind == k.type_switch_statement
            || kind == k.expression_case
            || kind == k.type_case
            || kind == k.default_case
            || kind == k.communication_case
        {
            cx.scope(stack, true);
            return;
        }
        if kind == k.function_declaration
            || kind == k.method_declaration
            || kind == k.func_literal
            || kind == k.method_elem
            || kind == k.function_type
        {
            cx.scope(stack, false);
            return;
        }
        if kind == k.identifier {
            if self.local_definition(stack) {
                cx.local(me.node);
            }
            self.identifier(cx, stack);
        } else if kind == k.type_identifier {
            self.type_identifier(cx, stack);
        } else if kind == k.field_identifier {
            self.field_identifier(cx, stack);
        }
    }
}

impl Go {
    /// locals.scm's `@local.definition` captures (all on identifiers).
    fn local_definition(&self, stack: &[Frame<'_>]) -> bool {
        let k = &self.0;
        let me = stack[stack.len() - 1];
        let Some(parent) = up(stack, 1) else {
            return false;
        };
        if me.field == k.f_name
            && (parent.kind == k.parameter_declaration
                || parent.kind == k.variadic_parameter_declaration)
        {
            return true;
        }
        if parent.kind == k.expression_list {
            if let Some(owner) = up(stack, 2) {
                let left = parent.field == k.f_left
                    && (owner.kind == k.short_var_declaration
                        || owner.kind == k.range_clause
                        || owner.kind == k.receive_statement);
                let alias = parent.field == k.f_alias && owner.kind == k.type_switch_statement;
                return left || alias;
            }
            return false;
        }
        // (statement_list | source_file) (var_declaration [(var_spec name:)
        // (var_spec_list (var_spec name:))]) and the same for const.
        if me.field == k.f_name && (parent.kind == k.var_spec || parent.kind == k.const_spec) {
            return self.declaration_at(stack, 2, false) || self.declaration_at(stack, 2, true);
        }
        false
    }

    /// Whether the spec 1 level above the name sits in a var or const
    /// declaration directly in a statement list (`in_file` false) or the
    /// source file (`in_file` true).
    fn declaration_at(&self, stack: &[Frame<'_>], from: usize, in_file: bool) -> bool {
        let k = &self.0;
        let Some(spec) = up(stack, from - 1) else {
            return false;
        };
        let mut level = from;
        let mut declaration = up(stack, level);
        if spec.kind == k.var_spec && declaration.is_some_and(|d| d.kind == k.var_spec_list) {
            level += 1;
            declaration = up(stack, level);
        }
        let Some(declaration) = declaration else {
            return false;
        };
        let expected = if spec.kind == k.var_spec {
            k.var_declaration
        } else {
            k.const_declaration
        };
        if declaration.kind != expected {
            return false;
        }
        let container = if in_file {
            k.source_file
        } else {
            k.statement_list
        };
        up(stack, level + 1).is_some_and(|c| c.kind == container)
    }

    fn identifier(&self, cx: &mut Ctx<'_>, stack: &[Frame<'_>]) {
        let k = &self.0;
        let me = stack[stack.len() - 1];
        if let Some(parent) = up(stack, 1) {
            // (function_declaration name: (identifier) @name
            //   parameters: (parameter_list) @doc) @definition.function
            if parent.kind == k.function_declaration
                && me.field == k.f_name
                && let Some(params) = field_of(parent.node, k.f_parameters, k.parameter_list)
            {
                cx.def(me.node, parent.node, DefKind::Function, Some(params));
                return;
            }
            // (source_file (const_declaration (const_spec (identifier))));
            // a const_spec's identifier children are its names.
            if parent.kind == k.const_spec
                && me.field == k.f_name
                && self.declaration_at(stack, 2, true)
            {
                cx.def(me.node, parent.node, DefKind::Constant, None);
                return;
            }
            if parent.kind == k.var_spec
                && me.field == k.f_name
                && self.declaration_at(stack, 2, true)
            {
                cx.def(me.node, parent.node, DefKind::Variable, None);
                return;
            }
        }
        // The call reference and the catch-all are both `#is-not? local`.
        cx.nonlocal(me.node);
    }

    fn type_identifier(&self, cx: &mut Ctx<'_>, stack: &[Frame<'_>]) {
        let k = &self.0;
        let me = stack[stack.len() - 1];
        if let Some(parent) = up(stack, 1) {
            if (parent.kind == k.type_spec || parent.kind == k.type_alias) && me.field == k.f_name {
                cx.def(me.node, parent.node, DefKind::Type, None);
                return;
            }
            // (method_declaration receiver: (parameter_list
            //   (parameter_declaration type: [T (pointer_type T)
            //   (generic_type type: T) (pointer_type (generic_type type: T))])))
            if let Some(method) = self.receiver_owner(stack) {
                cx.marker(me.node, method, Marker::Owner);
                return;
            }
        }
        cx.reference(me.node);
    }

    fn receiver_owner<'t>(&self, stack: &[Frame<'t>]) -> Option<tree_sitter::Node<'t>> {
        let k = &self.0;
        let me = stack[stack.len() - 1];
        let mut level = 1;
        let mut child = me;
        let mut frame = up(stack, level)?;
        // (generic_type type: T)
        if frame.kind == k.generic_type {
            if child.field != k.f_type {
                return None;
            }
            child = *frame;
            level += 1;
            frame = up(stack, level)?;
        }
        // (pointer_type X)
        if frame.kind == k.pointer_type {
            child = *frame;
            level += 1;
            frame = up(stack, level)?;
        }
        if frame.kind != k.parameter_declaration || child.field != k.f_type {
            return None;
        }
        let list = up(stack, level + 1)?;
        let method = up(stack, level + 2)?;
        (list.kind == k.parameter_list
            && list.field == k.f_receiver
            && method.kind == k.method_declaration)
            .then_some(method.node)
    }

    fn field_identifier(&self, cx: &mut Ctx<'_>, stack: &[Frame<'_>]) {
        let k = &self.0;
        let me = stack[stack.len() - 1];
        if let Some(parent) = up(stack, 1) {
            if (parent.kind == k.method_declaration || parent.kind == k.method_elem)
                && me.field == k.f_name
                && let Some(params) = field_of(parent.node, k.f_parameters, k.parameter_list)
            {
                cx.def(me.node, parent.node, DefKind::Method, Some(params));
                return;
            }
            if parent.kind == k.field_declaration && me.field == k.f_name {
                cx.def(me.node, parent.node, DefKind::Field, None);
                return;
            }
        }
        // The member call and the catch-all have no locals predicate.
        cx.reference(me.node);
    }
}
