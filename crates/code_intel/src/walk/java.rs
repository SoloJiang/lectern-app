//! Java: queries/java/tags.scm and queries/java/locals.scm as one walk.

use tree_sitter::Language;

use super::{Ctx, Frame, Walker, field_of, ids, up};
use crate::DefKind;
use crate::lang::Marker;

ids! {
    Ids {
        kinds {
            class_declaration = "class_declaration",
            record_declaration = "record_declaration",
            enum_declaration = "enum_declaration",
            interface_declaration = "interface_declaration",
            annotation_type_declaration = "annotation_type_declaration",
            annotation_type_element_declaration = "annotation_type_element_declaration",
            method_declaration = "method_declaration",
            constructor_declaration = "constructor_declaration",
            lambda_expression = "lambda_expression",
            formal_parameters = "formal_parameters",
            formal_parameter = "formal_parameter",
            spread_parameter = "spread_parameter",
            catch_formal_parameter = "catch_formal_parameter",
            inferred_parameters = "inferred_parameters",
            field_declaration = "field_declaration",
            constant_declaration = "constant_declaration",
            local_variable_declaration = "local_variable_declaration",
            variable_declarator = "variable_declarator",
            enum_constant = "enum_constant",
            modifiers = "modifiers",
            method_invocation = "method_invocation",
            argument_list = "argument_list",
            field_access = "field_access",
            identifier = "identifier",
            type_identifier = "type_identifier",
            block = "block",
            for_statement = "for_statement",
            enhanced_for_statement = "enhanced_for_statement",
            catch_clause = "catch_clause",
            try_with_resources_statement = "try_with_resources_statement",
            switch_block_statement_group = "switch_block_statement_group",
            resource = "resource",
            instanceof_expression = "instanceof_expression",
            type_pattern = "type_pattern",
            record_pattern_component = "record_pattern_component",
        }
        tokens {
            private = "private",
        }
        fields {
            f_name = "name",
            f_parameters = "parameters",
            f_declarator = "declarator",
            f_arguments = "arguments",
            f_field = "field",
        }
    }
}

pub(super) struct Java(Ids);

impl Java {
    pub(super) fn new(language: &Language) -> Result<Java, String> {
        Ids::new(language).map(Java)
    }
}

impl Walker for Java {
    fn visit(&self, cx: &mut Ctx<'_>, stack: &[Frame<'_>]) {
        let k = &self.0;
        let me = stack[stack.len() - 1];
        let kind = me.kind;
        if kind == k.block
            || kind == k.for_statement
            || kind == k.enhanced_for_statement
            || kind == k.catch_clause
            || kind == k.try_with_resources_statement
            || kind == k.switch_block_statement_group
        {
            cx.scope(stack.len() - 1, true);
        } else if kind == k.method_declaration
            || kind == k.constructor_declaration
            || kind == k.lambda_expression
        {
            cx.scope(stack.len() - 1, false);
        } else if kind == k.modifiers {
            // (modifiers "private" @name) @reference.lectern.private
            let mut cursor = me.node.walk();
            for child in me.node.children(&mut cursor) {
                if child.kind_id() == k.private {
                    cx.marker(child, me.node, Marker::Private);
                }
            }
        } else if kind == k.identifier {
            if self.local_definition(stack) {
                cx.local(me.node);
            }
            self.identifier(cx, stack);
        } else if kind == k.type_identifier {
            self.type_identifier(cx, stack);
        }
    }
}

impl Java {
    /// The kind of the declaration whose `parameters` hold the parameter
    /// `level` frames above the last one: a method, constructor, lambda or
    /// record (None for anything else).
    fn parameters_of(&self, stack: &[Frame<'_>], level: usize) -> Option<u16> {
        let k = &self.0;
        let list = up(stack, level)?;
        let owner = up(stack, level + 1)?;
        (list.kind == k.formal_parameters
            && list.field == k.f_parameters
            && (owner.kind == k.method_declaration
                || owner.kind == k.constructor_declaration
                || owner.kind == k.lambda_expression
                || owner.kind == k.record_declaration))
            .then_some(owner.kind)
    }

    /// locals.scm's `@local.definition` captures (all on identifiers).
    fn local_definition(&self, stack: &[Frame<'_>]) -> bool {
        let k = &self.0;
        let me = stack[stack.len() - 1];
        let Some(parent) = up(stack, 1) else {
            return false;
        };
        let p = parent.kind;
        let name = me.field == k.f_name;
        // A method's, constructor's or lambda's parameters; not a record
        // header's components.
        if name && p == k.formal_parameter {
            return self
                .parameters_of(stack, 2)
                .is_some_and(|o| o != k.record_declaration);
        }
        if name
            && (p == k.catch_formal_parameter
                || p == k.enhanced_for_statement
                || p == k.resource
                || p == k.instanceof_expression)
        {
            return true;
        }
        // (lambda_expression parameters: (identifier)), (inferred_parameters
        // (identifier)), (type_pattern (identifier)),
        // (record_pattern_component (identifier)).
        if (p == k.lambda_expression && me.field == k.f_parameters)
            || p == k.inferred_parameters
            || p == k.type_pattern
            || p == k.record_pattern_component
        {
            return true;
        }
        if name && p == k.variable_declarator {
            // (spread_parameter (variable_declarator name:)),
            // (local_variable_declaration declarator: (variable_declarator name:)).
            if let Some(owner) = up(stack, 2) {
                return (owner.kind == k.spread_parameter
                    && self
                        .parameters_of(stack, 3)
                        .is_some_and(|o| o != k.record_declaration))
                    || (owner.kind == k.local_variable_declaration
                        && parent.field == k.f_declarator);
            }
            return false;
        }
        // (block [(class_declaration name:) (record_declaration name:)
        //   (enum_declaration name:) (interface_declaration name:)]).
        name && (p == k.class_declaration
            || p == k.record_declaration
            || p == k.enum_declaration
            || p == k.interface_declaration)
            && up(stack, 2).is_some_and(|b| b.kind == k.block)
    }

    fn identifier(&self, cx: &mut Ctx<'_>, stack: &[Frame<'_>]) {
        let k = &self.0;
        let me = stack[stack.len() - 1];
        if let Some(parent) = up(stack, 1) {
            let p = parent.kind;
            if me.field == k.f_name {
                let kind = if p == k.class_declaration || p == k.record_declaration {
                    Some(DefKind::Class)
                } else if p == k.enum_declaration {
                    Some(DefKind::Enum)
                } else if p == k.interface_declaration || p == k.annotation_type_declaration {
                    Some(DefKind::Interface)
                } else {
                    None
                };
                if let Some(kind) = kind {
                    cx.def(me.node, parent.node, kind, None);
                    return;
                }
                if p == k.method_declaration
                    && let Some(params) = field_of(parent.node, k.f_parameters, k.formal_parameters)
                {
                    cx.def(me.node, parent.node, DefKind::Method, Some(params));
                    return;
                }
                // (field_declaration declarator: (variable_declarator name:))
                // and the same for constant_declaration.
                if p == k.variable_declarator
                    && parent.field == k.f_declarator
                    && let Some(owner) = up(stack, 2)
                {
                    let kind = if owner.kind == k.field_declaration {
                        Some(DefKind::Field)
                    } else if owner.kind == k.constant_declaration {
                        Some(DefKind::Constant)
                    } else {
                        None
                    };
                    if let Some(kind) = kind {
                        cx.def(me.node, owner.node, kind, None);
                        return;
                    }
                }
                if p == k.enum_constant {
                    cx.def(me.node, parent.node, DefKind::Constant, None);
                    return;
                }
                // (record_declaration parameters: (formal_parameters
                //   [(formal_parameter name:) (spread_parameter
                //   (variable_declarator name:))] @definition.field))
                if p == k.formal_parameter
                    && self.parameters_of(stack, 2) == Some(k.record_declaration)
                {
                    cx.def(me.node, parent.node, DefKind::Field, None);
                    return;
                }
                if p == k.variable_declarator
                    && let Some(spread) = up(stack, 2)
                    && spread.kind == k.spread_parameter
                    && self.parameters_of(stack, 3) == Some(k.record_declaration)
                {
                    cx.def(me.node, spread.node, DefKind::Field, None);
                    return;
                }
                if p == k.annotation_type_element_declaration {
                    cx.def(me.node, parent.node, DefKind::Method, None);
                    return;
                }
                // (method_invocation name: (identifier) @name
                //   arguments: (argument_list) @reference.call)
                if p == k.method_invocation
                    && field_of(parent.node, k.f_arguments, k.argument_list).is_some()
                {
                    cx.reference(me.node);
                    return;
                }
            }
            // (field_access field: (identifier)): no locals predicate.
            if p == k.field_access && me.field == k.f_field {
                cx.reference(me.node);
                return;
            }
        }
        cx.nonlocal(me.node);
    }

    fn type_identifier(&self, cx: &mut Ctx<'_>, stack: &[Frame<'_>]) {
        // type_list, object creation, superclass and the catch-all
        // references: none has a locals predicate.
        let me = stack[stack.len() - 1];
        cx.reference(me.node);
    }
}
