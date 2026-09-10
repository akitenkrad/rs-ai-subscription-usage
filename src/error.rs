#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    Message(String),
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => e.fmt(f),
            Self::Message(m) => f.write_str(m),
        }
    }
}
impl std::error::Error for Error {}
impl From<std::io::Error> for Error {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}
impl From<String> for Error {
    fn from(value: String) -> Self {
        Self::Message(value)
    }
}
pub type Result<T> = std::result::Result<T, Error>;
