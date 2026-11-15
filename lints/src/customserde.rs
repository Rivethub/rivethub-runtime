use rustc_hir::HirId;
use rustc_lint::{LateContext, LateLintPass};

use crate::util;

dylint_linting::impl_late_lint!(
    pub(crate) CUSTOMSERDE,
    Warn,
    "hand-written serde (use derives, #[serde(try_from/into)], strum, serde_with)",
    Customserde::default()
);

#[derive(Default)]
pub(crate) struct Customserde;

fn serde_violation(attrs: &[rustc_attr_ir::Attribute]) -> Option<String> {
    for (key, value) in util::serde_attr_pairs(attrs) {
        let key = key.as_str();
        match key {
            "serialize_with" | "deserialize_with" => {
                return Some(format!("{key} attribute"));
            }
            "with" if !value.is_some_and(|v| v.as_str().contains("serde_with")) => {
                return Some("with attribute outside serde_with".to_owned());
            }
            _ => {}
        }
    }
    None
}

fn check_attrs(cx: &LateContext<'_>, hir_id: HirId, attrs: &[rustc_attr_ir::Attribute]) {
    let Some(violation) = serde_violation(attrs) else {
        return;
    };
    util::lint(
        cx,
        CUSTOMSERDE,
        hir_id,
        attrs
            .iter()
            .map(rustc_attr_ir::Attribute::span)
            .next()
            .unwrap_or(rustc_span::DUMMY_SP),
        format!("{violation}"),
    );
}

impl<'tcx> LateLintPass<'tcx> for Customserde {
    fn check_item(&mut self, cx: &LateContext<'tcx>, item: &'tcx rustc_hir::Item<'tcx>) {
        if util::in_test_code(cx, item.hir_id(), item.span) {
            return;
        }
        if let rustc_hir::ItemKind::Impl(impl_) = &item.kind
            && let Some(trait_ref) = impl_.of_trait
            && let Some(segment) = trait_ref.trait_ref.path.segments.last()
            && matches!(segment.ident.name.as_str(), "Serialize" | "Deserialize")
        {
            util::lint(
                cx,
                CUSTOMSERDE,
                item.hir_id(),
                item.span,
                "hand-written impl Serialize/Deserialize (derive it; validated newtypes take #[serde(try_from/into)])"
                    .to_owned(),
            );
        }
        check_attrs(cx, item.hir_id(), cx.tcx.hir_attrs(item.hir_id()));
    }

    fn check_field_def(
        &mut self,
        cx: &LateContext<'tcx>,
        field: &'tcx rustc_hir::FieldDef<'tcx>,
    ) {
        if util::in_test_code(cx, field.hir_id, field.span) {
            return;
        }
        check_attrs(cx, field.hir_id, cx.tcx.hir_attrs(field.hir_id));
    }
}
