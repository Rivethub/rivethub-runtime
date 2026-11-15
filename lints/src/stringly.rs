use std::cell::RefCell;

use rustc_hir::{FieldDef, HirId, ItemKind};
use rustc_lint::{LateContext, LateLintPass};
use rustc_span::Span;

use crate::util;

dylint_linting::impl_late_lint!(
    pub(crate) STRINGLY,
    Warn,
    "a String field whose values are a closed set that already has an enum (use the enum)",
    Stringly::default()
);

#[derive(Default)]
pub(crate) struct Stringly {
    enums: RefCell<Vec<String>>,
    fields: RefCell<Vec<(Span, HirId, String)>>,
}

const SUFFIXES: [&str; 9] = [
    "Id", "Kind", "Status", "Role", "Phase", "Mode", "State", "Source", "Activity",
];

const WORKSPACE_CRATES: [&str; 5] = ["values", "wire", "config", "kernel", "testkit"];

impl<'tcx> LateLintPass<'tcx> for Stringly {
    fn check_item(&mut self, cx: &LateContext<'tcx>, item: &'tcx rustc_hir::Item<'tcx>) {
        if util::in_test_code(cx, item.hir_id(), item.span) {
            return;
        }
        if let ItemKind::Enum(ident, _, _) = &item.kind
            && util::has_pub_keyword(cx, item.vis_span)
            && SUFFIXES.iter().any(|suffix| ident.name.as_str().ends_with(suffix))
        {
            let name = util::snake(ident.name);
            self.enums.borrow_mut().push(name.clone());
            if let Some(stem) = name.strip_suffix("_id") {
                self.enums.borrow_mut().push(stem.to_owned());
            }
        }
    }

    fn check_field_def(
        &mut self,
        cx: &LateContext<'tcx>,
        field: &'tcx FieldDef<'tcx>,
    ) {
        if util::in_test_code(cx, field.hir_id, field.span)
            || !util::string_like(field.ty)
        {
            return;
        }
        self.fields.borrow_mut().push((
            field.span,
            field.hir_id,
            field.ident.name.to_string(),
        ));
    }

    fn check_crate_post(&mut self, cx: &LateContext<'tcx>) {
        for (name, _did) in self.external_enums(cx) {
            let snake = util::snake(name);
            if SUFFIXES.iter().any(|suffix| name.as_str().ends_with(suffix)) {
                self.enums.borrow_mut().push(snake.clone());
                if let Some(stem) = snake.strip_suffix("_id") {
                    self.enums.borrow_mut().push(stem.to_owned());
                }
            }
        }
        let enums = self.enums.borrow();
        for (span, hir_id, field) in self.fields.borrow().iter() {
            if !enums.contains(field) {
                continue;
            }
            util::lint(
                cx,
                STRINGLY,
                *hir_id,
                *span,
                format!(
                    "field `{field}` is a String but an enum with that name exists (use the enum)"
                ),
            );
        }
        drop(enums);
        self.enums.borrow_mut().clear();
        self.fields.borrow_mut().clear();
    }
}

impl Stringly {
    fn external_enums(
        &self,
        cx: &LateContext<'_>,
    ) -> Vec<(rustc_span::Symbol, rustc_span::def_id::DefId)> {
        let mut out = Vec::new();
        for cnum in cx.tcx.used_crates(()) {
            let name = cx.tcx.crate_name(*cnum).to_string();
            if !WORKSPACE_CRATES.contains(&name.as_str()) {
                continue;
            }
            self.walk(cx, cnum.as_def_id(), 0, &mut out);
        }
        out
    }

    fn walk(
        &self,
        cx: &LateContext<'_>,
        module: rustc_span::def_id::DefId,
        depth: u8,
        out: &mut Vec<(rustc_span::Symbol, rustc_span::def_id::DefId)>,
    ) {
        if depth > 2 {
            return;
        }
        for child in cx.tcx.module_children(module) {
            if !child.vis.is_public() {
                continue;
            }
            match child.res {
                rustc_hir::def::Res::Def(rustc_hir::def::DefKind::Enum, did) => {
                    out.push((cx.tcx.item_name(did), did));
                }
                rustc_hir::def::Res::Def(rustc_hir::def::DefKind::Mod, did) => {
                    self.walk(cx, did, depth + 1, out);
                }
                _ => {}
            }
        }
    }
}
