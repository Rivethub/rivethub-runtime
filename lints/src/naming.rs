use rustc_hir::ExprKind;
use rustc_lint::{LateContext, LateLintPass};

use crate::util;

dylint_linting::impl_late_lint!(
    pub(crate) NAMING,
    Warn,
    "den/room identifier inherited from the TypeScript (descriptive names; wire strings are serde renames)",
    Naming::default()
);

#[derive(Default)]
pub(crate) struct Naming;

fn check_ident(cx: &LateContext<'_>, hir_id: rustc_hir::HirId, span: rustc_span::Span, name: &str) {
    if util::banned_name(name) {
        util::lint(
            cx,
            NAMING,
            hir_id,
            span,
            format!(
                "`{name}` carries the old den/room name (gateway, session, view; wire strings are serde renames)"
            ),
        );
    }
}

impl<'tcx> LateLintPass<'tcx> for Naming {
    fn check_item(&mut self, cx: &LateContext<'tcx>, item: &'tcx rustc_hir::Item<'tcx>) {
        if util::in_test_code(cx, item.hir_id(), item.span) {
            return;
        }
        if let Some(ident) = item.kind.ident() {
            check_ident(cx, item.hir_id(), item.span, ident.name.as_str());
        }
    }

    fn check_field_def(
        &mut self,
        cx: &LateContext<'tcx>,
        field: &'tcx rustc_hir::FieldDef<'tcx>,
    ) {
        if util::in_test_code(cx, field.hir_id, field.span) {
            return;
        }
        check_ident(cx, field.hir_id, field.span, field.ident.name.as_str());
    }

    fn check_pat(&mut self, cx: &LateContext<'tcx>, pat: &'tcx rustc_hir::Pat<'tcx>) {
        let rustc_hir::PatKind::Binding(_, _, ident, _) = pat.kind else {
            return;
        };
        if util::in_test_code(cx, pat.hir_id, pat.span) {
            return;
        }
        check_ident(cx, pat.hir_id, pat.span, ident.name.as_str());
    }

    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx rustc_hir::Expr<'tcx>) {
        let ExprKind::MethodCall(segment, _, _, _) = expr.kind else {
            return;
        };
        if util::in_test_code(cx, expr.hir_id, expr.span) {
            return;
        }
        check_ident(cx, expr.hir_id, expr.span, segment.ident.name.as_str());
    }
}
