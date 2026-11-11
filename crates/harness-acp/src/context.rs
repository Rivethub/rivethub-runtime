use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use agent_client_protocol::schema::v1 as acp;
use kernel::harness::ContextUsage;
use kernel::harness::ContextUsageSource;
use parking_lot::Mutex;
use serde::Deserialize;
use serde::Serialize;
use serde::de::IntoDeserializer as _;
use wire::stream::LlmUsage;

use crate::error::AcpError;

#[derive(Debug, Clone, Serialize, Deserialize, agent_client_protocol::JsonRpcRequest)]
#[request(method = "session/new", response = OpenedSession)]
pub(crate) struct OpenSession {
    #[serde(flatten)]
    pub(crate) request: acp::NewSessionRequest,
}

#[derive(Debug, Clone, Serialize, Deserialize, agent_client_protocol::JsonRpcResponse)]
pub(crate) struct OpenedSession {
    #[serde(flatten)]
    pub(crate) session: acp::NewSessionResponse,
    pub(crate) models: Option<ModelState>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ModelState {
    available_models: Vec<ModelInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModelInfo {
    model_id: String,
    #[serde(rename = "_meta")]
    meta: Option<ModelMeta>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModelMeta {
    total_context_tokens: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LastCall {
    used: u64,
    model: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CallMeta {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    model_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TurnEnd {
    pub(crate) usage: Option<LlmUsage>,
    pub(crate) last_call: Option<LastCall>,
}

#[derive(Debug, Default)]
pub(crate) struct ContextWindows {
    reported: AtomicBool,
    windows: Mutex<HashMap<String, u64>>,
}

impl ContextWindows {
    pub(crate) fn reported(&self) {
        self.reported.store(true, Ordering::Release);
    }

    pub(crate) fn learn(&self, models: Option<ModelState>) {
        let Some(models) = models else {
            return;
        };
        let mut windows = self.windows.lock();
        for model in models.available_models {
            if let Some(window) = model.meta.and_then(|meta| meta.total_context_tokens) {
                windows.insert(model.model_id, window);
            }
        }
    }

    pub(crate) fn after_turn(&self, last_call: Option<&LastCall>) -> Option<ContextUsage> {
        if self.reported.load(Ordering::Acquire) {
            return None;
        }
        let call = last_call?;
        let window = call
            .model
            .as_ref()
            .and_then(|model| self.windows.lock().get(model).copied());
        Some(ContextUsage {
            used: call.used,
            window,
            source: ContextUsageSource::LastCall,
        })
    }
}

pub(crate) fn reported(update: &acp::UsageUpdate) -> ContextUsage {
    ContextUsage {
        used: update.used,
        window: (update.size > 0).then_some(update.size),
        source: ContextUsageSource::Harness,
    }
}

pub(crate) fn last_call(response: &acp::PromptResponse) -> Result<Option<LastCall>, AcpError> {
    let Some(meta) = &response.meta else {
        return Ok(None);
    };
    let call = CallMeta::deserialize(meta.into_deserializer())
        .map_err(|source| AcpError::UsageMeta { source })?;
    let (Some(input), Some(output)) = (call.input_tokens, call.output_tokens) else {
        return Ok(None);
    };
    Ok(input.checked_add(output).map(|used| LastCall {
        used,
        model: call.model_id,
    }))
}
