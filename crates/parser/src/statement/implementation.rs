use mollie_lexer::Token;
use mollie_shared::Positioned;
use mollie_typing::Accessor;

use crate::{Argument, BlockExpr, Ident, Parse, ParseError, ParseResult, Parser, TypePathExpr, ty::Type};

#[derive(Debug, Clone, PartialEq, PartialOrd, Hash)]
pub struct ImplFunction {
    pub public: bool,
    pub name: Positioned<Ident>,
    pub generics: Vec<Positioned<crate::GenericParam>>,
    pub this: Option<Positioned<()>>,
    pub mut_self: bool,
    pub args: Vec<Positioned<Argument>>,
    pub returns: Option<Positioned<Type>>,
    pub body: Positioned<BlockExpr>,
    pub external: bool,
    pub accessor: Option<Accessor>,
}

impl Parse for ImplFunction {
    fn parse(parser: &mut Parser) -> ParseResult<Positioned<Self>> {
        let public = parser.try_consume(&Token::Pub);
        // `extern` is a keyword only there, it stays a name everywhere else.
        // `extern get func` and `extern set func` declare properties.
        let external = parser.check_if(|token| token.is_ident_and(|name| name == "extern"))
            && (parser.check2(&Token::Func) || parser.check2_if(|token| token.is_ident_and(|name| name == "get" || name == "set")));

        if external {
            parser.next();
        }

        // `get` and `set` are keywords only there too.
        let accessor = if parser.check2(&Token::Func) {
            if parser.check_if(|token| token.is_ident_and(|name| name == "get")) {
                Some(Accessor::Getter)
            } else if parser.check_if(|token| token.is_ident_and(|name| name == "set")) {
                Some(Accessor::Setter)
            } else {
                None
            }
        } else {
            None
        };

        if accessor.is_some() {
            parser.next();
        }

        let start = parser.consume(&Token::Func)?;

        let name = Ident::parse(parser)?;
        let generics = if parser.try_consume(&Token::Less) {
            let generics = parser.consume_separated_until(&Token::Comma, &Token::Greater)?;

            parser.consume(&Token::Greater)?;

            generics
        } else {
            Vec::new()
        };

        parser.consume(&Token::ParenOpen)?;

        let mut_self = parser.check(&Token::Mut) && parser.check2(&Token::This);

        if mut_self {
            parser.next();
        }

        let this = parser.consume(&Token::This).ok().map(|v| v.wrap(()));

        if this.is_some() {
            parser.try_consume(&Token::Comma);
        }

        let mut args = Vec::new();

        while !parser.check(&Token::ParenClose) {
            if !args.is_empty() {
                parser.consume(&Token::Comma)?;
            }

            if parser.check(&Token::ParenClose) {
                break;
            }

            args.push(Argument::parse(parser)?);
        }

        parser.consume(&Token::ParenClose)?;

        let returns = if parser.try_consume(&Token::Arrow) {
            Some(Type::parse(parser)?)
        } else {
            None
        };

        let body = if external {
            parser.consume(&Token::Semi)?.wrap(BlockExpr {
                stmts: Vec::new(),
                final_stmt: None,
            })
        } else {
            BlockExpr::parse(parser)?
        };

        Ok(start.between(&body).wrap(Self {
            public,
            name,
            generics,
            this,
            mut_self,
            args,
            returns,
            body,
            external,
            accessor,
        }))
    }
}

#[derive(Debug, Clone, PartialEq, PartialOrd, Hash)]
pub struct Impl {
    pub generics: Vec<Positioned<crate::GenericParam>>,
    pub trait_name: Option<Positioned<TypePathExpr>>,
    pub target: Positioned<Type>,
    pub functions: Positioned<Vec<Positioned<ImplFunction>>>,
}

impl Parse for Impl {
    fn parse(parser: &mut Parser) -> ParseResult<Positioned<Self>> {
        let start = parser.consume(&Token::Impl)?;

        let generics = if parser.try_consume(&Token::Less) {
            let generics = parser.consume_separated_until(&Token::Comma, &Token::Greater)?;

            parser.consume(&Token::Greater)?;

            generics
        } else {
            Vec::new()
        };

        let target = Type::parse(parser)?;

        let (trait_name, target) = if parser.try_consume(&Token::For) {
            let trait_name = if let Type::Path(ty) = target.value {
                target.span.wrap(ty)
            } else {
                return Err(ParseError::new("expected valid trait name", target.span));
            };

            let target = Type::parse(parser)?;

            (Some(trait_name), target)
        } else {
            (None, target)
        };

        let functions = parser.consume_in(&Token::BraceOpen, &Token::BraceClose)?;

        Ok(start.between(&functions).wrap(Self {
            generics,
            trait_name,
            target,
            functions,
        }))
    }
}
