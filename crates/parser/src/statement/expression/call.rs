use mollie_lexer::Token;
use mollie_shared::Positioned;

use crate::{Expr, Ident, Parse, ParseResult, Parser};

/// An argument of a call: `value`, or `name: value`.
#[derive(Debug, Clone, PartialEq, PartialOrd, Hash)]
pub struct CallArg {
    pub name: Option<Positioned<Ident>>,
    pub value: Positioned<Expr>,
}

impl Parse for CallArg {
    fn parse(parser: &mut Parser) -> ParseResult<Positioned<Self>> {
        let name = if parser.verify_if(Token::is_ident).is_ok() && parser.verify2(&Token::Colon).is_ok() {
            let name = Ident::parse(parser)?;

            parser.consume(&Token::Colon)?;

            Some(name)
        } else {
            None
        };

        let value = Expr::parse(parser)?;
        let span = name.as_ref().map_or(value.span, |name| name.between(&value));

        Ok(span.wrap(Self { name, value }))
    }
}

#[derive(Debug, Clone, PartialEq, PartialOrd, Hash)]
pub struct FuncCallExpr {
    pub function: Box<Positioned<Expr>>,
    pub args: Positioned<Vec<Positioned<CallArg>>>,
}

impl FuncCallExpr {
    /// # Errors
    ///
    /// Returns error if parsing failed
    pub fn parse(parser: &mut Parser, target: Positioned<Expr>) -> ParseResult<Positioned<Self>> {
        let args = parser.consume_separated_in::<CallArg>(&Token::Comma, &Token::ParenOpen, &Token::ParenClose)?;

        Ok(target.between(&args).wrap(Self {
            function: Box::new(target),
            args,
        }))
    }
}
