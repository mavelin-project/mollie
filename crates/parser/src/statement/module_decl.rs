use mollie_lexer::Token;
use mollie_shared::Positioned;

use crate::{Ident, Parse, ParseResult, Parser};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Hash)]
pub struct ModuleDecl {
    pub public: bool,
    pub name: Positioned<Ident>,
}

impl Parse for ModuleDecl {
    fn parse(parser: &mut Parser) -> ParseResult<Positioned<Self>> {
        let start = parser.consume(&Token::Module)?;
        let name = Ident::parse(parser)?;
        let end = parser.consume(&Token::Semi)?;

        Ok(start.between(&end).wrap(Self { public: false, name }))
    }
}
