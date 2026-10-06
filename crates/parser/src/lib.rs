mod error;
mod statement;
mod ty;

use std::{fmt, vec::IntoIter};

use mollie_lexer::{Lexer, Token};
use mollie_shared::{
    Positioned,
    limits::{MAX_NESTING, grow_stack},
};
use peekmore::{PeekMore, PeekMoreIterator};

pub use self::{
    error::{ParseError, ParseResult},
    statement::*,
    ty::*,
};

#[derive(Debug)]
pub struct Parser {
    tokens: PeekMoreIterator<IntoIter<Positioned<Token>>>,
    /// How deeply the code being parsed is nested (see [`Parser::nested`]).
    depth: usize,
}

impl Parser {
    #[must_use]
    pub fn new(tokens: Vec<Positioned<Token>>) -> Self {
        Self {
            tokens: tokens.into_iter().peekmore(),
            depth: 0,
        }
    }

    /// A parser of `tokens` nested in the code of this one (e.g. inside
    /// parentheses or an interpolation), which counts as nested as deeply.
    #[must_use]
    pub fn sub(&self, tokens: Vec<Positioned<Token>>) -> Self {
        Self {
            tokens: tokens.into_iter().peekmore(),
            depth: self.depth,
        }
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
                self.tokens.peek().map(|token| token.span),
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
    pub fn split(&mut self, start: &Token, end: &Token) -> ParseResult<([Positioned<Token>; 2], Self)> {
        let start_position = self.consume(start)?;
        let mut tokens = Vec::new();
        let mut skip = 0;
        let mut end_position = None;

        while let Some(token) = self.next() {
            if &token.value == start {
                skip += 1;
            } else if &token.value == end {
                if skip == 0 {
                    end_position = Some(token);

                    break;
                }

                skip -= 1;
            }

            tokens.push(token);
        }

        if let Some(end_position) = end_position {
            Ok(([start_position, end_position], self.sub(tokens)))
        } else {
            Err(ParseError::expected_token(end, tokens.last()))
        }
    }

    /// Consumes the current token only if it exists and is equal to `value`.
    pub fn try_consume(&mut self, value: &Token) -> bool {
        self.tokens.next_if(|token| &token.value == value).is_some()
    }

    /// Consumes the current token only if it exists and is equal to `value`.
    pub fn try_consume_then<T, F: FnOnce(&mut Self) -> ParseResult<T>>(&mut self, value: &Token, func: F) -> ParseResult<Option<T>> {
        if self.tokens.next_if(|token| &token.value == value).is_some() {
            func(self).map(Some)
        } else {
            Ok(None)
        }
    }

    fn verify_nth(&mut self, index: usize, token: &Token) -> ParseResult<()> {
        if self.tokens.peek_nth(index).is_some_and(|value| &value.value == token) {
            Ok(())
        } else {
            Err(ParseError::expected_token(token, self.tokens.peek_nth(index)))
        }
    }

    fn verify_nth_if<F: Fn(&Token) -> bool>(&mut self, index: usize, func: F) -> ParseResult<()> {
        if self.tokens.peek_nth(index).is_some_and(|value| func(&value.value)) {
            Ok(())
        } else {
            Err(ParseError::unexpected_token(self.tokens.peek_nth(index)))
        }
    }

    pub fn len(&self) -> usize {
        self.tokens.len()
    }

    pub fn is_empty(&mut self) -> bool {
        self.tokens.peek().is_none()
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
        self.tokens.peek_nth(0).is_some_and(|value| func(&value.value))
    }

    /// Returns the `bool` result of `func` if the next token exists.
    pub fn check2_if<F: Fn(&Token) -> bool>(&mut self, func: F) -> bool {
        self.tokens.peek_nth(1).is_some_and(|value| func(&value.value))
    }

    /// Returns the `bool` result of `func` if the next token exists.
    pub fn check3_if<F: Fn(&Token) -> bool>(&mut self, func: F) -> bool {
        self.tokens.peek_nth(2).is_some_and(|value| func(&value.value))
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
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Option<Positioned<Token>> {
        self.tokens.next()
    }

    /// Peeks the current token and returns a reference to it wrapped in `Some`
    /// if it exists, otherwise returning `None`.
    pub fn peek(&mut self) -> Option<&Positioned<Token>> {
        self.tokens.peek()
    }

    /// Peeks the `n`-th token from the current one (`0` is the current one).
    pub fn peek_nth(&mut self, n: usize) -> Option<&Positioned<Token>> {
        self.tokens.peek_nth(n)
    }

    /// Consumes the current token and returns it wrapped in `Some` if the
    /// result of the `func` function is `true`, otherwise returning `None`.
    pub fn next_if<F: Fn(&Token) -> bool>(&mut self, func: F) -> Option<Positioned<Token>> {
        self.tokens.next_if(|value| func(&value.value))
    }

    #[must_use]
    pub fn collect(self) -> Vec<Positioned<Token>> {
        self.tokens.collect()
    }

    pub fn expected_token<T: fmt::Display>(&mut self, expected: T) -> ParseError {
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
        let mut parser = Parser::new(Lexer::lex(value));

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
        let tokens = Lexer::lex("Option::Some(value)");
        let mut parser = Parser::new(tokens);
        let expr = Expr::parse(&mut parser);

        println!("{expr:#?}");
    }

    fn parse_expr(source: &str) -> Expr {
        let mut parser = Parser::new(Lexer::lex(source));

        Expr::parse(&mut parser).expect("the expression must parse").value
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
