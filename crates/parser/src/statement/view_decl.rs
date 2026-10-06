use mollie_lexer::Token;
use mollie_shared::Positioned;

use crate::{Attribute, Expr, Ident, NameWithGenerics, NodeExpr, Parse, ParseResult, Parser, Type};

#[derive(Debug, Clone, PartialEq, PartialOrd, Hash)]
pub struct ViewProperty {
    pub name: Positioned<Ident>,
    pub nullable: Option<Positioned<bool>>,
    pub ty: Positioned<Type>,
    pub default_value: Option<Positioned<Expr>>,
}

impl Parse for ViewProperty {
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
pub struct ViewDecl {
    pub attributes: Vec<Positioned<Attribute>>,
    pub name: Positioned<NameWithGenerics>,
    pub inherits: Option<Positioned<Ident>>,
    pub properties: Vec<Positioned<ViewProperty>>,
    pub view: Option<Positioned<NodeExpr>>,
}

impl ViewDecl {
    /// Parses a view declaration (after its attributes).
    ///
    /// # Errors
    ///
    /// Returns an error if the tokens don't form one.
    pub fn parse(parser: &mut Parser, attributes: Vec<Positioned<Attribute>>) -> ParseResult<Positioned<Self>> {
        parser.consume(&Token::View)?;

        let name = NameWithGenerics::parse(parser)?;

        let inherits = if parser.try_consume(&Token::Inherits) {
            Some(Ident::parse(parser)?)
        } else {
            None
        };

        parser.consume(&Token::BraceOpen)?;

        let mut properties = Vec::new();

        loop {
            if !properties.is_empty() && parser.check2_if(Token::is_ident) && parser.check3(&Token::Colon) {
                parser.consume(&Token::Comma)?;
            }

            match ViewProperty::parse(parser) {
                Ok(property) => properties.push(property),
                Err(_) => break,
            }
        }

        // let view = if (parser.try_consume(&Token::Comma) ||
        // properties.is_empty()) && parser.check_if(Token::is_ident) {
        // Some(NodeExpr::parse(parser)?) } else {
        //     None
        // };

        let end = parser.consume(&Token::BraceClose)?;

        Ok(name.span.between(end.span).wrap(Self {
            attributes,
            name,
            inherits,
            properties,
            view: None,
        }))
    }
}
