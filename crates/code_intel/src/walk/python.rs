//! Python: queries/python/tags.scm and queries/python/locals.scm as one walk.

use tree_sitter::Language;

use super::{Ctx, Frame, Walker, field_of, ids, up};
use crate::DefKind;
use crate::lang::Marker;

ids! {
    Ids {
        kinds {
            expression_statement = "expression_statement",
            assignment = "assignment",
            type_alias_statement = "type_alias_statement",
            type_node = "type",
            generic_type = "generic_type",
            pattern_list = "pattern_list",
            tuple_pattern = "tuple_pattern",
            list_pattern = "list_pattern",
            list_splat_pattern = "list_splat_pattern",
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
            f_object = "object",
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
            cx.scope(stack, false);
        } else if kind == k.class_definition {
            // Its definitions are members.
            cx.member_scope(stack, true);
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
            // (type_alias_statement left: (type (identifier) @name))
            //   @definition.type: no other pattern names this node.
            if p == k.type_node
                && parent.field == k.f_left
                && let Some(alias) = up(stack, 2)
                && alias.kind == k.type_alias_statement
            {
                cx.def(me.node, alias.node, DefKind::Type, None);
                return;
            }
            // … and `type Alias[T] = …`: (type (generic_type (identifier))).
            if p == k.generic_type
                && let Some(ty) = up(stack, 2)
                && ty.kind == k.type_node
                && ty.field == k.f_left
                && let Some(alias) = up(stack, 3)
                && alias.kind == k.type_alias_statement
            {
                cx.def(me.node, alias.node, DefKind::Type, None);
                return;
            }
            // (class_definition body: (block (expression_statement
            //   (assignment left: [(identifier) (pattern_list …)
            //   (tuple_pattern …) (list_pattern …)])))) @definition.field,
            //   then (assignment left: …) @definition.constant anywhere
            //   (`rules::finish` keeps the module-level ones): the
            //   assignment's level above the name.
            let target = self.assignment_target(stack);
            if let Some(level) = target
                && let Some(assignment) = up(stack, level)
                && up(stack, level + 1).is_some_and(|s| s.kind == k.expression_statement)
                && self.class_body(stack, level + 2)
            {
                cx.def(me.node, assignment.node, DefKind::Field, None);
                return;
            }
            if p == k.class_definition && me.field == k.f_name {
                cx.def(me.node, parent.node, DefKind::Class, None);
                return;
            }
            if let Some(level) = target
                && let Some(assignment) = up(stack, level)
            {
                cx.def(me.node, assignment.node, DefKind::Constant, None);
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
            // (function_definition parameters: (parameters . (identifier)))
            //   @reference.lectern.receiver
            if p == k.parameters
                && parent.field == k.f_parameters
                && parent.node.named_child(0) == Some(me.node)
                && let Some(function) = up(stack, 2)
                && function.kind == k.function_definition
            {
                cx.marker(me.node, function.node, Marker::Receiver);
                return;
            }
            // (assignment left: [(attribute object: (identifier)
            //   attribute: (identifier) @name) @reference.lectern.attribute
            //   (pattern_list (attribute …)) (tuple_pattern (attribute …))])
            if p == k.attribute
                && me.field == k.f_attribute
                && parent
                    .node
                    .child_by_field_id(k.f_object)
                    .is_some_and(|o| o.kind_id() == k.identifier)
                && self.assigned(stack, 1)
            {
                cx.marker(me.node, parent.node, Marker::Attribute);
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

    /// The level of the assignment whose `left` binds the name being
    /// visited, as tags.scm spells it: the name itself, or a name (or
    /// `*name`) in a pattern list, tuple or list pattern, one more tuple or
    /// list pattern deep at most.
    fn assignment_target(&self, stack: &[Frame<'_>]) -> Option<usize> {
        let k = &self.0;
        let me = stack[stack.len() - 1];
        let mut level = 1;
        let mut frame = up(stack, level)?;
        if frame.kind == k.assignment {
            return (me.field == k.f_left).then_some(level);
        }
        if frame.kind == k.list_splat_pattern {
            level += 1;
            frame = up(stack, level)?;
        }
        let pattern =
            |kind: u16| kind == k.pattern_list || kind == k.tuple_pattern || kind == k.list_pattern;
        if !pattern(frame.kind) {
            return None;
        }
        // An inner tuple or list pattern, inside an outer pattern.
        let outer = up(stack, level + 1)?;
        if pattern(outer.kind) && frame.kind != k.pattern_list {
            level += 1;
            frame = outer;
        }
        let assignment = up(stack, level + 1)?;
        (assignment.kind == k.assignment && frame.field == k.f_left).then_some(level + 1)
    }

    /// Whether the frame `level` above the last is an assignment's `left`,
    /// directly or as an element of a pattern list or tuple pattern there.
    fn assigned(&self, stack: &[Frame<'_>], level: usize) -> bool {
        let k = &self.0;
        let Some(target) = up(stack, level) else {
            return false;
        };
        let is_assignment = |l: usize| up(stack, l).is_some_and(|a| a.kind == k.assignment);
        if target.field == k.f_left && is_assignment(level + 1) {
            return true;
        }
        up(stack, level + 1).is_some_and(|list| {
            (list.kind == k.pattern_list || list.kind == k.tuple_pattern)
                && list.field == k.f_left
                && is_assignment(level + 2)
        })
    }

    /// Whether the frame `level` above the last is a block in a class
    /// definition's `body`.
    fn class_body(&self, stack: &[Frame<'_>], level: usize) -> bool {
        let k = &self.0;
        up(stack, level).is_some_and(|b| b.kind == k.block && b.field == k.f_body)
            && up(stack, level + 1).is_some_and(|c| c.kind == k.class_definition)
    }
}
