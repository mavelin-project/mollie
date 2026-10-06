use mollie_lexer::Token;
use mollie_shared::Positioned;

use crate::{Expr, Ident, Parse, ParseResult, Parser, TypePathExpr, TypePathSegment};

#[derive(Debug, Clone, PartialEq, PartialOrd, Hash)]
pub struct NameValue {
    pub name: Positioned<Ident>,
    pub value: Option<Positioned<Expr>>,
}

impl Parse for NameValue {
    fn parse(parser: &mut Parser) -> ParseResult<Positioned<Self>> {
        let name = Ident::parse(parser)?;
        let value = if parser.try_consume(&Token::Colon) {
            Some(Expr::parse(parser)?)
        } else {
            None
        };

        Ok(name
            .span
            .between(value.as_ref().map_or(name.span, |value| value.span))
            .wrap(Self { name, value }))
    }
}

#[derive(Debug, Clone, PartialEq, PartialOrd, Hash)]
pub struct NodeExpr {
    pub name: Positioned<TypePathExpr>,
    pub from: Option<(Positioned<Box<Expr>>, Positioned<Ident>)>,
    pub properties: Vec<Positioned<NameValue>>,
    pub children: Positioned<Vec<Positioned<Self>>>,
}

impl NodeExpr {
    /// Parses a node (`Name { ... }`) whose name was already parsed.
    ///
    /// # Errors
    ///
    /// Returns an error if the tokens don't form one.
    pub fn parse(name: Positioned<TypePathExpr>, parser: &mut Parser) -> ParseResult<Positioned<Self>> {
        parser.verify_if(|t| matches!(t, Token::BraceOpen | Token::From | Token::Less))?;

        let from = if parser.try_consume(&Token::From) {
            parser.consume(&Token::Less)?;

            let target = Expr::parse(parser)?.map(Box::new);

            parser.consume(&Token::As)?;

            let ty = Ident::parse(parser)?;

            parser.consume(&Token::Greater)?;

            Some((target, ty))
        } else {
            None
        };

        parser.consume(&Token::BraceOpen)?;

        let mut properties = Vec::new();
        let mut children = Vec::new();
        let mut is_property_parsing = true;
        let mut last_comma = None;

        loop {
            if parser.check_if(Token::is_ident) {
                if is_property_parsing {
                    if parser.check2_one_of(&[Token::BraceOpen, Token::PathSep, Token::From, Token::Less]) {
                        is_property_parsing = false;

                        continue;
                    }

                    // Properties are separated by commas: without one, this
                    // is an error (nothing else would consume the name).
                    if last_comma == Some(false) {
                        parser.consume(&Token::Comma)?;
                    }

                    if parser.check2(&Token::Colon) {
                        let name = Ident::parse(parser)?;

                        parser.consume(&Token::Colon)?;

                        let value = Expr::parse(parser)?;
                        let width = name.between(&value);

                        properties.push(width.wrap(NameValue { name, value: Some(value) }));
                    } else {
                        let name = Ident::parse(parser)?;

                        properties.push(name.inner_map(|name| NameValue { name, value: None }));
                    }

                    last_comma = Some(parser.try_consume(&Token::Comma));
                } else {
                    let name = Ident::parse(parser)
                        .or_else(|_| parser.consume_map(|token| if matches!(token, Token::Super) { Some(Ident::new("super")) } else { None }))?;

                    let path = TypePathExpr::parse(TypePathSegment::parse_from(name, parser, false)?, parser, false)?;

                    children.push(parser.nested(|parser| Self::parse(path, parser))?);
                }
            } else {
                break;
            }
        }

        let end = parser.consume(&Token::BraceClose)?;

        let children = if children.is_empty() {
            name.span.between(end.span).wrap(children)
        } else {
            children[0].span.between(children[children.len() - 1].span).wrap(children)
        };

        Ok(name.span.between(end.span).wrap(Self {
            name,
            from,
            properties,
            children,
        }))
    }
}
