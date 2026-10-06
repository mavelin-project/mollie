use std::fmt;

use crate::Span;

#[derive(Debug, Hash, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
pub struct Positioned<T, S = Span> {
    pub value: T,
    pub span: S,
}

impl<T: fmt::Display> fmt::Display for Positioned<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.value.fmt(f)
    }
}

impl<T, S> Positioned<T, S> {
    pub const fn new(value: T, span: S) -> Self {
        Self { value, span }
    }

    pub fn unpack(self) -> (S, T) {
        (self.span, self.value)
    }
}

impl<T> Positioned<T> {
    pub const fn between<U>(&self, value: &Positioned<U>) -> Span {
        self.span.between(value.span)
    }

    pub const fn wrap<U>(&self, value: U) -> Positioned<U> {
        self.span.wrap(value)
    }

    pub fn map<U, F: FnOnce(T) -> U>(self, f: F) -> Positioned<U> {
        self.span.wrap(f(self.value))
    }

    pub fn inner_map<U, F: FnOnce(Self) -> U>(self, f: F) -> Positioned<U> {
        self.span.wrap(f(self))
    }
}
