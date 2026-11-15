use rustc_hir::ExprKind;
use rustc_lint::{LateContext, LateLintPass};

use crate::util;

dylint_linting::impl_late_lint!(
    pub(crate) DISPATCH,
    Warn,
    "string dispatch on a protocol tag (use a serde enum and match on the variant)",
    Dispatch::default()
);

#[derive(Default)]
pub(crate) struct Dispatch;

impl<'tcx> LateLintPass<'tcx> for Dispatch {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx rustc_hir::Expr<'tcx>) {
        if util::in_test_code(cx, expr.hir_id, expr.span) {
            return;
        }
        if let ExprKind::Match(scrutinee, _, _) = expr.kind
            && let ExprKind::MethodCall(segment, _, args, _) = scrutinee.kind
            && segment.ident.name == util::symbol("as_str")
            && args.is_empty()
        {
            util::lint(
                cx,
                DISPATCH,
                expr.hir_id,
                expr.span,
                "match on `.as_str()` (deserialize the tag into a serde enum and match on the variant)"
                    .to_owned(),
            );
        }
    }
}
