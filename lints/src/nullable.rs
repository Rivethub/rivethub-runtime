use rustc_hir::ItemId;
use rustc_lint::{LateContext, LateLintPass};

use crate::util;

dylint_linting::impl_late_lint!(
    pub(crate) NULLABLE,
    Warn,
    "Option<Option<T>> outside a *Patch/*Update body (absent and null are the same unless null means \"clear\")",
    Nullable::default()
);

#[derive(Default)]
pub(crate) struct Nullable;

impl<'tcx> LateLintPass<'tcx> for Nullable {
    fn check_field_def(
        &mut self,
        cx: &LateContext<'tcx>,
        field: &'tcx rustc_hir::FieldDef<'tcx>,
    ) {
        if util::in_test_code(cx, field.hir_id, field.span) || !util::option_option(field.ty) {
            return;
        }
        let owner = cx.tcx.hir_get_parent_item(field.hir_id);
        let parent_name = cx
            .tcx
            .hir_item(ItemId { owner_id: owner })
            .kind
            .ident()
            .map(|ident| ident.name.to_string());
        let patch = parent_name
            .as_deref()
            .is_some_and(|name| name.ends_with("Patch") || name.ends_with("Update"));
        if patch {
            return;
        }
        util::lint(
            cx,
            NULLABLE,
            field.hir_id,
            field.span,
            "Option<Option<T>> outside a *Patch/*Update type (use Option<T> with skip_serializing_if)"
                .to_owned(),
        );
    }
}
