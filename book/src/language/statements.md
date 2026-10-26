# Statements

These are just as important as expressions. They are what you use to declare everything: variables, functions, and types. A semicolon is required only if it is a variable declaration or an expression that does not end a code block and is not a return value at the end of a function.

## Variables

They are declared using `let`, and can't be changed afterwards, unless they're declared with `let mut`. The value of a variable can be *almost* anything: if-else expressions, code blocks, and even loops (`loop` with `break value`). Also, every variable declaration requires a semicolon at the end. Here are a couple of examples:

```mollie
let protected = "I'm protected by the language rules!";
```


```mollie
let mut dynamic = true;

dynamic = false;
```

```mollie
let should_be_true = if true == true {
  "Reality is stable!"
} else {
  "It seems we're in danger..."
};
```

You don't have to give a variable its value right away. Declare it with a type, and assign it later:

```mollie
let greeting: string;

if hour < 12 {
  greeting = "Good morning";
} else {
  greeting = "Hello";
}

log(greeting);
```

Mollie checks that it gets a value on every path before it's used. And since it isn't `mut`, it can be assigned only once, so you can't assign it inside a loop.

## Constants

`const` declares a value that's computed when the program is compiled. Since nothing is running yet at that point, a constant can only use literals, operators and other constants:

```mollie
const COLUMNS: i32 = 12;
const GAP = 4.0;
const WIDTH = COLUMNS * 80;
```

As you can see, the type can be left out. Constants live next to functions and types, so they can be used anywhere in their module and imported by others.
