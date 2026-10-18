//! Rust: queries/rust/tags.scm and queries/rust/locals.scm as one walk.

use tree_sitter::{Language, Node};

use super::{Ctx, Frame, Walker, field_of, ids, up};
use crate::DefKind;
use crate::lang::Marker;

ids! {
    Ids {
        kinds {
            struct_item = "struct_item",
            enum_item = "enum_item",
            enum_variant = "enum_variant",
            union_item = "union_item",
            type_item = "type_item",
            associated_type = "associated_type",
            impl_item = "impl_item",
            trait_item = "trait_item",
            declaration_list = "declaration_list",
            function_item = "function_item",
            function_signature_item = "function_signature_item",
            function_type = "function_type",
            parameters = "parameters",
            parameter = "parameter",
            closure_expression = "closure_expression",
            closure_parameters = "closure_parameters",
            mod_item = "mod_item",
            macro_definition = "macro_definition",
            const_item = "const_item",
            static_item = "static_item",
            field_declaration = "field_declaration",
            visibility_modifier = "visibility_modifier",
            generic_type = "generic_type",
            scoped_type_identifier = "scoped_type_identifier",
            call_expression = "call_expression",
            scoped_identifier = "scoped_identifier",
            macro_invocation = "macro_invocation",
            identifier = "identifier",
            type_identifier = "type_identifier",
            field_identifier = "field_identifier",
            block = "block",
            for_expression = "for_expression",
            match_arm = "match_arm",
            match_pattern = "match_pattern",
            let_declaration = "let_declaration",
            primitive_type = "primitive_type",
            reference_type = "reference_type",
            pointer_type = "pointer_type",
            array_type = "array_type",
            tuple_type = "tuple_type",
        }
        tokens {
            t_for = "for",
        }
        fields {
            f_name = "name",
            f_parameters = "parameters",
            f_body = "body",
            f_type = "type",
            f_function = "function",
            f_macro = "macro",
            f_pattern = "pattern",
            f_element = "element",
        }
    }
}

pub(super) struct Rust(Ids);

impl Rust {
    pub(super) fn new(language: &Language) -> Result<Rust, String> {
        Ids::new(language).map(Rust)
    }
}

impl Walker for Rust {
    fn visit(&self, cx: &mut Ctx<'_>, stack: &[Frame<'_>]) {
        let k = &self.0;
        let me = stack[stack.len() - 1];
        let kind = me.kind;
        if kind == k.block || kind == k.for_expression || kind == k.match_arm {
            cx.scope(stack, true);
        } else if kind == k.function_item
            || kind == k.closure_expression
            || kind == k.function_signature_item
            || kind == k.function_type
        {
            cx.scope(stack, false);
        }
        if self.local_definition(stack) {
            cx.local(me.node);
        }
        if kind == k.identifier {
            self.identifier(cx, stack);
        } else if kind == k.type_identifier {
            self.type_identifier(cx, stack);
        } else if kind == k.field_identifier {
            self.field_identifier(cx, stack);
        } else if kind == k.visibility_modifier {
            // (visibility_modifier) @name @reference.lectern.pub
            cx.marker(me.node, me.node, Marker::Pub);
        } else if kind == k.primitive_type {
            // The implemented type of an `impl` block, when it is primitive;
            // no other pattern names a primitive type.
            if let Some(implementation) = self.impl_of(stack) {
                cx.marker(me.node, implementation, Marker::Impl);
            }
        } else if kind == k.impl_item {
            // (impl_item "for" @name) @reference.lectern.impl-trait
            let mut cursor = me.node.walk();
            for child in me.node.children(&mut cursor) {
                if child.kind_id() == k.t_for {
                    cx.marker(child, me.node, Marker::ImplTrait);
                }
            }
        }
    }
}

impl Rust {
    /// locals.scm's `@local.definition` captures, on any node (the
    /// `if let` / `while let` bindings are `@definition.lectern.local`,
    /// which tree-sitter-tags ignores).
    fn local_definition(&self, stack: &[Frame<'_>]) -> bool {
        let k = &self.0;
        let me = stack[stack.len() - 1];
        let Some(parent) = up(stack, 1) else {
            return false;
        };
        let p = parent.kind;
        if me.field == k.f_pattern
            && (p == k.parameter || p == k.let_declaration || p == k.for_expression)
        {
            return true;
        }
        if p == k.closure_parameters {
            return true;
        }
        if p == k.match_arm && me.field == k.f_pattern && me.kind == k.match_pattern {
            return true;
        }
        // (block (function_item name:)), (block [(const_item name:)
        //   (static_item name:)]).
        me.kind == k.identifier
            && me.field == k.f_name
            && (p == k.function_item || p == k.const_item || p == k.static_item)
            && up(stack, 2).is_some_and(|b| b.kind == k.block)
    }

    fn identifier(&self, cx: &mut Ctx<'_>, stack: &[Frame<'_>]) {
        let k = &self.0;
        let me = stack[stack.len() - 1];
        if let Some(parent) = up(stack, 1) {
            let p = parent.kind;
            if me.field == k.f_name {
                if p == k.enum_variant {
                    cx.def(me.node, parent.node, DefKind::Constant, None);
                    return;
                }
                if (p == k.function_item || p == k.function_signature_item)
                    && let Some(params) = field_of(parent.node, k.f_parameters, k.parameters)
                {
                    // In the body of an impl or a trait: a method.
                    let method = up(stack, 2).is_some_and(|list| {
                        list.kind == k.declaration_list
                            && list.field == k.f_body
                            && up(stack, 3)
                                .is_some_and(|i| i.kind == k.impl_item || i.kind == k.trait_item)
                    });
                    let kind = if method {
                        DefKind::Method
                    } else {
                        DefKind::Function
                    };
                    cx.def(me.node, parent.node, kind, Some(params));
                    return;
                }
                let kind = if p == k.mod_item {
                    Some(DefKind::Module)
                } else if p == k.macro_definition {
                    Some(DefKind::Macro)
                } else if p == k.const_item || p == k.static_item {
                    Some(DefKind::Constant)
                } else {
                    None
                };
                if let Some(kind) = kind {
                    cx.def(me.node, parent.node, kind, None);
                    return;
                }
                // (call_expression function: (scoped_identifier name:)) and
                // (macro_invocation macro: (scoped_identifier name:)).
                if p == k.scoped_identifier
                    && let Some(owner) = up(stack, 2)
                    && ((owner.kind == k.call_expression && parent.field == k.f_function)
                        || (owner.kind == k.macro_invocation && parent.field == k.f_macro))
                {
                    cx.reference(me.node);
                    return;
                }
            }
            // (macro_invocation macro: (identifier)): no locals predicate.
            if p == k.macro_invocation && me.field == k.f_macro {
                cx.reference(me.node);
                return;
            }
        }
        // The plain call and the catch-all are `#is-not? local`.
        cx.nonlocal(me.node);
    }

    fn type_identifier(&self, cx: &mut Ctx<'_>, stack: &[Frame<'_>]) {
        let k = &self.0;
        let me = stack[stack.len() - 1];
        if let Some(parent) = up(stack, 1) {
            let p = parent.kind;
            if me.field == k.f_name {
                let kind = if p == k.struct_item || p == k.union_item {
                    Some(DefKind::Struct)
                } else if p == k.enum_item {
                    Some(DefKind::Enum)
                } else if p == k.type_item || p == k.associated_type {
                    Some(DefKind::Type)
                } else if p == k.trait_item {
                    Some(DefKind::Trait)
                } else {
                    None
                };
                if let Some(kind) = kind {
                    cx.def(me.node, parent.node, kind, None);
                    return;
                }
            }
            // (impl_item type: …) @reference.lectern.impl
            if let Some(implementation) = self.impl_of(stack) {
                cx.marker(me.node, implementation, Marker::Impl);
                return;
            }
        }
        // The implemented trait, struct expressions and the catch-all: no
        // locals predicate.
        cx.reference(me.node);
    }

    /// The `impl_item` whose implemented type the name being visited (a
    /// type identifier or a primitive type) names, as tags.scm spells it:
    /// the name itself, a path's last segment or a generic type's own name,
    /// inside at most two references, pointers, arrays or tuples (the first
    /// element).
    fn impl_of<'t>(&self, stack: &[Frame<'t>]) -> Option<Node<'t>> {
        let k = &self.0;
        let me = stack[stack.len() - 1];
        let mut level = 0;
        let mut at = me;
        if me.kind == k.type_identifier {
            // (scoped_type_identifier name: T), then (generic_type type: …).
            let parent = up(stack, level + 1)?;
            if parent.kind == k.scoped_type_identifier && at.field == k.f_name {
                level += 1;
                at = *parent;
            }
            let parent = up(stack, level + 1)?;
            if parent.kind == k.generic_type && at.field == k.f_type {
                level += 1;
                at = *parent;
            }
        }
        for _ in 0..2 {
            let parent = up(stack, level + 1)?;
            let p = parent.kind;
            let wrapped = ((p == k.reference_type || p == k.pointer_type) && at.field == k.f_type)
                || (p == k.array_type && at.field == k.f_element)
                || (p == k.tuple_type && parent.node.named_child(0) == Some(at.node));
            if !wrapped {
                break;
            }
            level += 1;
            at = *parent;
        }
        let owner = up(stack, level + 1)?;
        (owner.kind == k.impl_item && at.field == k.f_type).then_some(owner.node)
    }

    fn field_identifier(&self, cx: &mut Ctx<'_>, stack: &[Frame<'_>]) {
        let k = &self.0;
        let me = stack[stack.len() - 1];
        if let Some(parent) = up(stack, 1)
            && parent.kind == k.field_declaration
            && me.field == k.f_name
        {
            cx.def(me.node, parent.node, DefKind::Field, None);
            return;
        }
        // Member calls and the catch-all: no locals predicate.
        cx.reference(me.node);
    }
}
