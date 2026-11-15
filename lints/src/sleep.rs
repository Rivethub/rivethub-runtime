use rustc_hir::Expr;
use rustc_hir::ExprKind;
use rustc_lint::{LateContext, LateLintPass};

use crate::util;

dylint_linting::impl_late_lint!(
    pub(crate) SLEEP,
    Warn,
    "tokio::time::sleep in test code (tests await the event with a timeout; a genuine timing test carries an expect)",
    Sleep::default()
);

#[derive(Default)]
pub(crate) struct Sleep;

fn test_only_sleep(path: &str) -> bool {
    path == "tokio::time::sleep"
        || path.ends_with("::tokio::time::sleep")
        || path == "tokio::time::sleep_until"
        || path.ends_with("::tokio::time::sleep_until")
}

impl<'tcx> LateLintPass<'tcx> for Sleep {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        let def_id = match expr.kind {
            ExprKind::Call(callee, _) => cx
                .typeck_results()
                .type_dependent_def_id(callee.hir_id)
                .or_else(|| {
                    let rustc_hir::ExprKind::Path(rustc_hir::QPath::Resolved(_, path)) = callee.kind
                    else {
                        return None;
                    };
                    path.res.opt_def_id()
                }),
            ExprKind::MethodCall(..) => cx.typeck_results().type_dependent_def_id(expr.hir_id),
            _ => None,
        };
        let Some(def_id) = def_id else {
            return;
        };
        let path = cx.tcx.def_path_str(def_id);
        if !test_only_sleep(&path) {
            return;
        }
        let shown = if path.ends_with("::tokio::time::sleep_until") {
            "tokio::time::sleep_until"
        } else if path.ends_with("::tokio::time::sleep") {
            "tokio::time::sleep"
        } else {
            path.as_str()
        };
        if !util::in_test_code(cx, expr.hir_id, expr.span) {
            return;
        }
        util::lint(
            cx,
            SLEEP,
            expr.hir_id,
            expr.span,
            format!(
                "{shown} sleeps a test: wait for the event with a timeout, or carry #[cfg_attr(dylint_lib = \"rr_lints\", expect(sleep, reason = \"…\"))] on the statement"
            ),
        );
    }
}
