use mollie_lexer::Token;
use mollie_shared::Positioned;

use crate::{BlockExpr, ClosureExpr, Expr, Ident, Parse, ParseResult, Parser, Precedence, Stmt, TypePathExpr, TypePathSegment, VariableDecl};

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
struct Binding {
    name: Positioned<Ident>,
    value: Option<Positioned<Expr>>,
}

impl Parse for Binding {
    fn parse(parser: &mut Parser) -> ParseResult<Positioned<Self>> {
        let name = Ident::parse(parser)?;
        let value = if parser.try_consume(&Token::Eq) { Some(Expr::parse(parser)?) } else { None };

        Ok(name
            .span
            .between(value.as_ref().map_or(name.span, |value| value.span))
            .wrap(Self { name, value }))
    }
}

#[derive(Debug, Clone, PartialEq, PartialOrd, Hash)]
pub enum NodeChild {
    Node(NodeExpr),
    If {
        condition: Positioned<Expr>,
        then: Positioned<Vec<Positioned<Self>>>,
        otherwise: Option<Positioned<Vec<Positioned<Self>>>>,
    },
    For {
        name: Positioned<Ident>,
        target: Positioned<Expr>,
        body: Positioned<Vec<Positioned<Self>>>,
    },
    Expr(Expr),
}

impl NodeChild {
    fn parse_next(parser: &mut Parser) -> ParseResult<Option<Positioned<Self>>> {
        if parser.check(&Token::If) {
            parser.nested(Self::parse_if).map(Some)
        } else if parser.check(&Token::For) {
            let start = parser.consume(&Token::For)?;
            let name = Ident::parse(parser)?;

            parser.consume(&Token::In)?;

            let target = Expr::parse_pratt_expr(parser, Precedence::Cmp, true)?;
            let body = parser.nested(Self::parse_list)?;

            Ok(Some(start.between(&body).wrap(Self::For { name, target, body })))
        } else if parser.check(&Token::BraceOpen) {
            Ok(Some(BlockExpr::parse(parser)?.map(|block| Self::Expr(Expr::Block(block)))))
        } else if parser.check_if(Token::is_ident) || parser.check(&Token::Super) {
            let name =
                Ident::parse(parser).or_else(|_| parser.consume_map(|token| if matches!(token, Token::Super) { Some(Ident::new("super")) } else { None }))?;
            let path = TypePathExpr::parse(TypePathSegment::parse_from(name, parser, false)?, parser, false)?;

            Ok(Some(parser.nested(|parser| NodeExpr::parse(path, parser))?.map(Self::Node)))
        } else {
            Ok(None)
        }
    }

    fn parse_if(parser: &mut Parser) -> ParseResult<Positioned<Self>> {
        let start = parser.consume(&Token::If)?;
        let condition = Expr::parse_pratt_expr(parser, Precedence::PLowest, true)?;
        let then = Self::parse_list(parser)?;

        let otherwise = if parser.try_consume(&Token::Else) {
            if parser.check(&Token::If) {
                let nested = parser.nested(Self::parse_if)?;

                Some(nested.span.wrap(vec![nested]))
            } else {
                Some(Self::parse_list(parser)?)
            }
        } else {
            None
        };

        let end = otherwise.as_ref().map_or(then.span, |otherwise| otherwise.span);

        Ok(start.span.between(end).wrap(Self::If { condition, then, otherwise }))
    }

    fn parse_list(parser: &mut Parser) -> ParseResult<Positioned<Vec<Positioned<Self>>>> {
        let start = parser.consume(&Token::BraceOpen)?;
        let mut children = Vec::new();

        while let Some(child) = Self::parse_next(parser)? {
            children.push(child);
        }

        let end = parser.consume(&Token::BraceClose)?;

        Ok(start.between(&end).wrap(children))
    }
}

#[derive(Debug, Clone, PartialEq, PartialOrd, Hash)]
pub struct NodeExpr {
    pub name: Positioned<TypePathExpr>,
    pub properties: Vec<Positioned<NameValue>>,
    pub children: Positioned<Vec<Positioned<NodeChild>>>,
}

impl NodeExpr {
    pub fn parse(name: Positioned<TypePathExpr>, parser: &mut Parser) -> ParseResult<Positioned<Self>> {
        parser.consume(&Token::BraceOpen)?;

        let mut properties = Vec::new();
        let mut last_comma = None;

        // Properties come first, until something starts a child.
        while parser.check_if(Token::is_ident) && !parser.check2_one_of(&[Token::BraceOpen, Token::PathSep, Token::Less]) {
            // Properties are separated by commas: without one, this is an
            // error (nothing else would consume the name).
            if last_comma == Some(false) {
                parser.consume(&Token::Comma)?;
            }

            if parser.check2(&Token::Colon) {
                let name = Ident::parse(parser)?;

                parser.consume(&Token::Colon)?;

                let value = Expr::parse(parser)?;
                let width = name.between(&value);

                properties.push(width.wrap(NameValue { name, value: Some(value) }));
            } else if parser.check2_one_of(&[Token::BracketOpen, Token::ParenOpen, Token::FatArrow]) {
                properties.push(parser.nested(Self::parse_closure_field)?);
            } else {
                let name = Ident::parse(parser)?;

                properties.push(name.inner_map(|name| NameValue { name, value: None }));
            }

            last_comma = Some(parser.try_consume(&Token::Comma));
        }

        let mut children = Vec::new();

        while let Some(child) = NodeChild::parse_next(parser)? {
            children.push(child);
        }

        let end = parser.consume(&Token::BraceClose)?;

        let children = if children.is_empty() {
            name.span.between(end.span).wrap(children)
        } else {
            children[0].span.between(children[children.len() - 1].span).wrap(children)
        };

        Ok(name.span.between(end.span).wrap(Self {
            name,
            properties,
            children,
        }))
    }

    fn parse_closure_field(parser: &mut Parser) -> ParseResult<Positioned<NameValue>> {
        let name = Ident::parse(parser)?;
        let bindings = if parser.check(&Token::BracketOpen) {
            parser
                .consume_separated_in::<Binding>(&Token::Comma, &Token::BracketOpen, &Token::BracketClose)?
                .value
        } else {
            Vec::new()
        };
        let args = if parser.check(&Token::ParenOpen) {
            parser.consume_separated_in::<Ident>(&Token::Comma, &Token::ParenOpen, &Token::ParenClose)?
        } else {
            name.span.wrap(Vec::new())
        };

        parser.consume(&Token::FatArrow)?;

        let body = BlockExpr::parse(parser)?;
        let span = name.between(&body);
        let closure = span.wrap(Expr::Closure(ClosureExpr { args, body }));

        let value = if bindings.is_empty() {
            closure
        } else {
            let stmts = bindings
                .into_iter()
                .map(|binding| {
                    let Binding { name, value } = binding.value;
                    let value = value.unwrap_or_else(|| name.span.wrap(Expr::Ident(name.value.clone())));

                    binding.span.wrap(Stmt::VariableDecl(VariableDecl {
                        external: false,
                        mutable: None,
                        name,
                        ty: None,
                        value: Some(value),
                    }))
                })
                .collect();

            span.wrap(Expr::Block(BlockExpr {
                stmts,
                final_stmt: Some(Box::new(closure.map(Stmt::Expression))),
            }))
        };

        Ok(span.wrap(NameValue { name, value: Some(value) }))
    }
}
