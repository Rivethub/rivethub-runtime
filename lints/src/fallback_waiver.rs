use rustc_hir::{CRATE_HIR_ID, Expr, Item};
use rustc_lint::{LateContext, LateLintPass};
use rustc_span::Span;
use std::collections::HashSet;

use crate::{fallback_policy, util};

dylint_linting::impl_late_lint!(
    pub(crate) FALLBACK_WAIVER,
    Warn,
    "FALLBACK expectations belong on a statement",
    FallbackWaiver::default()
);

#[derive(Default)]
pub(crate) struct FallbackWaiver {
    files: HashSet<String>,
}

impl FallbackWaiver {
    fn check(&mut self, cx: &LateContext<'_>, span: Span) {
        if span.in_external_macro(cx.tcx.sess.source_map()) {
            return;
        }
        let Some(file) = util::file_path(cx, span) else {
            return;
        };
        if !self.files.insert(file.clone()) {
            return;
        }
        let source = match std::fs::read_to_string(&file) {
            Ok(source) => source,
            Err(error) => {
                util::lint(
                    cx,
                    FALLBACK_WAIVER,
                    CRATE_HIR_ID,
                    span,
                    format!("cannot validate FALLBACK expectations in {file}: {error}"),
                );
                return;
            }
        };
        match fallback_policy::violations(&source) {
            Ok(lines) => {
                for line in lines {
                    util::lint(
                        cx,
                        FALLBACK_WAIVER,
                        CRATE_HIR_ID,
                        span,
                        format!(
                            "{file}:{line}: FALLBACK expectation must be on a statement"
                        ),
                    );
                }
            }
            Err(error) => util::lint(
                cx,
                FALLBACK_WAIVER,
                CRATE_HIR_ID,
                span,
                format!("cannot validate FALLBACK expectations in {file}: {error}"),
            ),
        }
    }
}

impl<'tcx> LateLintPass<'tcx> for FallbackWaiver {
    fn check_item(&mut self, cx: &LateContext<'tcx>, item: &'tcx Item<'tcx>) {
        self.check(cx, item.span);
    }
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        self.check(cx, expr.span);
    }
}

pub(crate) fn expanded_expectation(cx: &LateContext<'_>, id: rustc_hir::HirId, span: Span) {
    if !span.from_expansion() {
        return;
    }
    let specification = cx
        .tcx
        .lint_level_spec_at_node(crate::fallback::FALLBACK, id);
    if !matches!(specification.level(), rustc_lint::Expect) {
        return;
    }
    let valid = match specification.src {
        rustc_middle::lint::LintLevelSource::Node {
            span: attribute, ..
        } => statement_expectation(cx, id, attribute),
        _ => false,
    };
    if !valid {
        util::lint(cx, FALLBACK_WAIVER, CRATE_HIR_ID, span.source_callsite(), "FALLBACK expectation must be on a statement".to_owned());
    }
}

fn statement_expectation(cx: &LateContext<'_>, id: rustc_hir::HirId, attribute: Span) -> bool {
    std::iter::once(id)
        .chain(cx.tcx.hir_parent_id_iter(id))
        .find(|id| {
            cx.tcx
                .hir_attrs(*id)
                .iter()
                .any(|attr| attr.span().contains(attribute))
        })
        .is_some_and(|id| match cx.tcx.hir_node(id) {
            rustc_hir::Node::LetStmt(_) | rustc_hir::Node::Stmt(_) => true,
            rustc_hir::Node::Expr(_) => {
                cx.tcx.hir_parent_id_iter(id).next().is_some_and(|parent| {
                    matches!(
                        cx.tcx.hir_node(parent),
                        rustc_hir::Node::Stmt(_) | rustc_hir::Node::Block(_)
                    )
                })
            }
            _ => false,
        })
}
