use std::sync::OnceLock;

use regex::Regex;
use rustc_lint::{LateContext, LateLintPass};

use crate::util;

dylint_linting::impl_late_lint!(
    pub(crate) COUNTTYPE,
    Warn,
    "JavaScript-number habits in integer fields (u16 ports; u32/u64/usize counts; unsigned settings)",
    Counttype::default()
);

#[derive(Default)]
pub(crate) struct Counttype;

fn float_settings() -> &'static Regex {
    static FLOAT_SETTINGS: OnceLock<Regex> = OnceLock::new();
    FLOAT_SETTINGS.get_or_init(|| {
        Regex::new(
            "^(?:\\w+_ms|\\w*timeout\\w*|\\w+_tokens|\\w+_dims|\\w*minutes|\\w*hours|start|end|\
             port|\\w+_count|\\w+_window|top_k|seed|num_ctx|\\w*max_\\w+|\\w*min_tokens)$",
        )
        .expect("valid float settings regex")
    })
}

fn field_kind(field: &rustc_hir::FieldDef<'_>) -> Option<String> {
    let symbol = util::option_inner(field.ty)
        .and_then(util::type_name)
        .or_else(|| util::type_name(field.ty))?;
    Some(symbol.as_str().to_owned())
}

impl<'tcx> LateLintPass<'tcx> for Counttype {
    fn check_field_def(
        &mut self,
        cx: &LateContext<'tcx>,
        field: &'tcx rustc_hir::FieldDef<'tcx>,
    ) {
        if util::in_test_code(cx, field.hir_id, field.span) || !util::has_pub_keyword(cx, field.vis_span)
        {
            return;
        }
        let name = field.ident.name.as_str().to_owned();
        let Some(kind) = field_kind(field) else {
            return;
        };
        if (name == "port" || name.ends_with("_port"))
            && ["i64", "i32", "u64", "u32", "usize"].contains(&kind.as_str())
        {
            util::lint(
                cx,
                COUNTTYPE,
                field.hir_id,
                field.span,
                "signed or oversized integer for a port (u16)".to_owned(),
            );
            return;
        }
        if [
            "count",
            "limit",
            "offset",
            "total",
            "inserted",
            "skipped",
            "index",
            "attempt",
            "attempts",
            "moved",
            "dropped",
            "len",
        ]
        .contains(&name.as_str())
            || name.ends_with("_count")
            || name.ends_with("_tokens")
        {
            if ["i64", "i32"].contains(&kind.as_str()) {
                util::lint(
                    cx,
                    COUNTTYPE,
                    field.hir_id,
                    field.span,
                    "signed integer for a count (u32/u64/usize)".to_owned(),
                );
            }
            return;
        }
        if name == "pid" && ["i64", "i32"].contains(&kind.as_str()) {
            util::lint(
                cx,
                COUNTTYPE,
                field.hir_id,
                field.span,
                "signed pid (use u32)".to_owned(),
            );
            return;
        }
        if util::crate_name(cx) == "config"
            && ["f32", "f64"].contains(&kind.as_str())
            && float_settings().is_match(&name)
        {
            util::lint(
                cx,
                COUNTTYPE,
                field.hir_id,
                field.span,
                "float for a duration, count or size setting (use u64/u32/u16/u8)"
                    .to_owned(),
            );
        }
    }
}
