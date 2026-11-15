use rustc_hir::ItemKind;
use rustc_lint::{LateContext, LateLintPass};

use crate::util;

dylint_linting::impl_late_lint!(
    pub(crate) TYPEMACRO,
    Warn,
    "macro_rules! stamping out types (write each type out with its derives)",
    Typemacro::default()
);

#[derive(Default)]
pub(crate) struct Typemacro;

impl<'tcx> LateLintPass<'tcx> for Typemacro {
    fn check_item(&mut self, cx: &LateContext<'tcx>, item: &'tcx rustc_hir::Item<'tcx>) {
        let ItemKind::Macro(_, def, _) = &item.kind else {
            return;
        };
        if util::in_test_code(cx, item.hir_id(), item.span) {
            return;
        }
        if def.macro_rules {
            util::lint(
                cx,
                TYPEMACRO,
                item.hir_id(),
                item.span,
                "macro_rules! in production code (write the types and impls out)"
                    .to_owned(),
            );
        }
    }
}
