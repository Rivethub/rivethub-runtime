pub type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

pub mod guard;
pub mod provider;
