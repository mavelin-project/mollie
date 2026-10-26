use mollie_lexer::Token;
use mollie_shared::Positioned;

use crate::{Attribute, Expr, Ident, NameWithGenerics, Parse, ParseResult, Parser, Type};

#[derive(Debug, Clone, PartialEq, PartialOrd, Hash)]
pub struct Property {
    pub public: bool,
    pub name: Positioned<Ident>,
    pub nullable: Option<Positioned<bool>>,
    pub ty: Positioned<Type>,
    pub default_value: Option<Positioned<Expr>>,
}

impl Parse for Property {
    fn parse(parser: &mut Parser) -> ParseResult<Positioned<Self>> {
        let public = parse_field_visibility(parser)?;
        let name = Ident::parse(parser)?;
        let nullable = parser.consume(&Token::Question).map(|v| v.map(|_| true)).ok();

        parser.consume(&Token::Colon)?;

        let ty = Type::parse(parser)?;

        let default_value = if parser.try_consume(&Token::Eq) { Some(Expr::parse(parser)?) } else { None };

        Ok(name.span.between(ty.span).wrap(Self {
            public,
            name,
            nullable,
            ty,
            default_value,
        }))
    }
}

pub fn parse_field_visibility(parser: &mut Parser) -> ParseResult<bool> {
    let public = parser.check(&Token::Pub);

    if public {
        parser.verify2_if(Token::is_ident)?;
        parser.verify3_if(|token| matches!(token, Token::Colon | Token::Question))?;
        parser.next();
    } else {
        parser.verify_if(Token::is_ident)?;
        parser.verify2_if(|token| matches!(token, Token::Colon | Token::Question))?;
    }

    Ok(public)
}

#[derive(Debug, Clone, PartialEq, PartialOrd, Hash)]
pub struct StructDecl {
    pub public: bool,
    pub attributes: Vec<Positioned<Attribute>>,
    pub value: bool,
    pub name: Positioned<NameWithGenerics>,
    pub properties: Positioned<Vec<Positioned<Property>>>,
}

impl StructDecl {
    pub fn parse(parser: &mut Parser, attributes: Vec<Positioned<Attribute>>) -> ParseResult<Positioned<Self>> {
        let start = parser.consume(&Token::Struct)?;

        let name = NameWithGenerics::parse(parser)?;
        let properties = parser.consume_separated_in(&Token::Comma, &Token::BraceOpen, &Token::BraceClose)?;

        Ok(start.span.between(properties.span).wrap(Self {
            public: false,
            attributes,
            value: false,
            name,
            properties,
        }))
    }
}
