use mollie_lexer::Token;
use mollie_shared::Positioned;

use crate::{Expr, Ident, LiteralExpr, Parse, ParseResult, Parser, TypePathExpr, TypePathSegment};

#[derive(Debug, Clone, PartialEq, PartialOrd, Hash)]
pub struct NameValuePattern {
    pub name: Positioned<Ident>,
    pub value: Option<Positioned<IsPattern>>,
}

impl Parse for NameValuePattern {
    fn parse(parser: &mut Parser) -> ParseResult<Positioned<Self>> {
        let name = Ident::parse(parser)?;
        let value = parser.try_consume_then(&Token::Colon, |parser| parser.nested(|parser| IsPattern::parse_with(parser, true)))?;

        Ok(if let Some(value) = &value { name.between(value) } else { name.span }.wrap(Self { name, value }))
    }
}

#[derive(Debug, Clone, PartialEq, PartialOrd, Hash)]
pub enum TypePattern {
    /// Binds the value with the type of the path: `shape is Circle circle`.
    Name(Ident),
    /// Patterns of fields: `Option::Some { value }`.
    Values(Vec<Positioned<NameValuePattern>>),
}

#[derive(Debug, Clone, PartialEq, PartialOrd, Hash)]
pub enum IsPattern {
    Literal(LiteralExpr),
    /// `_`, matching anything.
    Wildcard,
    /// A path, optionally followed by field patterns or a name:
    /// `Option::Some { value }`, `Option::None`, `None`, `value`,
    /// `Circle circle`.
    ///
    /// A single name without fields is either a variant of the matched enum
    /// (`None`) or a binding of the whole value (`value`), which is decided
    /// by the type checker.
    Type {
        ty: Positioned<TypePathExpr>,
        pattern: Option<Positioned<TypePattern>>,
    },
}

impl IsPattern {
    /// Whether braces at the current token hold field patterns
    /// (`x is Some { value } && ...`) rather than a block after the pattern
    /// (`if x is None { ... }`), judging by the token after the closing brace.
    fn braces_hold_fields(parser: &mut Parser) -> bool {
        let mut depth = 0usize;
        let mut index = 0;

        loop {
            match parser.peek_nth(index).map(|token| &token.value) {
                None | Some(Token::EOF) => return false,
                Some(Token::BraceOpen) => depth += 1,
                Some(Token::BraceClose) => {
                    depth = depth.saturating_sub(1);

                    if depth == 0 {
                        break;
                    }
                }
                Some(_) => (),
            }

            index += 1;
        }

        matches!(
            parser.peek_nth(index + 1).map(|token| &token.value),
            Some(
                Token::BraceOpen
                    | Token::FatArrow
                    | Token::AndAnd
                    | Token::OrOr
                    | Token::ParenClose
                    | Token::Semi
                    | Token::Comma
                    | Token::If
                    | Token::BracketClose
            )
        )
    }
}

impl Parse for IsPattern {
    fn parse(parser: &mut Parser) -> ParseResult<Positioned<Self>> {
        parser.nested(|parser| Self::parse_with(parser, false))
    }
}

impl IsPattern {
    /// Parses a pattern. Braces after a path in a `nested` pattern (of a
    /// field) are always field patterns, there's no block after it.
    pub(crate) fn parse_with(parser: &mut Parser, nested: bool) -> ParseResult<Positioned<Self>> {
        if let Ok(literal) = LiteralExpr::parse(parser) {
            return Ok(literal.map(Self::Literal));
        }

        if parser.check_if(|token| token.is_ident_and(|name| name == "_")) {
            let token = parser.consume_if(Token::is_ident)?;

            return Ok(token.span.wrap(Self::Wildcard));
        }

        let ty = TypePathExpr::parse(TypePathSegment::parse_from(Ident::parse(parser)?, parser, false)?, parser, false)?;

        if parser.check(&Token::BraceOpen) && (nested || Self::braces_hold_fields(parser)) {
            let values = parser.consume_separated_in(&Token::Comma, &Token::BraceOpen, &Token::BraceClose)?;

            Ok(ty.between(&values).wrap(Self::Type {
                ty,
                pattern: Some(values.map(TypePattern::Values)),
            }))
        } else if parser.check_if(Token::is_ident) {
            let name = Ident::parse(parser)?;

            Ok(ty.between(&name).wrap(Self::Type {
                ty,
                pattern: Some(name.map(TypePattern::Name)),
            }))
        } else {
            Ok(ty.span.wrap(Self::Type { ty, pattern: None }))
        }
    }
}

#[derive(Debug, Clone, PartialEq, PartialOrd, Hash)]
pub struct IsExpr {
    pub target: Box<Positioned<Expr>>,
    pub pattern: Positioned<IsPattern>,
}
