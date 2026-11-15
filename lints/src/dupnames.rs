use std::cell::RefCell;
use std::collections::HashMap;

use rustc_ast::LitKind;
use rustc_hir::{ExprKind, HirId, PatExprKind, PatKind, QPath, def::DefKind};
use rustc_lint::{LateContext, LateLintPass};
use rustc_span::Span;

use crate::util;

dylint_linting::impl_late_lint!(
    pub(crate) DUPNAMES,
    Warn,
    "match table mapping each variant to its string (derive strum Display/EnumString with serde's rename rule)",
    Dupnames::default()
);

#[derive(Default)]
pub(crate) struct Dupnames {
    arms: RefCell<HashMap<String, Vec<(Span, HirId)>>>,
}

impl<'tcx> LateLintPass<'tcx> for Dupnames {
    fn check_arm(&mut self, cx: &LateContext<'tcx>, arm: &'tcx rustc_hir::Arm<'tcx>) {
        let path = match arm.pat.kind {
            PatKind::Expr(pat_expr) => match pat_expr.kind {
                PatExprKind::Path(QPath::Resolved(_, path)) => Some(path),
                _ => None,
            },
            _ => None,
        };
        let Some(path) = path else {
            return;
        };
        if !matches!(path.res, rustc_hir::def::Res::Def(DefKind::Ctor(..), _)) {
            return;
        }
        let ExprKind::Lit(literal) = arm.body.kind else {
            return;
        };
        let LitKind::Str(..) = literal.node else {
            return;
        };
        if util::in_test_code(cx, arm.hir_id, arm.span) {
            return;
        }
        let Some(file) = util::file_path(cx, arm.span) else {
            return;
        };
        self.arms
            .borrow_mut()
            .entry(file)
            .or_default()
            .push((arm.span, arm.hir_id));
    }

    fn check_crate_post(&mut self, cx: &LateContext<'tcx>) {
        for arms in self.arms.borrow().values() {
            if arms.len() < 3 {
                continue;
            }
            for (span, hir_id) in arms {
                util::lint(
                    cx,
                    DUPNAMES,
                    *hir_id,
                    *span,
                    "match table mapping variants to their strings (one naming rule on the enum: strum Display/EnumString with serde's rename rule)"
                        .to_owned(),
                );
            }
        }
        self.arms.borrow_mut().clear();
    }
}
