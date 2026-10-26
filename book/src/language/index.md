# Before You Begin

This part of the book does NOT describe or cover specific language implementations in detail; it merely touches on some of them.

It is primarily intended to provide an introduction to the language’s structure for those who are just beginning to learn it and/or are unfamiliar with other programming languages.

For more detailed information about its implementation, see [Embedding and Internals](../internals/index.md).

## What is Mollie?

Mollie is a scripting language with a strict type system, much like the Rust programming language.

However, unlike Rust, Mollie has a garbage collector. This is largely because implementing a full-fledged ownership and lifetime system in a language designed to be relatively easy to learn and use would be somewhat impractical.

Also, one of Mollie’s features is a separate type designed primarily for describing user interfaces in a Qt-like style. It’s called `view`. We’ll talk about it in more detail a little later. For now, let’s take a look at...
