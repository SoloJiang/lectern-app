//! Python: queries/python/tags.scm and queries/python/locals.scm as one walk.

use tree_sitter::Language;

use super::{Ctx, Frame, Walker, field_of, ids, up};
use crate::DefKind;

ids! {
    Ids {
        kinds {
            module = "module",
            expression_statement = "expression_statement",
            assignment = "assignment",
            augmented_assignment = "augmented_assignment",
            class_definition = "class_definition",
            function_definition = "function_definition",
            decorated_definition = "decorated_definition",
            block = "block",
            parameters = "parameters",
            lambda = "lambda",
            lambda_parameters = "lambda_parameters",
            list_comprehension = "list_comprehension",
            set_comprehension = "set_comprehension",
            dictionary_comprehension = "dictionary_comprehension",
            generator_expression = "generator_expression",
            for_statement = "for_statement",
            for_in_clause = "for_in_clause",
            named_expression = "named_expression",
            as_pattern = "as_pattern",
            attribute = "attribute",
            argument_list = "argument_list",
            identifier = "identifier",
        }
        tokens {}
        fields {
            f_left = "left",
            f_name = "name",
            f_body = "body",
            f_parameters = "parameters",
            f_definition = "definition",
            f_attribute = "attribute",
            f_superclasses = "superclasses",
            f_alias = "alias",
        }
    }
}

pub(super) struct Python(Ids);

impl Python {
    pub(super) fn new(language: &Language) -> Result<Python, String> {
        Ids::new(language).map(Python)
    }
}

impl Walker for Python {
    fn visit(&self, cx: &mut Ctx<'_>, stack: &[Frame<'_>]) {
        let k = &self.0;
        let me = stack[stack.len() - 1];
        let kind = me.kind;
        // locals.scm: scopes. Functions, lambdas and comprehensions do not
        // inherit; a class body does.
        if kind == k.function_definition
            || kind == k.lambda
            || kind == k.list_comprehension
            || kind == k.set_comprehension
            || kind == k.dictionary_comprehension
            || kind == k.generator_expression
        {
            cx.scope(stack.len() - 1, false);
        } else if kind == k.class_definition {
            cx.scope(stack.len() - 1, true);
        }
        if self.local_definition(stack) {
            cx.local(me.node);
        }
        if kind == k.identifier {
            self.identifier(cx, stack);
        }
    }
}

impl Python {
    /// locals.scm's `@local.definition` captures, on any node.
    fn local_definition(&self, stack: &[Frame<'_>]) -> bool {
        let k = &self.0;
        let me = stack[stack.len() - 1];
        let Some(parent) = up(stack, 1) else {
            return false;
        };
        let p = parent.kind;
        // (parameters (_)), (lambda_parameters (_)): any named child.
        if p == k.parameters || p == k.lambda_parameters {
            return true;
        }
        if me.field == k.f_left
            && (p == k.assignment || p == k.for_statement || p == k.for_in_clause)
        {
            return true;
        }
        if me.kind != k.identifier {
            return me.field == k.f_alias && p == k.as_pattern;
        }
        (me.field == k.f_left && p == k.augmented_assignment)
            || (me.field == k.f_name
                && (p == k.named_expression
                    || p == k.function_definition
                    || p == k.class_definition))
            || (me.field == k.f_alias && p == k.as_pattern)
    }

    fn identifier(&self, cx: &mut Ctx<'_>, stack: &[Frame<'_>]) {
        let k = &self.0;
        let me = stack[stack.len() - 1];
        if let Some(parent) = up(stack, 1) {
            let p = parent.kind;
            // (module (expression_statement (assignment left: (identifier)
            //   @name) @definition.constant))
            if p == k.assignment && me.field == k.f_left {
                let statement = up(stack, 2).filter(|s| s.kind == k.expression_statement);
                let container = up(stack, 3);
                if statement.is_some() && container.is_some_and(|m| m.kind == k.module) {
                    cx.def(me.node, parent.node, DefKind::Constant, None);
                    return;
                }
            }
            if p == k.class_definition && me.field == k.f_name {
                cx.def(me.node, parent.node, DefKind::Class, None);
                return;
            }
            // (class_definition body: (block (expression_statement
            //   (assignment left: (identifier) @name) @definition.field)))
            if p == k.assignment
                && me.field == k.f_left
                && up(stack, 2).is_some_and(|s| s.kind == k.expression_statement)
                && self.class_body(stack, 3)
            {
                cx.def(me.node, parent.node, DefKind::Field, None);
                return;
            }
            if p == k.function_definition
                && me.field == k.f_name
                && let Some(params) = field_of(parent.node, k.f_parameters, k.parameters)
            {
                // (class_definition body: (block (function_definition …)))
                // and through a decorated_definition's `definition`.
                let method = self.class_body(stack, 2)
                    || (parent.field == k.f_definition
                        && up(stack, 2).is_some_and(|d| d.kind == k.decorated_definition)
                        && self.class_body(stack, 3));
                let kind = if method {
                    DefKind::Method
                } else {
                    DefKind::Function
                };
                cx.def(me.node, parent.node, kind, Some(params));
                return;
            }
            // (call function: (attribute attribute: (identifier))) and
            // (attribute attribute: (identifier)): no locals predicate.
            if p == k.attribute && me.field == k.f_attribute {
                cx.reference(me.node);
                return;
            }
            // (class_definition superclasses: (argument_list (identifier))).
            if p == k.argument_list
                && parent.field == k.f_superclasses
                && up(stack, 2).is_some_and(|c| c.kind == k.class_definition)
            {
                cx.reference(me.node);
                return;
            }
        }
        // The plain call and the catch-all are `#is-not? local`.
        cx.nonlocal(me.node);
    }

    /// Whether the frame `level` above the last is a block in a class
    /// definition's `body`.
    fn class_body(&self, stack: &[Frame<'_>], level: usize) -> bool {
        let k = &self.0;
        up(stack, level).is_some_and(|b| b.kind == k.block && b.field == k.f_body)
            && up(stack, level + 1).is_some_and(|c| c.kind == k.class_definition)
    }
}
