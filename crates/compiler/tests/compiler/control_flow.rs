use crate::run_i32;

#[test]
fn if_else_produces_a_value() {
    assert_eq!(run_i32("let x = 5;\nif x > 3 { 10 } else { 20 }"), 10);
}

#[test]
fn else_if_chains() {
    assert_eq!(
        run_i32(
            "func classify(n: i32) -> i32 {
    if n < 0 { 0 - 1 } else if n == 0 { 0 } else { 1 }
}

classify(-5) + classify(0) * 10 + classify(7) * 100"
        ),
        99
    );
}

#[test]
fn while_loop() {
    assert_eq!(
        run_i32(
            "let mut total = 0;
let mut i = 0;

while i < 10 {
    total += i;
    i += 1;
}

total"
        ),
        45
    );
}

#[test]
fn if_without_else_in_loop() {
    assert_eq!(
        run_i32(
            "let mut count = 0;
let mut i = 1;

while i <= 10 {
    if i - (i / 2) * 2 == 0 {
        count += 1;
    }

    i += 1;
}

count"
        ),
        5
    );
}

#[test]
fn block_is_an_expression() {
    assert_eq!(run_i32("let x = {\n    let a = 2;\n    a * 3\n};\nx + 1"), 7);
}

#[test]
fn recursion() {
    assert_eq!(
        run_i32(
            "func factorial(n: i32) -> i32 {
    if n == 0 { 1 } else { n * factorial(n - 1) }
}

factorial(5)"
        ),
        120
    );
}

#[test]
fn mutual_recursion() {
    assert_eq!(
        run_i32(
            "func even(n: i32) -> bool {
    if n == 0 { true } else { odd(n - 1) }
}

func odd(n: i32) -> bool {
    if n == 0 { false } else { even(n - 1) }
}

if even(10) && odd(7) { 1 } else { 0 }"
        ),
        1
    );
}

#[test]
fn a_block_like_statement_ends_before_a_new_expression() {
    // `-1` after the loop is the block's value, not a subtraction from the
    // loop (like in Rust).
    assert_eq!(
        run_i32(
            "func find(values: i32[], wanted: i32) -> i32 {
    for index in 0..values.len() {
        if values[index] == wanted {
            return index as i32;
        }
    }

    -1
}

find([4, 5, 6], 6) * 10 + find([4, 5, 6], 9)"
        ),
        19
    );
}
