#![allow(missing_docs)]

use std::{
    fmt,
    hash::Hash,
    marker::PhantomData,
    ops::{Index, IndexMut},
};

use serde::Serialize;

#[macro_export]
macro_rules! new_idx_type {
    ($(#[$attribute:meta])* $name:ident) => {
        $(#[$attribute])*
        #[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[cfg_attr(feature = "serde", derive(serde::Serialize))]
        #[repr(transparent)]
        pub struct $name(usize);

        impl $crate::Idx for $name {
            const INVALID: Self = Self(usize::MAX);
            const ZERO: Self = Self(0);

            fn new(idx: usize) -> Self {
                Self(idx)
            }

            fn index(self) -> usize {
                self.0
            }
        }
    };
}

pub trait Idx: fmt::Debug + Copy + PartialEq + Eq + Hash + 'static {
    const ZERO: Self;
    const INVALID: Self;

    fn new(idx: usize) -> Self;

    fn index(self) -> usize;
}

#[derive(Clone, Serialize)]
#[serde(bound = "T: Serialize")]
pub struct IndexBoxedSlice<I: Idx, T> {
    #[serde(flatten)]
    pub raw: Box<[T]>,
    _phantom: PhantomData<I>,
}

impl<I: Idx, T> IndexBoxedSlice<I, T> {
    pub const fn len(&self) -> usize {
        self.raw.len()
    }

    pub const fn is_empty(&self) -> bool {
        self.raw.is_empty()
    }

    pub fn get(&self, index: I) -> Option<&T> {
        self.raw.get(index.index())
    }

    pub fn get_mut(&mut self, index: I) -> Option<&mut T> {
        self.raw.get_mut(index.index())
    }

    pub fn into_values(self) -> impl Iterator<Item = T> {
        self.raw.into_iter()
    }

    pub fn values(&self) -> impl Iterator<Item = &T> {
        self.raw.iter()
    }

    pub fn values_mut(&mut self) -> impl Iterator<Item = &mut T> {
        self.raw.iter_mut()
    }

    #[allow(clippy::should_implement_trait)]
    pub fn into_iter(self) -> impl Iterator<Item = (I, T)> {
        self.raw.into_iter().enumerate().map(|(i, value)| (I::new(i), value))
    }

    pub fn iter(&self) -> impl Iterator<Item = (I, &T)> {
        self.raw.iter().enumerate().map(|(i, value)| (I::new(i), value))
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = (I, &mut T)> {
        self.raw.iter_mut().enumerate().map(|(i, value)| (I::new(i), value))
    }
}

impl<I: Idx, T: fmt::Debug> fmt::Debug for IndexBoxedSlice<I, T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Debug::fmt(&self.raw, f)
    }
}

impl<I: Idx, T> Default for IndexBoxedSlice<I, T> {
    fn default() -> Self {
        Self {
            raw: Box::default(),
            _phantom: PhantomData,
        }
    }
}

impl<I: Idx, T> Index<I> for IndexBoxedSlice<I, T> {
    type Output = T;

    fn index(&self, index: I) -> &Self::Output {
        &self.raw[index.index()]
    }
}

impl<I: Idx, T> IndexMut<I> for IndexBoxedSlice<I, T> {
    fn index_mut(&mut self, index: I) -> &mut Self::Output {
        &mut self.raw[index.index()]
    }
}

impl<I: Idx, T> From<Box<[T]>> for IndexBoxedSlice<I, T> {
    fn from(value: Box<[T]>) -> Self {
        Self {
            raw: value,
            _phantom: PhantomData,
        }
    }
}

impl<I: Idx, T> From<Vec<T>> for IndexBoxedSlice<I, T> {
    fn from(value: Vec<T>) -> Self {
        value.into_boxed_slice().into()
    }
}

#[derive(Clone, Serialize)]
#[serde(bound = "T: Serialize")]
pub struct IndexVec<I: Idx, T> {
    #[serde(flatten)]
    pub raw: Vec<T>,
    _phantom: PhantomData<I>,
}

impl<I: Idx, T> IndexVec<I, T> {
    pub const fn new() -> Self {
        Self {
            raw: Vec::new(),
            _phantom: PhantomData,
        }
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            raw: Vec::with_capacity(capacity),
            _phantom: PhantomData,
        }
    }

    pub fn get(&self, index: I) -> Option<&T> {
        self.raw.get(index.index())
    }

    pub fn get_mut(&mut self, index: I) -> Option<&mut T> {
        self.raw.get_mut(index.index())
    }

    pub const fn len(&self) -> usize {
        self.raw.len()
    }

    pub const fn is_empty(&self) -> bool {
        self.raw.is_empty()
    }

    pub fn pop(&mut self) {
        self.raw.pop();
    }

    pub fn push(&mut self, value: T) {
        self.raw.push(value);
    }

    pub fn last(&self) -> Option<&T> {
        self.raw.last()
    }

    pub fn last_mut(&mut self) -> Option<&mut T> {
        self.raw.last_mut()
    }

    pub fn next_index(&self) -> I {
        I::new(self.raw.len())
    }

    pub fn insert(&mut self, value: T) -> I {
        let index = self.next_index();

        self.raw.push(value);

        index
    }

    pub fn into_values(self) -> impl DoubleEndedIterator<Item = T> {
        self.raw.into_iter()
    }

    pub fn values(&self) -> impl DoubleEndedIterator<Item = &T> {
        self.raw.iter()
    }

    pub fn values_mut(&mut self) -> impl DoubleEndedIterator<Item = &mut T> {
        self.raw.iter_mut()
    }

    #[allow(clippy::should_implement_trait)]
    pub fn into_iter(self) -> impl DoubleEndedIterator<Item = (I, T)> {
        self.raw.into_iter().enumerate().map(|(i, value)| (I::new(i), value))
    }

    pub fn iter(&self) -> impl DoubleEndedIterator<Item = (I, &T)> {
        self.raw.iter().enumerate().map(|(i, value)| (I::new(i), value))
    }

    pub fn iter_mut(&mut self) -> impl DoubleEndedIterator<Item = (I, &mut T)> {
        self.raw.iter_mut().enumerate().map(|(i, value)| (I::new(i), value))
    }

    pub fn truncate(&mut self, len: usize) {
        self.raw.truncate(len);
    }

    pub fn into_boxed_slice(self) -> IndexBoxedSlice<I, T> {
        IndexBoxedSlice {
            raw: self.raw.into_boxed_slice(),
            _phantom: PhantomData,
        }
    }
}

impl<I: Idx, T: fmt::Debug> fmt::Debug for IndexVec<I, T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Debug::fmt(&self.raw, f)
    }
}

impl<I: Idx, T> FromIterator<T> for IndexBoxedSlice<I, T> {
    fn from_iter<Iter: IntoIterator<Item = T>>(iter: Iter) -> Self {
        Self {
            raw: iter.into_iter().collect(),
            _phantom: PhantomData,
        }
    }
}

impl<I: Idx, T> FromIterator<T> for IndexVec<I, T> {
    fn from_iter<Iter: IntoIterator<Item = T>>(iter: Iter) -> Self {
        Self {
            raw: iter.into_iter().collect(),
            _phantom: PhantomData,
        }
    }
}

impl<I: Idx, T> Default for IndexVec<I, T> {
    fn default() -> Self {
        Self {
            raw: Vec::new(),
            _phantom: PhantomData,
        }
    }
}

impl<I: Idx, T> Index<I> for IndexVec<I, T> {
    type Output = T;

    fn index(&self, index: I) -> &Self::Output {
        &self.raw[index.index()]
    }
}

impl<I: Idx, T> IndexMut<I> for IndexVec<I, T> {
    fn index_mut(&mut self, index: I) -> &mut Self::Output {
        &mut self.raw[index.index()]
    }
}

impl<I: Idx, T> From<Vec<T>> for IndexVec<I, T> {
    fn from(value: Vec<T>) -> Self {
        Self {
            raw: value,
            _phantom: PhantomData,
        }
    }
}

pub trait IdxEnumerate: Iterator + Sized {
    fn enumerate_idx<T: Idx>(self) -> Enumerate<T, Self>;
}

pub struct Enumerate<T: Idx, I: Iterator> {
    iter: I,
    counter: T,
}

impl<T: Idx, I: Iterator> Iterator for Enumerate<T, I> {
    type Item = (T, I::Item);

    fn next(&mut self) -> Option<Self::Item> {
        let value = self.iter.next()?;
        let index = self.counter;

        self.counter = T::new(index.index() + 1);

        Some((index, value))
    }
}

impl<I: Iterator> IdxEnumerate for I {
    fn enumerate_idx<T: Idx>(self) -> Enumerate<T, Self> {
        Enumerate { iter: self, counter: T::ZERO }
    }
}
