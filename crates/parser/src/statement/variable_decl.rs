use mollie_lexer::Token;
use mollie_shared::Positioned;

use crate::{Expr, Ident, Parse, ParseResult, Parser, Type};

#[derive(Debug, Clone, PartialEq, PartialOrd, Hash)]
pub struct VariableDecl {
    pub external: bool,
    pub mutable: Option<Positioned<()>>,
    pub name: Positioned<Ident>,
    pub ty: Option<Positioned<Type>>,
    pub value: Option<Positioned<Expr>>,
}

impl VariableDecl {
    pub fn parse_with(parser: &mut Parser, external: bool) -> ParseResult<Positioned<Self>> {
        Ok(Self::parse(parser)?.map(|decl| Self { external, ..decl }))
    }
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

        let value = if parser.try_consume(&Token::Eq) {
            Some(Expr::parse(parser)?)
        } else {
            None
        };

        let end = parser.consume(&Token::Semi)?;

        Ok(start.between(&end).wrap(Self {
            external: false,
            mutable,
            name,
            ty,
            value,
        }))
    }
}

#[derive(Debug, Clone, PartialEq, PartialOrd, Hash)]
pub struct ConstDecl {
    pub public: bool,
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

        Ok(start.between(&end).wrap(Self { public: false, name, ty, value }))
    }
}
