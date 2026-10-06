use std::fmt;

use mollie_lexer::Token;
use mollie_shared::Positioned;

use crate::{Ident, Parse, ParseResult, Parser, TypeArgs};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Hash)]
pub struct TypePathSegment {
    pub name: Positioned<Ident>,
    pub args: Option<Positioned<TypeArgs>>,
}

impl TypePathSegment {
    pub fn parse_from(name: Positioned<Ident>, parser: &mut Parser, special_case: bool) -> ParseResult<Positioned<Self>> {
        let args = if parser.check(&Token::PathSep) && parser.check2(&Token::Less) {
            parser.consume(&Token::PathSep)?;

            Some(TypeArgs::parse(parser)?)
        } else if parser.check(&Token::Less) && !special_case {
            Some(TypeArgs::parse(parser)?)
        } else {
            None
        };

        Ok(name.span.between(args.as_ref().map_or(name.span, |args| args.span)).wrap(Self { name, args }))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Hash)]
pub struct TypePathExpr {
    pub segments: Vec<Positioned<TypePathSegment>>,
}

impl fmt::Display for TypePathExpr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, segment) in self.segments.iter().enumerate() {
            if i != 0 {
                f.write_str("::")?;
            }

            f.write_str(&segment.value.name.value)?;
        }

        Ok(())
    }
}

impl TypePathExpr {
    pub fn parse(init: Positioned<TypePathSegment>, parser: &mut Parser, special_case: bool) -> ParseResult<Positioned<Self>> {
        let mut segments = vec![init];

        while parser.try_consume(&Token::PathSep) {
            segments.push(TypePathSegment::parse_from(Ident::parse(parser)?, parser, special_case)?);
        }

        // if segments.len() == 1 && segments[0].value.name.value.0 == "super" {
        //     return
        // Err(ParseError::unexpected_token(Some(&segments[0].value.name.span.
        // wrap(Token::Super)))); }

        Ok(segments[0].between(&segments[segments.len() - 1]).wrap(Self { segments }))
    }
}
