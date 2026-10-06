mod error;
mod statement;
mod ty;

use std::{fmt, mem};

use mollie_lexer::{Lexer, Token};
use mollie_shared::{
    Positioned,
    limits::{MAX_NESTING, grow_stack},
};

pub use self::{
    error::{ParseError, ParseResult},
    statement::*,
    ty::*,
};

/// Parses tokens of a buffer it borrows.
///
/// It holds the tokens it didn't consume yet. Consumed tokens are taken out
/// of the buffer (leaving a placeholder), so they're owned without being
/// cloned, and code inside brackets is parsed by a parser of that part of the
/// buffer (see [`Parser::split`]) without moving tokens around.
#[derive(Debug)]
pub struct Parser<'t> {
    tokens: &'t mut [Positioned<Token>],
    /// How deeply the code being parsed is nested (see [`Parser::nested`]).
    depth: usize,
}

/// What's left in the buffer in place of a consumed token.
const fn placeholder() -> Positioned<Token> {
    Positioned {
        value: Token::EOF,
        span: mollie_shared::Span::new(0, 0, mollie_shared::SpanRange::new(0, 0, 0, 0)),
    }
}

impl<'t> Parser<'t> {
    #[must_use]
    pub const fn new(tokens: &'t mut [Positioned<Token>]) -> Self {
        Self { tokens, depth: 0 }
    }

    /// A parser of `tokens` nested in the code of this one (e.g. inside an
    /// interpolation), which counts as nested as deeply.
    #[must_use]
    pub const fn sub<'s>(&self, tokens: &'s mut [Positioned<Token>]) -> Parser<'s> {
        Parser { tokens, depth: self.depth }
    }

    /// Fails if code nested `extra` levels deeper than the current code would
    /// be nested too deeply (see [`MAX_NESTING`]).
    ///
    /// # Errors
    ///
    /// Returns an error if the code is nested too deeply.
    pub fn check_depth(&mut self, extra: usize) -> ParseResult<()> {
        if self.depth + extra > MAX_NESTING {
            Err(ParseError::new(
                format!("the code is nested too deeply (more than {MAX_NESTING} levels)"),
                self.peek().map(|token| token.span),
            ))
        } else {
            Ok(())
        }
    }

    /// Parses code nested one level deeper with `parse`, failing if it's
    /// nested too deeply (see [`MAX_NESTING`]). The stack grows if needed.
    ///
    /// # Errors
    ///
    /// Returns an error if the code is nested too deeply, or the error of
    /// `parse`.
    pub fn nested<T>(&mut self, parse: impl FnOnce(&mut Self) -> ParseResult<T>) -> ParseResult<T> {
        self.check_depth(1)?;
        self.depth += 1;

        let result = grow_stack(|| parse(self));

        self.depth -= 1;

        result
    }

    /// # Errors
    ///
    /// Will return an error if next token is not equal to "start" or can't find
    /// "end" token.
    ///
    /// The returned parser parses the tokens between `start` and the matching
    /// `end`, in place: they're the part of the buffer this parser skips.
    pub fn split(&mut self, start: &Token, end: &Token) -> ParseResult<([Positioned<Token>; 2], Self)> {
        let start_position = self.consume(start)?;
        let mut skip = 0usize;
        let mut end_index = None;

        for (index, token) in self.tokens.iter().enumerate() {
            if &token.value == start {
                skip += 1;
            } else if &token.value == end {
                if skip == 0 {
                    end_index = Some(index);

                    break;
                }

                skip -= 1;
            }
        }

        let Some(end_index) = end_index else {
            return Err(ParseError::expected_token(end, self.tokens.last()));
        };

        let (inside, rest) = mem::take(&mut self.tokens).split_at_mut(end_index);
        let Some((end_position, rest)) = rest.split_first_mut() else {
            unreachable!("the end token was found");
        };

        self.tokens = rest;

        Ok(([start_position, mem::replace(end_position, placeholder())], Parser {
            tokens: inside,
            depth: self.depth,
        }))
    }

    /// Consumes the current token only if it exists and is equal to `value`.
    pub fn try_consume(&mut self, value: &Token) -> bool {
        self.next_if(|token| token == value).is_some()
    }

    /// Consumes the current token only if it exists and is equal to `value`.
    ///
    /// # Errors
    ///
    /// Returns the error of `func`.
    pub fn try_consume_then<T, F: FnOnce(&mut Self) -> ParseResult<T>>(&mut self, value: &Token, func: F) -> ParseResult<Option<T>> {
        if self.try_consume(value) { func(self).map(Some) } else { Ok(None) }
    }

    fn verify_nth(&self, index: usize, token: &Token) -> ParseResult<()> {
        if self.peek_nth(index).is_some_and(|value| &value.value == token) {
            Ok(())
        } else {
            Err(ParseError::expected_token(token, self.peek_nth(index)))
        }
    }

    fn verify_nth_if<F: Fn(&Token) -> bool>(&self, index: usize, func: F) -> ParseResult<()> {
        if self.peek_nth(index).is_some_and(|value| func(&value.value)) {
            Ok(())
        } else {
            Err(ParseError::unexpected_token(self.peek_nth(index)))
        }
    }

    /// Number of tokens left.
    pub const fn len(&self) -> usize {
        self.tokens.len()
    }

    pub const fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }

    /// # Errors
    ///
    /// Will return an error if next token is not equal to "token".
    pub fn verify(&mut self, token: &Token) -> ParseResult<()> {
        self.verify_nth(0, token)
    }

    /// # Errors
    ///
    /// Will return an error if next token at second position is not equal to
    /// "token".
    pub fn verify2(&mut self, token: &Token) -> ParseResult<()> {
        self.verify_nth(1, token)
    }

    /// # Errors
    ///
    /// Will return an error if next token at third position is not equal to
    /// "token".
    pub fn verify3(&mut self, token: &Token) -> ParseResult<()> {
        self.verify_nth(2, token)
    }

    /// # Errors
    ///
    /// Will return an error if "func" return false.
    pub fn verify_if<F: Fn(&Token) -> bool>(&mut self, func: F) -> ParseResult<()> {
        self.verify_nth_if(0, func)
    }

    /// # Errors
    ///
    /// Will return an error if "func" return false.
    pub fn verify2_if<F: Fn(&Token) -> bool>(&mut self, func: F) -> ParseResult<()> {
        self.verify_nth_if(1, func)
    }

    /// # Errors
    ///
    /// Will return an error if "func" return false.
    pub fn verify3_if<F: Fn(&Token) -> bool>(&mut self, func: F) -> ParseResult<()> {
        self.verify_nth_if(2, func)
    }

    /// Checks if the next token exists and it is equal to `value`.
    pub fn check_one_of(&mut self, values: &[Token]) -> bool {
        self.check_if(|v| values.contains(v))
    }

    /// Checks if the next token exists and it is equal to `value`.
    pub fn check2_one_of(&mut self, values: &[Token]) -> bool {
        self.check2_if(|v| values.contains(v))
    }

    /// Checks if the next token exists and it is equal to `value`.
    pub fn check3_one_of(&mut self, values: &[Token]) -> bool {
        self.check3_if(|v| values.contains(v))
    }

    /// Checks if the next token exists and it is equal to `value`.
    pub fn check(&mut self, value: &Token) -> bool {
        self.check_if(|v| v == value)
    }

    /// Checks if the next token exists and it is equal to `value`.
    pub fn check2(&mut self, value: &Token) -> bool {
        self.check2_if(|v| v == value)
    }

    /// Checks if the next token exists and it is equal to `value`.
    pub fn check3(&mut self, value: &Token) -> bool {
        self.check3_if(|v| v == value)
    }

    /// Returns the `bool` result of `func` if the next token exists.
    pub fn check_if<F: Fn(&Token) -> bool>(&mut self, func: F) -> bool {
        self.peek_nth(0).is_some_and(|value| func(&value.value))
    }

    /// Returns the `bool` result of `func` if the next token exists.
    pub fn check2_if<F: Fn(&Token) -> bool>(&mut self, func: F) -> bool {
        self.peek_nth(1).is_some_and(|value| func(&value.value))
    }

    /// Returns the `bool` result of `func` if the next token exists.
    pub fn check3_if<F: Fn(&Token) -> bool>(&mut self, func: F) -> bool {
        self.peek_nth(2).is_some_and(|value| func(&value.value))
    }

    /// # Errors
    ///
    /// Returns error if parsing failed
    pub fn consume_separated<T: Parse>(&mut self, separator: &Token) -> ParseResult<Vec<Positioned<T>>> {
        let mut values = vec![T::parse(self)?];

        while self.try_consume(separator) {
            values.push(T::parse(self)?);
        }

        Ok(values)
    }

    /// # Errors
    ///
    /// Returns error if parsing failed
    pub fn consume_separated_until<T: Parse>(&mut self, separator: &Token, until: &Token) -> ParseResult<Vec<Positioned<T>>> {
        let mut values = vec![T::parse(self)?];

        while !self.check(until) {
            self.consume(separator)?;

            values.push(T::parse(self)?);
        }

        Ok(values)
    }

    /// # Errors
    ///
    /// Returns error if parsing failed
    pub fn consume_separated_in<T: Parse>(&mut self, separator: &Token, from: &Token, to: &Token) -> ParseResult<Positioned<Vec<Positioned<T>>>> {
        let from = self.consume(from)?;

        let mut values = vec![];

        while !self.check(to) {
            if !values.is_empty() {
                self.consume(separator)?;
            }

            if self.check(to) {
                break;
            }

            values.push(T::parse(self)?);
        }

        let to = self.consume(to)?;

        Ok(from.between(&to).wrap(values))
    }

    /// # Errors
    ///
    /// Returns error if parsing failed
    pub fn consume_in<T: Parse>(&mut self, from: &Token, to: &Token) -> ParseResult<Positioned<Vec<Positioned<T>>>> {
        let from = self.consume(from)?;

        let mut values = Vec::new();

        while !self.check(to) {
            values.push(T::parse(self)?);
        }

        let to = self.consume(to)?;

        Ok(from.between(&to).wrap(values))
    }

    /// # Errors
    ///
    /// Returns error if parsing failed
    pub fn consume_until<T: Parse>(&mut self, value: &Token) -> ParseResult<Vec<Positioned<T>>> {
        let mut values = Vec::new();

        while !self.check(value) {
            values.push(T::parse(self)?);
        }

        self.consume(value)?;

        Ok(values)
    }

    /// # Errors
    ///
    /// Returns error if parsing failed
    pub fn consume_while<T: Parse, F: Fn(&mut Self) -> bool>(&mut self, func: F) -> ParseResult<Vec<Positioned<T>>> {
        let mut values = Vec::new();

        while func(self) {
            values.push(T::parse(self)?);
        }

        Ok(values)
    }

    /// Consumes the current token if it exists and is equal to `value`,
    /// otherwise returning `ParseError`.
    ///
    /// # Errors
    ///
    /// Returns error if current token is not equal to `value`
    pub fn consume(&mut self, value: &Token) -> ParseResult<Positioned<Token>> {
        self.next_if(|current| current == value)
            .map_or_else(|| Err(ParseError::expected_token(value, self.peek())), Ok)
    }

    /// Consumes the current token if it exists and is equal to one of the
    /// values inside `values`, otherwise returning `ParseError`.
    ///
    /// # Errors
    ///
    /// Returns error if current token is not equal to one of the tokens inside
    /// `values`
    pub fn consume_one_of(&mut self, values: &[Token]) -> ParseResult<Positioned<Token>> {
        self.next_if(|value| values.contains(value))
            .map_or_else(|| Err(ParseError::expected_tokens(values, self.peek())), Ok)
    }

    /// Consumes the current token if it exists and the result of `func` is
    /// `true`, otherwise returning `ParseError`.
    ///
    /// # Errors
    ///
    /// Returns error if result of the `func` is false
    pub fn consume_if<F: Fn(&Token) -> bool>(&mut self, func: F) -> ParseResult<Positioned<Token>> {
        self.next_if(func).map_or_else(|| Err(ParseError::unexpected_token(self.peek())), Ok)
    }

    /// Consumes the current token if it exists and the result of the `func` is
    /// `Some(T)`, otherwise returning `ParseError`.
    ///
    /// # Errors
    ///
    /// Returns error if there is no token or result of the `func` is None
    pub fn consume_map<T, F: Fn(&Token) -> Option<T>>(&mut self, func: F) -> ParseResult<Positioned<T>> {
        if let Some(value) = self.peek().and_then(|value| func(&value.value).map(|result| value.span.wrap(result))) {
            self.next();

            Ok(value)
        } else {
            Err(ParseError::unexpected_token(self.peek()))
        }
    }

    /// Consumes the current token and returns it wrapped in `Some` if it
    /// exists, otherwise returning `None`.
    #[allow(clippy::should_implement_trait, clippy::mem_replace_with_default)]
    pub const fn next(&mut self) -> Option<Positioned<Token>> {
        let Some((first, rest)) = mem::replace(&mut self.tokens, &mut []).split_first_mut() else {
            return None;
        };

        self.tokens = rest;

        Some(mem::replace(first, placeholder()))
    }

    /// Peeks the current token and returns a reference to it wrapped in `Some`
    /// if it exists, otherwise returning `None`.
    pub const fn peek(&self) -> Option<&Positioned<Token>> {
        self.tokens.first()
    }

    /// Peeks the `n`-th token from the current one (`0` is the current one).
    pub fn peek_nth(&self, n: usize) -> Option<&Positioned<Token>> {
        self.tokens.get(n)
    }

    /// Consumes the current token and returns it wrapped in `Some` if the
    /// result of the `func` function is `true`, otherwise returning `None`.
    pub fn next_if<F: Fn(&Token) -> bool>(&mut self, func: F) -> Option<Positioned<Token>> {
        if self.peek().is_some_and(|token| func(&token.value)) {
            self.next()
        } else {
            None
        }
    }

    /// Takes the tokens left.
    #[must_use]
    pub fn collect(self) -> Vec<Positioned<Token>> {
        self.tokens.iter_mut().map(|token| mem::replace(token, placeholder())).collect()
    }

    pub fn expected_token<T: fmt::Display>(&self, expected: T) -> ParseError {
        self.peek().map_or_else(
            || ParseError(format!("Expected {expected}, found nothing"), None),
            |found| ParseError(format!("Expected {expected}, found {}", found.value), Some(found.span)),
        )
    }
}

pub trait Parse: Sized {
    /// # Errors
    ///
    /// Returns error if parsing failed
    fn parse_value<T: AsRef<str>>(value: T) -> ParseResult<Positioned<Self>> {
        let mut tokens = Lexer::lex(value);
        let mut parser = Parser::new(&mut tokens);

        let value = Self::parse(&mut parser)?;

        parser.try_consume(&Token::EOF);

        Ok(value)
    }

    /// # Errors
    ///
    /// Returns error if parsing failed
    fn parse(parser: &mut Parser) -> ParseResult<Positioned<Self>>;
}

#[cfg(test)]
mod tests {
    use mollie_lexer::Lexer;

    use crate::{Expr, Parse, Parser};

    #[test]
    fn test_node_parsing() {
        let mut tokens = Lexer::lex("Option::Some(value)");
        let mut parser = Parser::new(&mut tokens);
        let expr = Expr::parse(&mut parser);

        println!("{expr:#?}");
    }

    fn parse_expr(source: &str) -> Expr {
        let mut tokens = Lexer::lex(source);
        let mut parser = Parser::new(&mut tokens);

        Expr::parse(&mut parser).expect("the expression must parse").value
    }

    #[test]
    fn parentheses_hold_one_expression() {
        let Expr::Binary(product) = parse_expr("((a + b) * (c))") else {
            panic!("expected a product");
        };

        assert!(matches!(product.lhs.value, Expr::Binary(_)));
        assert!(matches!(parse_expr("()"), Expr::Nothing));
        // The parser goes on after the parentheses.
        assert!(matches!(parse_expr("(a) + (b)"), Expr::Binary(_)));

        // Tokens after the expression are an error, not dropped.
        for source in ["(1 2)", "(a + b c d)", "((a) b)"] {
            let mut tokens = Lexer::lex(source);

            assert!(Expr::parse(&mut Parser::new(&mut tokens)).is_err(), "{source}");
        }
    }

    #[test]
    fn ranges_bind_looser_than_arithmetic() {
        let Expr::Range(range) = parse_expr("a + 1..b * 2") else {
            panic!("expected a range");
        };

        assert!(!range.inclusive);
        assert!(matches!(range.start.value, Expr::Binary(_)));
        assert!(matches!(range.end.value, Expr::Binary(_)));

        let Expr::Range(range) = parse_expr("0..=n") else {
            panic!("expected a range");
        };

        assert!(range.inclusive);
    }

    #[test]
    fn labeled_loops() {
        let Expr::Loop(loop_expr) = parse_expr("'outer: loop { break 'outer 1; }") else {
            panic!("expected a loop");
        };

        assert_eq!(loop_expr.label.map(|label| label.value.0), Some(String::from("outer")));
        assert!(matches!(parse_expr("'rows: for row in rows { continue 'rows; }"), Expr::ForIn(for_in) if for_in.label.is_some()));
        assert!(matches!(parse_expr("'wait: while ready { break; }"), Expr::While(while_expr) if while_expr.label.is_some()));
    }
}
