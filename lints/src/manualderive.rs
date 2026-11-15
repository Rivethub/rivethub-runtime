use rustc_hir::ItemKind;
use rustc_lint::{LateContext, LateLintPass};

use crate::util;

dylint_linting::impl_late_lint!(
    pub(crate) MANUALDERIVE,
    Warn,
    "hand-written impl of a derivable trait (use #[derive])",
    Manualderive::default()
);

#[derive(Default)]
pub(crate) struct Manualderive;

const DERIVABLE: [&str; 9] = [
    "PartialEq", "Eq", "Clone", "Copy", "Debug", "Default", "Hash", "PartialOrd", "Ord",
];

impl<'tcx> LateLintPass<'tcx> for Manualderive {
    fn check_item(&mut self, cx: &LateContext<'tcx>, item: &'tcx rustc_hir::Item<'tcx>) {
        let ItemKind::Impl(impl_) = &item.kind else {
            return;
        };
        if util::in_test_code(cx, item.hir_id(), item.span) {
            return;
        }
        let Some(trait_ref) = impl_.of_trait else {
            return;
        };
        let Some(segment) = trait_ref.trait_ref.path.segments.last() else {
            return;
        };
        let name = segment.ident.name.as_str();
        if DERIVABLE.contains(&name) {
            util::lint(
                cx,
                MANUALDERIVE,
                item.hir_id(),
                item.span,
                format!(
                    "hand-written impl {name} for (what #[derive({name})] writes)"
                ),
            );
        }
    }
}
