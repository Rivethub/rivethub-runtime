use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::routing::get;
use strum::IntoEnumIterator as _;
use wire::harness::{HarnessId, StoreHarness, StoreHarnessCapabilities, StoreHarnessList};

use crate::Gateway;

pub(crate) fn mount(router: Router<Arc<Gateway>>) -> Router<Arc<Gateway>> {
    router.route("/api/harnesses", get(list_harnesses))
}

async fn list_harnesses(State(gateway): State<Arc<Gateway>>) -> Json<StoreHarnessList> {
    let list_sessions = gateway.session_lister().is_some();
    Json(StoreHarnessList {
        harnesses: HarnessId::iter()
            .map(|harness_id| StoreHarness {
                harness_id,
                capabilities: StoreHarnessCapabilities { list_sessions },
            })
            .collect(),
    })
}
