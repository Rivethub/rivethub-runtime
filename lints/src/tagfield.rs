use rustc_lint::{LateContext, LateLintPass};

use crate::util;

dylint_linting::impl_late_lint!(
    pub(crate) TAGFIELD,
    Warn,
    "constant type tag as a field (use #[serde(tag = \"type\")])",
    Tagfield::default()
);

#[derive(Default)]
pub(crate) struct Tagfield;

impl<'tcx> LateLintPass<'tcx> for Tagfield {
    fn check_field_def(
        &mut self,
        cx: &LateContext<'tcx>,
        field: &'tcx rustc_hir::FieldDef<'tcx>,
    ) {
        if util::in_test_code(cx, field.hir_id, field.span)
            || field.ident.name != util::symbol("type")
        {
            return;
        }
        if util::type_name(field.ty).is_some_and(|name| name.as_str() == "String") {
            util::lint(
                cx,
                TAGFIELD,
                field.hir_id,
                field.span,
                "`r#type` field typed String (use #[serde(tag = \"type\")] on the struct or enum)"
                    .to_owned(),
            );
            return;
        }
        if let Some(did) = util::resolved_enum(cx, field.ty)
            && cx.tcx.adt_def(did).variants().len() == 1
        {
            util::lint(
                cx,
                TAGFIELD,
                field.hir_id,
                field.span,
                "constant type tag as a field (use #[serde(tag = \"type\")])"
                    .to_owned(),
            );
        }
    }
}
