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
            impl_item = "impl_item",
            trait_item = "trait_item",
            declaration_list = "declaration_list",
            function_item = "function_item",
            function_signature_item = "function_signature_item",
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
        }
        tokens {}
        fields {
            f_name = "name",
            f_parameters = "parameters",
            f_body = "body",
            f_trait = "trait",
            f_type = "type",
            f_function = "function",
            f_macro = "macro",
            f_pattern = "pattern",
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
            cx.scope(stack.len() - 1, true);
        } else if kind == k.function_item || kind == k.closure_expression {
            cx.scope(stack.len() - 1, false);
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
                } else if p == k.type_item {
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
            // (impl_item trait: (_)? type: [T (generic_type type: T)
            //   (scoped_type_identifier name: T)]) @reference.lectern.impl[-trait]
            if let Some(implementation) = self.impl_of(stack, k.f_type) {
                let marker = if implementation.child_by_field_id(k.f_trait).is_some() {
                    Marker::ImplTrait
                } else {
                    Marker::Impl
                };
                cx.marker(me.node, implementation, marker);
                return;
            }
        }
        // The implemented trait, struct expressions and the catch-all: no
        // locals predicate.
        cx.reference(me.node);
    }

    /// The `impl_item` whose `field` (type or trait) is the name being
    /// visited, directly or as `generic_type.type` or
    /// `scoped_type_identifier.name`.
    fn impl_of<'t>(&self, stack: &[Frame<'t>], field: u16) -> Option<Node<'t>> {
        let k = &self.0;
        let me = stack[stack.len() - 1];
        let parent = up(stack, 1)?;
        let (child, owner) = if (parent.kind == k.generic_type && me.field == k.f_type)
            || (parent.kind == k.scoped_type_identifier && me.field == k.f_name)
        {
            (parent, up(stack, 2)?)
        } else {
            (&me, parent)
        };
        (owner.kind == k.impl_item && child.field == field).then_some(owner.node)
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
