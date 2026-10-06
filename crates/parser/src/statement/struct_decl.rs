use mollie_lexer::Token;
use mollie_shared::Positioned;

use crate::{Attribute, Expr, Ident, NameWithGenerics, Parse, ParseResult, Parser, Type};

#[derive(Debug, Clone, PartialEq, PartialOrd, Hash)]
pub struct Property {
    pub name: Positioned<Ident>,
    pub nullable: Option<Positioned<bool>>,
    pub ty: Positioned<Type>,
    pub default_value: Option<Positioned<Expr>>,
}

impl Parse for Property {
    fn parse(parser: &mut Parser) -> ParseResult<Positioned<Self>> {
        parser.verify_if(Token::is_ident)?;
        parser.verify2_if(|token| matches!(token, Token::Colon | Token::Question))?;

        let name = Ident::parse(parser)?;
        let nullable = parser.consume(&Token::Question).map(|v| v.map(|_| true)).ok();

        parser.consume(&Token::Colon)?;

        let ty = Type::parse(parser)?;

        let default_value = if parser.try_consume(&Token::Eq) { Some(Expr::parse(parser)?) } else { None };

        Ok(name.span.between(ty.span).wrap(Self {
            name,
            nullable,
            ty,
            default_value,
        }))
    }
}

#[derive(Debug, Clone, PartialEq, PartialOrd, Hash)]
pub struct StructDecl {
    pub attributes: Vec<Positioned<Attribute>>,
    /// `value struct`: values are copied instead of referenced.
    pub value: bool,
    pub name: Positioned<NameWithGenerics>,
    pub properties: Positioned<Vec<Positioned<Property>>>,
}

impl StructDecl {
    /// Parses a struct declaration (after its attributes).
    ///
    /// # Errors
    ///
    /// Returns an error if the tokens don't form one.
    pub fn parse(parser: &mut Parser, attributes: Vec<Positioned<Attribute>>) -> ParseResult<Positioned<Self>> {
        let start = parser.consume(&Token::Struct)?;

        let name = NameWithGenerics::parse(parser)?;
        let properties = parser.consume_separated_in(&Token::Comma, &Token::BraceOpen, &Token::BraceClose)?;

        Ok(start.span.between(properties.span).wrap(Self {
            attributes,
            value: false,
            name,
            properties,
        }))
    }
}
