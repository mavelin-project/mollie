use std::{error::Error, fmt};

use cranelift::module::ModuleError;
use mollie_typing::{Diagnostic, ModuleSpan, TyCtxt};

pub type CompileResult<T> = Result<T, CompileError>;

#[derive(Debug)]
pub enum CompileError {
    /// The program has type errors.
    Type(Vec<Diagnostic>),
    /// Declaring or defining a function or data failed.
    Module(ModuleError),
    /// Cranelift rejected a generated function (a bug in the compiler).
    Codegen(String),
    /// The program uses something the compiler doesn't support yet.
    Unsupported { message: String, span: Option<ModuleSpan> },
    /// A field of a constructed ADT has neither a value nor a default value.
    MissingField { name: String, span: Option<ModuleSpan> },
    /// The compiler failed unexpectedly (a bug). The compiler can't be used
    /// anymore.
    Internal(String),
}

impl CompileError {
    pub fn unsupported(message: impl Into<String>) -> Self {
        Self::Unsupported {
            message: message.into(),
            span: None,
        }
    }

    pub fn missing_field(name: impl Into<String>) -> Self {
        Self::MissingField { name: name.into(), span: None }
    }

    /// Location of the error in the program, if it's known.
    pub const fn span(&self) -> Option<ModuleSpan> {
        match self {
            Self::Unsupported { span, .. } | Self::MissingField { span, .. } => *span,
            Self::Type(_) | Self::Module(_) | Self::Codegen(_) | Self::Internal(_) => None,
        }
    }

    /// Sets the location of the error, unless it already has one: errors get
    /// the span of the innermost expression they come from.
    #[must_use]
    pub const fn or_at(mut self, location: ModuleSpan) -> Self {
        if let Self::Unsupported { span, .. } | Self::MissingField { span, .. } = &mut self
            && span.is_none()
        {
            *span = Some(location);
        }

        self
    }

    /// Displays the error with type errors and locations, in the same format
    /// as [`TyCtxt::display_of_diagnostic`].
    pub fn display<'a>(&'a self, tcx: &'a TyCtxt) -> impl fmt::Display + 'a {
        fmt::from_fn(move |f| {
            if let Self::Type(diagnostics) = self {
                for (index, diagnostic) in diagnostics.iter().enumerate() {
                    if index > 0 {
                        f.write_str("\n")?;
                    }

                    write!(f, "{}", tcx.display_of_diagnostic(diagnostic))?;
                }
            } else {
                write!(f, "error: {self}")?;

                if let Some(ModuleSpan(module, span)) = self.span() {
                    f.write_str("\n  --> ")?;

                    write!(f, "{}", tcx.display_of_module(module))?;

                    write!(f, ":{}:{}", span.range.start_line + 1, span.range.start_column + 1)?;
                }
            }

            Ok(())
        })
    }
}

impl fmt::Display for CompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Type(errors) => write!(f, "{} type error(s)", errors.len()),
            Self::Module(error) => write!(f, "module error: {error}"),
            Self::Codegen(error) => write!(f, "code generation failed: {error}"),
            Self::Unsupported { message, .. } => write!(f, "unsupported: {message}"),
            Self::MissingField { name, .. } => write!(f, "missing value for field `{name}`"),
            Self::Internal(message) => write!(f, "internal compiler error: {message}"),
        }
    }
}

impl Error for CompileError {}

impl From<ModuleError> for CompileError {
    fn from(error: ModuleError) -> Self {
        Self::Module(error)
    }
}
