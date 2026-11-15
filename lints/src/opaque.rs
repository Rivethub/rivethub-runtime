use rustc_hir::{ItemKind, VariantData};
use rustc_lint::{LateContext, LateLintPass};

use crate::util;

dylint_linting::impl_late_lint!(
    pub(crate) OPAQUE,
    Warn,
    "empty struct that swallows a section (type its fields, or hold free-form data in the sanctioned holder)",
    Opaque::default()
);

#[derive(Default)]
pub(crate) struct Opaque;

impl<'tcx> LateLintPass<'tcx> for Opaque {
    fn check_item(&mut self, cx: &LateContext<'tcx>, item: &'tcx rustc_hir::Item<'tcx>) {
        let ItemKind::Struct(_, _, data) = &item.kind else {
            return;
        };
        if util::in_test_code(cx, item.hir_id(), item.span) {
            return;
        }
        let VariantData::Struct { fields, .. } = data else {
            return;
        };
        if !fields.is_empty() {
            return;
        }
        util::lint(
            cx,
            OPAQUE,
            item.hir_id(),
            item.span,
            "empty struct accepting a section and throwing its contents away (type the section; arbitrary values go in wire::JsonObject or config::FreeForm)"
                .to_owned(),
        );
    }
}
