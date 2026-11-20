use agent_client_protocol::v1::schema as acp;
use kernel::harness::SessionEvent;
use tokio::sync::mpsc;

use crate::events;
use crate::permissions::PermissionRegistry;

#[derive(Clone)]
pub(crate) struct SessionClient {
    pub(crate) events: mpsc::Sender<SessionEvent>,
    pub(crate) permissions: std::sync::Arc<PermissionRegistry>,
    pub(crate) options: std::sync::Arc<crate::options::SessionOptions>,
    pub(crate) context: std::sync::Arc<crate::context::ContextWindows>,
}

pub(crate) async fn on_session_update(
    notification: acp::SessionNotification,
    events: &mpsc::Sender<SessionEvent>,
    options: &crate::options::SessionOptions,
    context: &crate::context::ContextWindows,
) -> Result<(), agent_client_protocol::Error> {
    if let acp::SessionUpdate::ConfigOptionUpdate(update) = notification.update {
        options.replace(update.config_options);
        return Ok(());
    }
    if matches!(notification.update, acp::SessionUpdate::UsageUpdate(_)) {
        context.reported();
    }
    match events::session_event(notification.update) {
        Ok(Some(event)) => {
            if events.send(event).await.is_err() {
                Err(agent_client_protocol::Error::internal_error()
                    .data("session event stream is closed"))
            } else {
                Ok(())
            }
        }
        Ok(None) => Ok(()),
        Err(error) => Err(agent_client_protocol::Error::invalid_params().data(error.to_string())),
    }
}

pub(crate) async fn on_permission(
    request: acp::RequestPermissionRequest,
    responder: agent_client_protocol::v1::Responder<acp::RequestPermissionResponse>,
    permissions: &PermissionRegistry,
    events: &mpsc::Sender<SessionEvent>,
) -> Result<(), agent_client_protocol::Error> {
    let (request_id, answer) = permissions.register(request.options.clone());
    let event = match events::permission_request(&request_id, &request) {
        Ok(event) => event,
        Err(error) => {
            permissions.withdraw(&request_id);
            return responder.respond_with_error(
                agent_client_protocol::Error::internal_error().data(error.to_string()),
            );
        }
    };
    if events.send(SessionEvent::Permission(event)).await.is_err() {
        permissions.withdraw(&request_id);
        return responder.respond_with_error(
            agent_client_protocol::Error::internal_error().data("session event stream is closed"),
        );
    }
    let outcome = match answer.await {
        Ok(outcome) => outcome,
        Err(error) => {
            return responder.respond_with_error(
                agent_client_protocol::Error::internal_error()
                    .data(format!("the permission request was withdrawn: {error}")),
            );
        }
    };
    responder.respond(acp::RequestPermissionResponse::new(outcome))
}
