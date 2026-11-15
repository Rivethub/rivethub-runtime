use rustc_hir::ItemKind;
use rustc_lint::{LateContext, LateLintPass};

use crate::util;

dylint_linting::impl_late_lint!(
    pub(crate) DYNVALUE,
    Warn,
    "home-made dynamic value model (deserialize into typed structs)",
    Dynvalue::default()
);

#[derive(Default)]
pub(crate) struct Dynvalue;

impl<'tcx> LateLintPass<'tcx> for Dynvalue {
    fn check_item(&mut self, cx: &LateContext<'tcx>, item: &'tcx rustc_hir::Item<'tcx>) {
        if util::in_test_code(cx, item.hir_id(), item.span) {
            return;
        }
        match &item.kind {
            ItemKind::Enum(_, _, enum_def) => {
                if enum_def
                    .variants
                    .iter()
                    .any(|variant| variant.ident.name.as_str() == "Null")
                {
                    util::lint(
                        cx,
                        DYNVALUE,
                        item.hir_id(),
                        item.span,
                        "value tree with a Null variant (deserialize into typed structs and enums)"
                            .to_owned(),
                    );
                }
            }
            ItemKind::Impl(impl_) => {
                if let Some(trait_ref) = impl_.of_trait
                    && let Some(segment) = trait_ref.trait_ref.path.segments.last()
                    && segment.ident.name.as_str() == "Deserializer"
                {
                    util::lint(
                        cx,
                        DYNVALUE,
                        item.hir_id(),
                        item.span,
                        "hand-written Deserializer (deserialize straight into typed structs)"
                            .to_owned(),
                    );
                }
            }
            _ => {}
        }
    }
}
