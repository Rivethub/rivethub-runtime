use rustc_hir::Node;
use rustc_lint::{LateContext, LateLintPass};
use rustc_span::{ExpnId, ExpnKind, MacroKind, Span, def_id::DefId};

use crate::util;

dylint_linting::impl_late_lint!(
    pub(crate) UNTYPED,
    Warn,
    "raw serde value or json! outside its sanctioned holders (a typed struct, wire::JsonObject, or config::FreeForm)",
    Untyped::default()
);

#[derive(Default)]
pub(crate) struct Untyped;

const JSON_MACRO: &str = "serde_json::json";
const JSON_MACRO_TAIL: &str = "::serde_json::json";

const BANNED: [(&str, &str, bool); 6] = [
    ("serde_json", "Value", true),
    ("serde_json", "Map", true),
    ("serde_yaml", "Value", false),
    ("serde_yaml", "Mapping", false),
    ("serde_yaml", "Number", false),
    ("serde_yaml", "Sequence", false),
];

fn is_json_macro(cx: &LateContext<'_>, def_id: DefId) -> bool {
    let path = cx.tcx.def_path_str(def_id);
    path == JSON_MACRO || path.ends_with(JSON_MACRO_TAIL)
}

fn json_macro_expn(cx: &LateContext<'_>, span: Span) -> Option<ExpnId> {
    if span.ctxt().is_root() {
        return None;
    }
    let mut expn = span.ctxt().outer_expn();
    while expn != ExpnId::root() {
        let data = expn.expn_data();
        if let ExpnKind::Macro(MacroKind::Bang, name) = &data.kind {
            let invoked = name.as_str();
            if invoked == JSON_MACRO
                || (invoked == "json"
                    && data
                        .macro_def_id
                        .is_some_and(|def_id| is_json_macro(cx, def_id)))
            {
                return Some(expn);
            }
        }
        expn = data.parent;
    }
    None
}

impl<'tcx> LateLintPass<'tcx> for Untyped {
    fn check_ty(
        &mut self,
        cx: &LateContext<'tcx>,
        ty: &'tcx rustc_hir::Ty<'tcx, rustc_hir::AmbigArg>,
    ) {
        let Some(segments) = util::segments(ty) else {
            return;
        };
        if segments.len() < 2 {
            return;
        }
        let head = segments[segments.len() - 2].ident.name.as_str();
        let last = segments[segments.len() - 1].ident.name.as_str();
        let banned = if last == "IgnoredAny" {
            Some(false)
        } else {
            BANNED
                .iter()
                .find(|(krate, name, _)| *krate == head && *name == last)
                .map(|(_, _, is_json)| *is_json)
        };
        let Some(is_json) = banned else {
            return;
        };
        let pkg = util::package_or_crate(cx, ty.span);
        if pkg == "testkit" {
            return;
        }
        if pkg.starts_with("wire") && util::in_test_code(cx, ty.hir_id, ty.span) {
            return;
        }
        if pkg == "values"
            && is_json
            && util::file_path(cx, ty.span).is_some_and(|path| path.ends_with("src/json.rs"))
        {
            return;
        }
        if pkg == "config"
            && !is_json
            && util::file_path(cx, ty.span).is_some_and(|path| {
                ["src/load.rs", "src/resolve.rs", "src/free_form.rs"]
                    .iter()
                    .any(|tail| path.ends_with(tail))
            })
        {
            return;
        }
        util::lint(
            cx,
            UNTYPED,
            ty.hir_id,
            ty.span,
            format!(
                "{head}::{last} outside its sanctioned holder (a typed struct, wire::JsonObject, or config::FreeForm)"
            ),
        );
    }

    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx rustc_hir::Expr<'tcx>) {
        let Some(expn) = json_macro_expn(cx, expr.span) else {
            return;
        };
        if let Node::Expr(parent) = cx.tcx.parent_hir_node(expr.hir_id)
            && json_macro_expn(cx, parent.span) == Some(expn)
        {
            return;
        }
        let data = expn.expn_data();
        let pkg = util::package_or_crate(cx, data.call_site);
        if pkg == "testkit" {
            return;
        }
        if pkg.starts_with("wire") && util::in_test_code(cx, expr.hir_id, data.call_site) {
            return;
        }
        if pkg == "values"
            && util::file_path(cx, data.call_site).is_some_and(|path| path.ends_with("src/json.rs"))
        {
            return;
        }
        util::lint(
            cx,
            UNTYPED,
            expr.hir_id,
            data.call_site,
            format!(
                "{JSON_MACRO} builds untyped JSON — a typed serde struct or enum (or the crate's own protocol types) instead of json!"
            ),
        );
    }
}
