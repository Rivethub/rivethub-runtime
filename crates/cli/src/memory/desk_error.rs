use capture::seal::SealError;
use kernel::memory::share::desk::DeskError;

use super::dropbox::DropError;
use super::share::ShareError;
use crate::Error;
use crate::share_server::ServerError;

#[must_use]
pub fn desk_error(error: Error) -> DeskError {
    if let Error::ShareServer(server) | Error::Drop(DropError::Server(server)) = error {
        server_error(server)
    } else if let Error::Sharing(
        refusal @ (ShareError::Rescanned { .. }
        | ShareError::Raw
        | ShareError::Withheld(_)
        | ShareError::Seal(
            SealError::Raw | SealError::Withheld(_) | SealError::Unconfirmed(_),
        )),
    ) = error
    {
        DeskError::Refused(refusal.to_string())
    } else {
        DeskError::Failed(error.to_string())
    }
}

fn server_error(error: ServerError) -> DeskError {
    match error {
        ServerError::LoggedOut(server) => DeskError::SignedOut(server.to_string().into()),
        ServerError::Expired { server, .. } => DeskError::LoginExpired(server.to_string().into()),
        ServerError::Denied(reason) => DeskError::LoginDenied(reason),
        ServerError::TokenRefused { status, .. } => DeskError::BadToken(format!("HTTP {status}")),
        ServerError::Http(error) => DeskError::Unreachable(error.to_string()),
        ServerError::Unreachable(reason) => DeskError::Unreachable(reason),
        error @ ServerError::Unavailable { .. } => DeskError::Unreachable(error.to_string()),
        error @ ServerError::Answered(status) => DeskError::DropBox {
            status,
            message: error.to_string(),
        },
        ServerError::Finished => DeskError::UnknownLogin,
        error @ (ServerError::MissingScope { .. } | ServerError::Forbidden { .. }) => {
            DeskError::DropBox {
                status: 403,
                message: error.to_string(),
            }
        }
        error @ (ServerError::DropsOff(_) | ServerError::NoCaptureRoute(_)) => DeskError::DropBox {
            status: 404,
            message: error.to_string(),
        },
        ServerError::Server(error) => DeskError::BadServer(error.to_string()),
        error @ (ServerError::UnknownHub(_)
        | ServerError::AmbiguousHub { .. }
        | ServerError::HubUrl { .. }
        | ServerError::Policy(_)) => DeskError::BadServer(error.to_string()),
        other @ (ServerError::Store(_)
        | ServerError::Keyring(_)
        | ServerError::Ca { .. }
        | ServerError::File(_)
        | ServerError::Io(_)
        | ServerError::Lifetime(_)
        | ServerError::AllowedServer(_)
        | ServerError::Refused(_)
        | ServerError::Protocol(_)
        | ServerError::NoDeviceFlow(_)
        | ServerError::TokenFile(_)
        | ServerError::Cancelled) => DeskError::Failed(other.to_string()),
    }
}
