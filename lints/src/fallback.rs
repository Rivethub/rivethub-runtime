use rustc_ast::LitKind;
use rustc_hir::{Expr, ExprKind, LetStmt, MatchSource, Pat, PatKind, UnOp};
use rustc_lint::{LateContext, LateLintPass};
use rustc_middle::ty;
use rustc_span::sym;

use crate::util;

dylint_linting::impl_late_lint!(
    pub(crate) FALLBACK,
    Warn,
    "an error or absence becomes a silent default",
    Fallback
);

pub(crate) struct Fallback;

fn result(cx: &LateContext<'_>, expression: &Expr<'_>) -> bool {
    matches!(cx.typeck_results().expr_ty_adjusted(expression).peel_refs().kind(), ty::Adt(adt, _) if cx.tcx.is_diagnostic_item(sym::Result, adt.did()))
}

fn option(cx: &LateContext<'_>, expression: &Expr<'_>) -> bool {
    matches!(cx.typeck_results().expr_ty_adjusted(expression).peel_refs().kind(), ty::Adt(adt, _) if cx.tcx.is_diagnostic_item(sym::Option, adt.did()))
}

fn sentinel(expression: &Expr<'_>) -> bool {
    match expression.kind {
        ExprKind::Lit(lit) => match lit.node {
            LitKind::Str(value, _) => matches!(value.as_str(), "" | "unknown" | "default"),
            LitKind::Int(value, _) => value.get() == 0,
            _ => false,
        },
        ExprKind::Unary(UnOp::Neg, inner) => {
            matches!(inner.kind, ExprKind::Lit(lit) if matches!(lit.node, LitKind::Int(value, _) if value.get() == 1))
        }
        ExprKind::Block(block, _) => block.expr.is_some_and(sentinel),
        _ => false,
    }
}

fn variant(cx: &LateContext<'_>, pattern: &Pat<'_>, name: &str) -> bool {
    match pattern.kind {
        PatKind::TupleStruct(ref path, _, _) => {
            let Some(did) = cx.qpath_res(path, pattern.hir_id).opt_def_id() else {
                return false;
            };
            let variant = match cx.tcx.def_kind(did) {
                rustc_hir::def::DefKind::Ctor(..) => cx.tcx.parent(did),
                _ => did,
            };
            let expected = if name == "Err" {
                cx.tcx.lang_items().result_err_variant()
            } else {
                cx.tcx.lang_items().result_ok_variant()
            };
            Some(variant) == expected
        }
        PatKind::Or(patterns) => patterns.iter().any(|pattern| variant(cx, pattern, name)),
        _ => false,
    }
}

fn discards_error(pattern: &Pat<'_>) -> bool {
    match pattern.kind {
        PatKind::TupleStruct(_, fields, _) => fields
            .iter()
            .all(|field| matches!(field.kind, PatKind::Wild)),
        PatKind::Or(patterns) => patterns.iter().any(discards_error),
        _ => false,
    }
}

fn error_value(cx: &LateContext<'_>, expression: &Expr<'_>) -> bool {
    if let ExprKind::Call(callee, _) = expression.kind
        && let ExprKind::Path(ref path) = callee.kind
        && let Some(did) = cx.qpath_res(path, callee.hir_id).opt_def_id()
    {
        let variant = match cx.tcx.def_kind(did) {
            rustc_hir::def::DefKind::Ctor(..) => cx.tcx.parent(did),
            _ => did,
        };
        return Some(variant) == cx.tcx.lang_items().result_err_variant();
    }
    false
}

fn produces_default(cx: &LateContext<'_>, expression: &Expr<'_>) -> bool {
    match expression.kind {
        ExprKind::Ret(Some(value)) => !error_value(cx, value),
        ExprKind::Ret(None) | ExprKind::Continue(_) => true,
        ExprKind::Block(block, _) => block_default(cx, block),
        ExprKind::If(_, then, otherwise) => {
            produces_default(cx, then) || otherwise.is_none_or(|expr| produces_default(cx, expr))
        }
        _ => !cx.typeck_results().expr_ty(expression).is_never() && !error_value(cx, expression),
    }
}

fn block_default(cx: &LateContext<'_>, block: &rustc_hir::Block<'_>) -> bool {
    for statement in block.stmts {
        if let rustc_hir::StmtKind::Semi(expr) | rustc_hir::StmtKind::Expr(expr) = statement.kind
            && cx.typeck_results().expr_ty(expr).is_never()
        {
            return produces_default(cx, expr);
        }
    }
    block.expr.is_none_or(|expr| produces_default(cx, expr))
}

fn emit(cx: &LateContext<'_>, expression: &Expr<'_>, text: &str) {
    crate::fallback_waiver::expanded_expectation(cx, expression.hir_id, expression.span);
    util::lint(
        cx,
        FALLBACK,
        expression.hir_id,
        expression.span,
        format!("FALLBACK {text}"),
    );
}

fn conversion(
    cx: &LateContext<'_>,
    expression: &Expr<'_>,
    name: &str,
    receiver: &Expr<'_>,
    arguments: &[Expr<'_>],
) {
    let converts = name == "ok" || name.starts_with("unwrap_or") || name.starts_with("map_or");
    if result(cx, receiver) && converts {
        if name == "unwrap_or_else"
            && let Some(argument) = arguments.first()
            && let ExprKind::Closure(closure) = argument.kind
        {
            let body = cx.tcx.hir_body(closure.body);
            if cx.typeck_results().expr_ty(body.value).is_never() {
                return;
            }
        }
        emit(cx, expression, name);
    } else if option(cx, receiver)
        && name.starts_with("unwrap_or")
        && arguments.first().is_some_and(|argument| {
            sentinel(argument)
                || if let ExprKind::Closure(closure) = argument.kind {
                    sentinel(cx.tcx.hir_body(closure.body).value)
                } else {
                    false
                }
        })
    {
        emit(cx, expression, name);
    }
}

impl<'tcx> LateLintPass<'tcx> for Fallback {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expression: &'tcx Expr<'tcx>) {
        if util::in_test_code(cx, expression.hir_id, expression.span) {
            return;
        }
        match expression.kind {
            ExprKind::MethodCall(segment, receiver, arguments, _) => {
                let name = segment.ident.name.as_str();
                conversion(cx, expression, name, receiver, arguments);
            }
            ExprKind::Match(scrutinee, arms, MatchSource::Normal) if result(cx, scrutinee) => {
                for arm in arms {
                    if arm.guard.is_none()
                        && variant(cx, arm.pat, "Err")
                        && discards_error(arm.pat)
                        && produces_default(cx, arm.body)
                    {
                        emit(cx, arm.body, "Err(..) => value");
                    }
                }
            }
            ExprKind::Call(callee, arguments) => {
                if let ExprKind::Path(ref path) = callee.kind
                    && let Some(did) = cx.qpath_res(path, callee.hir_id).opt_def_id()
                    && cx.tcx.def_kind(did) == rustc_hir::def::DefKind::AssocFn
                    && let Some((receiver, arguments)) = arguments.split_first()
                {
                    let symbol = cx.tcx.item_name(did);
                    conversion(cx, expression, symbol.as_str(), receiver, arguments);
                }
            }
            _ => {}
        }
    }

    fn check_local(&mut self, cx: &LateContext<'tcx>, local: &'tcx LetStmt<'tcx>) {
        if util::in_test_code(cx, local.hir_id, local.span) {
            return;
        }
        if let Some(init) = local.init
            && result(cx, init)
            && variant(cx, local.pat, "Ok")
            && let Some(block) = local.els
        {
            if block_default(cx, block) {
                crate::fallback_waiver::expanded_expectation(cx, local.hir_id, local.span);
                util::lint(
                    cx,
                    FALLBACK,
                    local.hir_id,
                    local.span,
                    "FALLBACK let Ok(..) = .. else".to_owned(),
                );
            }
        }
    }
}
