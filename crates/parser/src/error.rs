use std::{error::Error, fmt};

use mollie_lexer::Token;
use mollie_shared::{Positioned, Span};

pub type ParseResult<T> = Result<T, ParseError>;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct ParseError(pub String, pub Span);

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl Error for ParseError {}

impl ParseError {
    pub fn new<T: Into<String>>(value: T, span: Span) -> Self {
        Self(value.into(), span)
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub const fn location(&self) -> Span {
        self.1
    }

    pub fn unexpected_token(token: &Positioned<Token>) -> Self {
        Self(format!("Unexpected {}", token.value), token.span)
    }

    pub fn expected_token(expected: &Token, found: &Positioned<Token>) -> Self {
        Self(format!("Expected {expected}, found {}", found.value), found.span)
    }

    pub fn expected_tokens(expected: &[Token], found: &Positioned<Token>) -> Self {
        let expected = match expected.len() {
            0 => "nothing".into(),
            1 => expected[0].to_string(),
            value => {
                format!(
                    "{}{} or {}",
                    expected[0],
                    expected[1..value - 1].iter().map(ToString::to_string).collect::<Vec<_>>().join(", "),
                    expected[value - 1]
                )
            }
        };

        Self(format!("Expected {expected}, found {}", found.value), found.span)
    }
}
