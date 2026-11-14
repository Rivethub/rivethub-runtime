use std::process::ExitCode;

pub(crate) const ALREADY_RUNNING: u8 = 11;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    Ok,
    Failure,
    Memory,
    Bind,
    Serve,
    Runtime,
    Held,
    AlreadyRunning,
    Child(u8),
}

impl Exit {
    pub fn code(self) -> ExitCode {
        match self {
            Self::Ok => ExitCode::SUCCESS,
            Self::Failure => ExitCode::from(1),
            Self::Memory => ExitCode::from(2),
            Self::Bind | Self::Held => ExitCode::from(3),
            Self::Serve => ExitCode::from(4),
            Self::Runtime => ExitCode::from(5),
            Self::AlreadyRunning => ExitCode::from(ALREADY_RUNNING),
            Self::Child(code) => ExitCode::from(code),
        }
    }
}
