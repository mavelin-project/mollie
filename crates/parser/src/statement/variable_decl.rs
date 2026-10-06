use mollie_lexer::Token;
use mollie_shared::Positioned;

use crate::{Expr, Ident, Parse, ParseResult, Parser, Type};

/// `let name = value;`, or `let mut name = value;` for a variable that can be
/// assigned again.
#[derive(Debug, Clone, PartialEq, PartialOrd, Hash)]
pub struct VariableDecl {
    pub mutable: Option<Positioned<()>>,
    pub name: Positioned<Ident>,
    pub ty: Option<Positioned<Type>>,
    pub value: Positioned<Expr>,
}

impl Parse for VariableDecl {
    fn parse(parser: &mut Parser) -> ParseResult<Positioned<Self>> {
        let start = parser.consume(&Token::Let)?;
        let mutable = parser.consume(&Token::Mut).ok().map(|token| token.wrap(()));
        let name = Ident::parse(parser)?;

        let ty = if parser.try_consume(&Token::Colon) {
            Some(Type::parse(parser)?)
        } else {
            None
        };

        parser.consume(&Token::Eq)?;

        let value = Expr::parse(parser)?;
        let end = parser.consume(&Token::Semi)?;

        Ok(start.between(&end).wrap(Self { mutable, name, ty, value }))
    }
}

/// `const NAME: Type = value;`: a constant of a module, evaluated at compile
/// time.
#[derive(Debug, Clone, PartialEq, PartialOrd, Hash)]
pub struct ConstDecl {
    pub name: Positioned<Ident>,
    pub ty: Option<Positioned<Type>>,
    pub value: Positioned<Expr>,
}

impl Parse for ConstDecl {
    fn parse(parser: &mut Parser) -> ParseResult<Positioned<Self>> {
        let start = parser.consume(&Token::Const)?;
        let name = Ident::parse(parser)?;

        let ty = if parser.try_consume(&Token::Colon) {
            Some(Type::parse(parser)?)
        } else {
            None
        };

        parser.consume(&Token::Eq)?;

        let value = Expr::parse(parser)?;
        let end = parser.consume(&Token::Semi)?;

        Ok(start.between(&end).wrap(Self { name, ty, value }))
    }
}
