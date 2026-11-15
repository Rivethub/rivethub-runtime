use rustc_ast::Attribute;
use rustc_ast::Item;
use rustc_errors::DiagDecorator;
use rustc_hir::Expr;
use rustc_hir::ExprKind;
use rustc_lint::{EarlyContext, EarlyLintPass, LateContext, LateLintPass, LintContext};
use rustc_span::Span;
use rustc_span::sym;

use crate::util;

dylint_linting::declare_lint!(
    pub(crate) RUNTIME,
    Warn,
    "a library builds a tokio runtime (only the rivethub binary does)"
);

dylint_linting::impl_lint_pass!(RuntimeEarly => [RUNTIME]);

dylint_linting::impl_lint_pass!(RuntimeLate => [RUNTIME]);

#[derive(Default)]
pub(crate) struct RuntimeEarly {
    cfg_test: Vec<bool>,
}

#[derive(Default)]
pub(crate) struct RuntimeLate;

const CONSTRUCTORS: [&str; 3] = [
    "tokio::runtime::Builder::new_multi_thread",
    "tokio::runtime::Builder::new_current_thread",
    "tokio::runtime::Runtime::new",
];

impl EarlyLintPass for RuntimeEarly {
    fn check_item(&mut self, _cx: &EarlyContext<'_>, item: &Item) {
        self.cfg_test.push(cfg_test(&item.attrs));
    }

    fn check_item_post(&mut self, _cx: &EarlyContext<'_>, _item: &Item) {
        self.cfg_test.pop();
    }

    fn check_attribute(&mut self, cx: &EarlyContext<'_>, attr: &Attribute) {
        if !is_tokio_main(attr) {
            return;
        }
        if self.in_test_code(cx, attr.span) || util::in_rivethub_main_sess(cx.sess(), attr.span) {
            return;
        }
        cx.emit_span_lint(
            RUNTIME,
            attr.span,
            DiagDecorator(|diag| {
                diag.primary_message(
                    "`#[tokio::main]` builds a tokio runtime in a library (libraries expose async fns; only the rivethub binary builds one)"
                        .to_owned(),
                );
            }),
        );
    }
}

impl RuntimeEarly {
    fn in_test_code(&self, cx: &EarlyContext<'_>, span: Span) -> bool {
        self.cfg_test.contains(&true) || util::in_test_file_sess(cx.sess(), span)
    }
}

impl<'tcx> LateLintPass<'tcx> for RuntimeLate {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        let def_id = match expr.kind {
            ExprKind::Call(callee, _) => {
                cx.typeck_results().type_dependent_def_id(callee.hir_id)
            }
            ExprKind::MethodCall(..) => {
                cx.typeck_results().type_dependent_def_id(expr.hir_id)
            }
            _ => None,
        };
        let Some(def_id) = def_id else {
            return;
        };
        let path = cx.tcx.def_path_str(def_id);
        if !CONSTRUCTORS.contains(&path.as_str()) {
            return;
        }
        if util::in_test_code(cx, expr.hir_id, expr.span) || util::in_rivethub_main(cx, expr.span) {
            return;
        }
        util::lint(
            cx,
            RUNTIME,
            expr.hir_id,
            expr.span,
            format!(
                "{path} builds a tokio runtime in a library (libraries expose async fns; only the rivethub binary builds one)"
            ),
        );
    }
}

fn is_tokio_main(attr: &Attribute) -> bool {
    let path = attr.path();
    path.len() == 2 && path[0] == util::symbol("tokio") && path[1] == sym::main
}

fn cfg_test(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.has_name(sym::cfg)
            && attr.meta_item_list().is_some_and(|list| {
                list.iter()
                    .any(|item| item.ident().map(|ident| ident.name) == Some(sym::test))
            })
    })
}
