use std::cell::RefCell;

use rustc_hir::{ItemId, ItemKind};
use rustc_lint::{LateContext, LateLintPass};

use crate::util;

dylint_linting::impl_late_lint!(
    pub(crate) WIREPUB,
    Warn,
    "private field on a wire payload struct (payloads have pub fields; validated newtypes are exempt)",
    Wirepub::default()
);

#[derive(Default)]
pub(crate) struct Wirepub {
    payloads: RefCell<Vec<String>>,
    fields: RefCell<Vec<(rustc_span::Span, rustc_hir::HirId)>>,
}

impl<'tcx> LateLintPass<'tcx> for Wirepub {
    fn check_item(&mut self, cx: &LateContext<'tcx>, item: &'tcx rustc_hir::Item<'tcx>) {
        if !util::crate_name(cx).starts_with("wire") || util::in_test_code(cx, item.hir_id(), item.span)
        {
            return;
        }
        if let ItemKind::Impl(impl_) = &item.kind
            && let Some(trait_ref) = impl_.of_trait
            && let Some(segment) = trait_ref.trait_ref.path.segments.last()
            && matches!(segment.ident.name.as_str(), "Serialize" | "Deserialize")
        {
            self.payloads
                .borrow_mut()
                .push(util::type_name(impl_.self_ty).map(|name| name.to_string()).unwrap_or_default());
        }
    }

    fn check_field_def(
        &mut self,
        cx: &LateContext<'tcx>,
        field: &'tcx rustc_hir::FieldDef<'tcx>,
    ) {
        if !util::crate_name(cx).starts_with("wire")
            || util::in_test_code(cx, field.hir_id, field.span)
            || util::has_pub_keyword(cx, field.vis_span)
        {
            return;
        }
        self.fields.borrow_mut().push((field.span, field.hir_id));
    }

    fn check_crate_post(&mut self, cx: &LateContext<'tcx>) {
        let payloads = self.payloads.borrow();
        for (span, hir_id) in self.fields.borrow().iter() {
            let owner = cx.tcx.hir_get_parent_item(*hir_id);
            let item = cx.tcx.hir_item(ItemId { owner_id: owner });
            let ItemKind::Struct(ident, _, data) = &item.kind else {
                continue;
            };
            if matches!(data, rustc_hir::VariantData::Tuple(..)) {
                continue;
            }
            let name = ident.name.to_string();
            if !payloads.contains(&name) {
                continue;
            }
            util::lint(
                cx,
                WIREPUB,
                *hir_id,
                *span,
                "private field on a wire payload struct (wire payloads have pub fields; validated newtypes are exempt)"
                    .to_owned(),
            );
        }
        drop(payloads);
        self.payloads.borrow_mut().clear();
        self.fields.borrow_mut().clear();
    }
}
