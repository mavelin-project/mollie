use mollie_lexer::Token;
use mollie_shared::Positioned;

use crate::{Attribute, Ident, NameWithGenerics, Parse, ParseResult, Parser, Property};

#[derive(Debug, Clone, PartialEq, PartialOrd, Hash)]
pub struct EnumVariant {
    pub attributes: Vec<Positioned<Attribute>>,
    pub name: Positioned<Ident>,
    pub properties: Option<Positioned<Vec<Positioned<Property>>>>,
}

impl Parse for EnumVariant {
    fn parse(parser: &mut Parser) -> ParseResult<Positioned<Self>> {
        let attributes = {
            let mut items = Vec::new();

            while parser.check(&Token::Attr) {
                items.push(Attribute::parse(parser)?);
            }

            items
        };

        parser.verify_if(Token::is_ident)?;

        let name = Ident::parse(parser)?;
        let properties = parser.consume_separated_in(&Token::Comma, &Token::BraceOpen, &Token::BraceClose).ok();

        Ok(if let Some(properties) = &properties {
            name.span.between(properties.span)
        } else {
            name.span
        }
        .wrap(Self { attributes, name, properties }))
    }
}

#[derive(Debug, Clone, PartialEq, PartialOrd, Hash)]
pub struct EnumDecl {
    pub attributes: Vec<Positioned<Attribute>>,
    /// `value enum`: values are copied instead of referenced.
    pub value: bool,
    pub name: Positioned<NameWithGenerics>,
    pub variants: Positioned<Vec<Positioned<EnumVariant>>>,
}

impl EnumDecl {
    /// Parses an enum declaration (after its attributes).
    ///
    /// # Errors
    ///
    /// Returns an error if the tokens don't form one.
    pub fn parse(parser: &mut Parser, attributes: Vec<Positioned<Attribute>>) -> ParseResult<Positioned<Self>> {
        parser.consume(&Token::Enum)?;

        let name = NameWithGenerics::parse(parser)?;
        let variants = parser.consume_separated_in(&Token::Comma, &Token::BraceOpen, &Token::BraceClose)?;

        Ok(name.span.between(variants.span).wrap(Self {
            attributes,
            value: false,
            name,
            variants,
        }))
    }
}
