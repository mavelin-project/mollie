use mollie_lexer::Token;
use mollie_shared::Positioned;

use crate::{Ident, Parse, ParseResult, Parser, TypePathExpr, TypePathSegment};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Hash)]
pub enum ImportKind {
    Partial(Positioned<Vec<Positioned<Ident>>>), // import { a, b } from module;
    Named, // import module;
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Hash)]
pub struct Import {
    pub public: bool,
    pub kind: ImportKind,
    pub path: Positioned<TypePathExpr>,
}

impl Parse for Import {
    fn parse(parser: &mut Parser) -> ParseResult<Positioned<Self>> {
        let start = parser.consume(&Token::Import)?;

        let kind = if parser.check(&Token::BraceOpen) {
            let values = parser.consume_separated_in(&Token::Comma, &Token::BraceOpen, &Token::BraceClose)?;

            // `from` isn't a keyword (it's a fine name elsewhere).
            parser.consume_map(|token| if matches!(token, Token::Ident(name) if name == "from") { Some(()) } else { None })?;

            ImportKind::Partial(values)
        } else {
            ImportKind::Named
        };

        let path = TypePathExpr::parse(
            TypePathSegment::parse_from(
                Ident::parse(parser).or_else(|_| parser.consume_map(|token| if matches!(token, Token::Super) { Some(Ident::new("super")) } else { None }))?,
                parser,
                false,
            )?,
            parser,
            false,
        )?;
        let end = parser.consume(&Token::Semi)?;

        Ok(start.between(&end).wrap(Self { public: false, kind, path }))
    }
}
