use std::cell::RefCell;
use std::collections::HashMap;

use rustc_hir::{HirId, ItemKind, def::DefKind};
use rustc_lint::{LateContext, LateLintPass};
use rustc_span::Span;

use crate::util;

dylint_linting::impl_late_lint!(
    pub(crate) DUPTYPE,
    Warn,
    "the same enum defined in several modules (one definition, imported where needed)",
    Duptype::default()
);

#[derive(Default)]
pub(crate) struct Duptype {
    local: RefCell<Vec<(String, Vec<String>, Span, HirId)>>,
    seen: RefCell<HashMap<(Vec<String>, String), Vec<String>>>,
}

impl<'tcx> LateLintPass<'tcx> for Duptype {
    fn check_item(&mut self, cx: &LateContext<'tcx>, item: &'tcx rustc_hir::Item<'tcx>) {
        let ItemKind::Enum(ident, _, enum_def) = &item.kind else {
            return;
        };
        if util::in_test_code(cx, item.hir_id(), item.span)
            || !util::has_pub_keyword(cx, item.vis_span)
        {
            return;
        }
        let variants: Vec<String> = enum_def
            .variants
            .iter()
            .map(|variant| variant.ident.name.to_string())
            .collect();
        if variants.len() < 3 {
            return;
        }
        let mut key = variants;
        key.sort();
        self.local.borrow_mut().push((
            ident.name.to_string(),
            key.clone(),
            item.span,
            item.hir_id(),
        ));
        self.seen
            .borrow_mut()
            .entry((key, util::final_name_word(&ident.name.to_string())))
            .or_default()
            .push(ident.name.to_string());
    }

    fn check_crate_post(&mut self, cx: &LateContext<'tcx>) {
        self.collect_external(cx);
        let groups = self.seen.borrow();
        for (name, variants, span, hir_id) in self.local.borrow().iter() {
            let key = (variants.clone(), util::final_name_word(name));
            let Some(group) = groups.get(&key) else {
                continue;
            };
            if group.len() < 2 {
                continue;
            }
            util::lint(
                cx,
                DUPTYPE,
                *hir_id,
                *span,
                format!(
                    "same enum defined {} times ({}) — one definition, imported where needed",
                    group.len(),
                    group.join(", ")
                ),
            );
        }
        drop(groups);
        self.local.borrow_mut().clear();
        self.seen.borrow_mut().clear();
    }
}

impl Duptype {
    fn collect_external(&self, cx: &LateContext<'_>) {
        for cnum in cx.tcx.used_crates(()) {
            let root = cnum.as_def_id();
            self.walk(cx, root, 0);
        }
    }

    fn walk(&self, cx: &LateContext<'_>, module: rustc_span::def_id::DefId, depth: u8) {
        if depth > 2 {
            return;
        }
        for child in cx.tcx.module_children(module) {
            if !child.vis.is_public() {
                continue;
            }            match child.res {
                rustc_hir::def::Res::Def(DefKind::Enum, did) => {
                    let name = cx.tcx.item_name(did);
                    let mut variants: Vec<String> = cx
                        .tcx
                        .adt_def(did)
                        .variants()
                        .iter()
                        .map(|variant| variant.name.to_string())
                        .collect();
                    if variants.len() < 3 {
                        continue;
                    }
                    variants.sort();
                    self.seen
                        .borrow_mut()
                        .entry((variants, util::final_name_word(&name.to_string())))
                        .or_default()
                        .push(name.to_string());
                }
                rustc_hir::def::Res::Def(DefKind::Mod, did) => self.walk(cx, did, depth + 1),
                _ => {}
            }        }
    }
}
