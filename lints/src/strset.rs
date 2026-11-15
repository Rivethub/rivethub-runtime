use rustc_hir::ItemKind;
use rustc_lint::{LateContext, LateLintPass};

use crate::util;

dylint_linting::impl_late_lint!(
    pub(crate) STRSET,
    Warn,
    "table of protocol strings (use an enum with methods; unknown keys come from serde)",
    Strset::default()
);

#[derive(Default)]
pub(crate) struct Strset;

impl<'tcx> LateLintPass<'tcx> for Strset {
    fn check_item(&mut self, cx: &LateContext<'tcx>, item: &'tcx rustc_hir::Item<'tcx>) {
        if util::in_test_code(cx, item.hir_id(), item.span) {
            return;
        }
        let ty = match &item.kind {
            ItemKind::Const(_, _, ty, _) => Some(ty),
            ItemKind::Static(_, _, ty, _) => Some(ty),
            _ => None,
        };
        let Some(ty) = ty else {
            return;
        };
        if util::str_table(ty) {
            let name = item.kind.ident().map(|ident| ident.name.to_string());
            util::lint(
                cx,
                STRSET,
                item.hir_id(),
                item.span,
                format!(
                    "{}: table of protocol strings (one enum with methods for each property; messages list values from the enum)",
                    name.unwrap_or_default()
                ),
            );
        }
    }
}
