use mollie_lexer::Token;
use mollie_shared::Positioned;

use crate::{Expr, Ident, Parse, ParseResult, Parser};

#[derive(Debug, Clone, PartialEq, PartialOrd, Hash)]
pub enum IndexTarget {
    Named(Ident),
    Expression(Box<Expr>),
}

impl Parse for IndexTarget {
    fn parse(parser: &mut Parser) -> ParseResult<Positioned<Self>> {
        if parser.try_consume(&Token::Dot) {
            Ident::parse(parser).map(|v| v.map(Self::Named))
        } else {
            let start = parser.consume(&Token::BracketOpen)?;
            let value = Expr::parse(parser).map(|v| Self::Expression(Box::new(v.value)))?;
            let end = parser.consume(&Token::BracketClose)?;

            Ok(start.between(&end).wrap(value))
        }
    }
}

#[derive(Debug, Clone, PartialEq, PartialOrd, Hash)]
pub struct IndexExpr {
    pub target: Box<Positioned<Expr>>,
    pub index: Positioned<IndexTarget>,
}

impl IndexExpr {
    pub fn parse(parser: &mut Parser, target: Positioned<Expr>) -> ParseResult<Positioned<Self>> {
        let index = IndexTarget::parse(parser)?;

        Ok(target.between(&index).wrap(Self {
            target: Box::new(target),
            index,
        }))
    }
}
