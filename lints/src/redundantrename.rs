use crate::util;

dylint_linting::impl_late_lint!(
    pub(crate) REDUNDANT_RENAME,
    Warn,
    "serde rename equal to the field name (drop the attribute; rename_all on the container; alias for a second spelling)",
    RedundantRename::default()
);

#[derive(Default)]
pub(crate) struct RedundantRename;

impl<'tcx> rustc_lint::LateLintPass<'tcx> for RedundantRename {
    fn check_field_def(
        &mut self,
        cx: &rustc_lint::LateContext<'tcx>,
        field: &'tcx rustc_hir::FieldDef<'tcx>,
    ) {
        if util::in_test_code(cx, field.hir_id, field.span) {
            return;
        }
        for (key, value) in
            util::serde_attr_pairs(cx.tcx.hir_attrs(field.hir_id))
        {
            if key == util::symbol("rename")
                && let Some(value) = value
                && value.as_str() == field.ident.name.as_str()
            {
                util::lint(
                    cx,
                    REDUNDANT_RENAME,
                    field.hir_id,
                    field.span,
                    "serde rename equal to the field name"
                        .to_owned(),
                );
            }
        }
    }
}
