//! Errors: declaration mistakes and boot-time violations.

use std::fmt;

/// A stable, machine-readable violation code (SPEC §11.2 item 5). Every
/// docuconf SDK uses the same codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum Code {
    /// A required variable is not set, by the environment or a config file.
    MissingRequired,
    /// A value does not parse as its type.
    InvalidType,
    /// A number or duration is outside `min`/`max`, or a string's length outside its limits.
    OutOfRange,
    /// A string or text file does not match its `pattern`.
    PatternMismatch,
    /// A value is not one of an `enum`'s `values`.
    NotInEnum,
    /// A URL's scheme is not one of `schemes`.
    InvalidScheme,
    /// A list has fewer than `minItems` items.
    TooFewItems,
    /// A list has more than `maxItems` items.
    TooManyItems,
    /// A required file input does not exist.
    FileMissing,
    /// A file input exists but cannot be read.
    FileUnreadable,
    /// A file input is larger than `maxSize`.
    FileTooLarge,
    /// A file does not parse in its format, or a CA bundle has too few certificates.
    FileMalformed,
    /// A config file or `json` value violates its schema or does not bind to the app's type.
    SchemaMismatch,
    /// A certificate does not parse, is expired or not yet valid, uses a disallowed key algorithm, or does not chain to `ca.crt`.
    CertificateInvalid,
    /// A certificate has less than `minRemaining` left.
    CertificateExpiring,
    /// A certificate does not cover a name in `dnsNames`.
    CertificateNameMismatch,
    /// `tls.key` does not match the certificate.
    KeyMismatch,
    /// A keystore cannot be opened with its password.
    KeystoreUnreadable,
}

impl Code {
    /// The code as the spec writes it, such as `missing_required`.
    pub fn as_str(self) -> &'static str {
        match self {
            Code::MissingRequired => "missing_required",
            Code::InvalidType => "invalid_type",
            Code::OutOfRange => "out_of_range",
            Code::PatternMismatch => "pattern_mismatch",
            Code::NotInEnum => "not_in_enum",
            Code::InvalidScheme => "invalid_scheme",
            Code::TooFewItems => "too_few_items",
            Code::TooManyItems => "too_many_items",
            Code::FileMissing => "file_missing",
            Code::FileUnreadable => "file_unreadable",
            Code::FileTooLarge => "file_too_large",
            Code::FileMalformed => "file_malformed",
            Code::SchemaMismatch => "schema_mismatch",
            Code::CertificateInvalid => "certificate_invalid",
            Code::CertificateExpiring => "certificate_expiring",
            Code::CertificateNameMismatch => "certificate_name_mismatch",
            Code::KeyMismatch => "key_mismatch",
            Code::KeystoreUnreadable => "keystore_unreadable",
        }
    }
}

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One problem found while loading configuration at boot.
///
/// `message` never contains the value of a secret variable or the contents
/// of a secret file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    /// The environment variable name, the file input name, or the overlay
    /// name (for an overlay that cannot be read or parsed).
    pub input: String,
    /// The stable violation code.
    pub code: Code,
    /// A human-readable explanation, without the input name.
    pub message: String,
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {} ({})", self.input, self.message, self.code)
    }
}

/// Every violation found at boot, reported together.
///
/// Its `Debug` output is the same as its `Display` output, so `?` from
/// `main` prints the report rather than a struct dump.
#[derive(Clone, PartialEq, Eq)]
pub struct ValidationError {
    /// The violations, sorted by input name: variables first, then file
    /// inputs, then overlays.
    pub violations: Vec<Violation>,
}

impl fmt::Debug for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl ValidationError {
    /// Whether any violation has the given code.
    pub fn has(&self, code: Code) -> bool {
        self.violations.iter().any(|v| v.code == code)
    }

    /// The codes reported for one input.
    pub fn codes_for(&self, input: &str) -> Vec<Code> {
        self.violations
            .iter()
            .filter(|v| v.input == input)
            .map(|v| v.code)
            .collect()
    }
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let n = self.violations.len();
        if n == 1 {
            write!(f, "docuconf: 1 configuration problem:")?;
        } else {
            write!(f, "docuconf: {n} configuration problems:")?;
        }
        for v in &self.violations {
            write!(f, "\n  {v}")?;
        }
        Ok(())
    }
}

impl std::error::Error for ValidationError {}

/// Mistakes in the declaration itself: an invalid variable name, a missing
/// description, a default that breaks its own constraints, a pattern that is
/// not RE2. These are programming errors, found before any value is read.
/// Most of them are compile errors already; these are the ones that need
/// the whole declaration (names, file paths, RE2 patterns).
#[derive(Clone, PartialEq, Eq)]
pub struct DeclarationError {
    /// One message per problem, naming the variable and the Rust field.
    pub problems: Vec<String>,
}

impl fmt::Display for DeclarationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.problems.len() == 1 {
            write!(f, "docuconf: invalid declaration: {}", self.problems[0])
        } else {
            write!(
                f,
                "docuconf: invalid declaration:\n  {}",
                self.problems.join("\n  ")
            )
        }
    }
}

impl fmt::Debug for DeclarationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for DeclarationError {}

/// What [`load`](crate::load) returns on failure.
///
/// `Debug` prints the same report as `Display`, and the error has no
/// `source()`, so `fn main() -> Result<(), Box<dyn Error>>` and
/// `anyhow::Result` both print the report once. Prefer
/// [`load_or_exit`](crate::load_or_exit) in `main`.
pub enum Error {
    /// The struct's declaration is invalid.
    Declaration(DeclarationError),
    /// The environment or files do not satisfy the declaration.
    Validation(ValidationError),
}

impl Error {
    /// The violations, when this is a validation error.
    pub fn violations(&self) -> &[Violation] {
        match self {
            Error::Validation(v) => &v.violations,
            Error::Declaration(_) => &[],
        }
    }

    /// Whether any violation has the given code.
    pub fn has(&self, code: Code) -> bool {
        self.violations().iter().any(|v| v.code == code)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Declaration(e) => e.fmt(f),
            Error::Validation(e) => e.fmt(f),
        }
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

/// The error is the report itself: it has no `source()`, so error
/// reporters such as `anyhow` print it once, not twice.
impl std::error::Error for Error {}

impl From<DeclarationError> for Error {
    fn from(e: DeclarationError) -> Self {
        Error::Declaration(e)
    }
}

impl From<ValidationError> for Error {
    fn from(e: ValidationError) -> Self {
        Error::Validation(e)
    }
}
