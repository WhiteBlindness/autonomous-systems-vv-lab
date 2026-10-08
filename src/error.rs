#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LabError(pub String);

impl std::fmt::Display for LabError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for LabError {}
