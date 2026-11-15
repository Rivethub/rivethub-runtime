use rustc_hir::HirId;
use rustc_lint::{LateContext, LateLintPass};
use rustc_span::Span;

use crate::util;

dylint_linting::impl_late_lint!(
    pub(crate) JSMODEL,
    Warn,
    "JavaScript value model or semantics (Js* types, truthiness, typeof, [object Object], JSON.stringify)",
    Jsmodel::default()
);

#[derive(Default)]
pub(crate) struct Jsmodel;

const JS_TYPES: [&str; 5] = ["JsValue", "JsString", "JsObject", "JsNumber", "JsArray"];

fn banned_ident(name: &str) -> bool {
    JS_TYPES.contains(&name)
        || name.contains("truthy")
        || name.starts_with("typeof")
        || name.contains("NumberOr")
        || name.contains("OrNumber")
}

fn check_ident(cx: &LateContext<'_>, hir_id: HirId, span: Span, name: &str) {
    if banned_ident(name) {
        util::lint(
            cx,
            JSMODEL,
            hir_id,
            span,
            format!("{name}: JavaScript value model (plain Rust types instead)"),
        );
    }
}

impl<'tcx> LateLintPass<'tcx> for Jsmodel {
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
        let rustc_hir::ExprKind::Lit(literal) = expr.kind else {
            return;
        };
        let rustc_ast::LitKind::Str(text, _) = literal.node else {
            return;
        };
        if util::in_test_code(cx, expr.hir_id, expr.span) {
            return;
        }
        let text = text.as_str();
        if text.contains("[object Object]") || text.contains("JSON.stringify") {
            util::lint(
                cx,
                JSMODEL,
                expr.hir_id,
                expr.span,
                "JavaScript semantics in a string (build such bodies in Rust where the test needs them)"
                    .to_owned(),
            );
        }
    }
}
