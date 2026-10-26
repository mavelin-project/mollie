use std::collections::{HashMap, HashSet};

use mollie_shared::{Operator, Span};
use mollie_typing::{ModuleSpan, TypeError};

use crate::{
    FirstPass, TypedAST, TypedASTContextRef,
    block::BlockRef,
    expr::{Expr, ExprRef, IsPattern, LoopId},
    stmt::Stmt,
};

#[derive(Debug, Clone, Default)]
struct State {
    assigned: HashSet<Span>,
    maybe: HashSet<Span>,
}

impl State {
    fn merge(&mut self, other: &Self) {
        self.assigned.retain(|span| other.assigned.contains(span));
        self.maybe.extend(&other.maybe);
    }
}

struct Checker<'a, 'c, 'ctx> {
    ast: &'a TypedAST<FirstPass>,
    context: &'c mut TypedASTContextRef<'ctx>,
    uses: HashMap<Span, Span>,
    deferred: HashMap<Span, (bool, usize)>,
    depth: usize,
    breaks: HashMap<LoopId, State>,
    reported: HashSet<Span>,
}

impl TypedAST<FirstPass> {
    pub(crate) fn check_assignments(&self, root: BlockRef, context: &mut TypedASTContextRef<'_>) {
        if !self.exprs.values().any(|expr| matches!(expr.value, Expr::Uninit)) {
            return;
        }

        let mut checker = Checker {
            ast: self,
            context,
            uses: self.var_uses.iter().copied().collect(),
            deferred: HashMap::new(),
            depth: 0,
            breaks: HashMap::new(),
            reported: HashSet::new(),
        };

        checker.block(root, &mut State::default());
    }
}

impl Checker<'_, '_, '_> {
    fn block(&mut self, block: BlockRef, state: &mut State) -> bool {
        let ast = self.ast;
        let block = &ast[block].value;

        for &stmt in &block.stmts {
            let diverges = match ast[stmt] {
                Stmt::Expr(expr) => self.expr(expr, state),
                Stmt::NewVar { mutable, value, .. } if matches!(ast[value].value, Expr::Uninit) => {
                    let declaration = ast[value].span;

                    self.deferred.insert(declaration, (mutable, self.depth));
                    // Declared again by every iteration of a loop.
                    state.assigned.remove(&declaration);
                    state.maybe.remove(&declaration);

                    false
                }
                Stmt::NewVar { value, .. } => self.expr(value, state),
            };

            // The rest is never reached.
            if diverges {
                return true;
            }
        }

        block.expr.is_some_and(|expr| self.expr(expr, state))
    }

    fn expr(&mut self, expr: ExprRef, state: &mut State) -> bool {
        let ast = self.ast;

        match &ast[expr].value {
            Expr::Var(_) => {
                self.read(expr, state);

                false
            }
            Expr::Lit(_) | Expr::Func { .. } | Expr::Const(_) | Expr::Uninit | Expr::Error(_) => false,
            Expr::Continue { .. } | Expr::Unreachable => true,
            Expr::Array { elements, .. } => self.all(elements, state),
            Expr::Call { func, args } => self.expr(*func, state) || self.all(args, state),
            Expr::Construct { fields, .. } => fields.iter().any(|&(.., value)| self.expr(value, state)),
            &(Expr::AdtIndex { target, .. }
            | Expr::TraitFunc { target, .. }
            | Expr::BoundFunc { target, .. }
            | Expr::TypeCast(target, _)
            | Expr::Format { value: target, .. }
            | Expr::Unary { expr: target, .. }) => self.expr(target, state),
            Expr::VTableIndex { target, .. } => target.is_some_and(|target| self.expr(target, state)),
            &Expr::ArrayIndex { target, element } => self.expr(target, state) || self.expr(element, state),
            &Expr::Block(block) => self.block(block, state),
            &Expr::Return(value) => {
                if let Some(value) = value {
                    self.expr(value, state);
                }

                true
            }
            &Expr::Panic(message) => {
                self.expr(message, state);

                true
            }
            &Expr::Break { id, value } => {
                if !value.is_some_and(|value| self.expr(value, state)) {
                    self.breaks
                        .entry(id)
                        .and_modify(|broken| broken.merge(state))
                        .or_insert_with(|| state.clone());
                }

                true
            }
            &Expr::IfElse { condition, block, otherwise } => {
                if self.expr(condition, state) {
                    return true;
                }

                let mut then = state.clone();
                let then_diverges = self.block(block, &mut then);
                let otherwise_diverges = otherwise.is_some_and(|otherwise| self.expr(otherwise, state));

                // Paths that never finish don't meet the others.
                match (then_diverges, otherwise_diverges) {
                    (true, diverges) => diverges,
                    (false, true) => {
                        *state = then;

                        false
                    }
                    (false, false) => {
                        state.merge(&then);

                        false
                    }
                }
            }
            &Expr::While { condition, block, id } => {
                if self.expr(condition, state) {
                    return true;
                }

                // The body may not run at all.
                let body = self.repeated(block, state);

                state.maybe.extend(body.maybe);

                if let Some(broken) = self.breaks.remove(&id) {
                    state.merge(&broken);
                }

                false
            }
            &Expr::Loop { block, id } => {
                let body = self.repeated(block, state);

                // It only ends with `break`.
                let Some(mut broken) = self.breaks.remove(&id) else {
                    return true;
                };

                broken.maybe.extend(body.maybe);
                *state = broken;

                false
            }
            // Assignments in closures change their copies of the variables.
            &Expr::Closure { body, .. } => {
                self.repeated(body, state);

                false
            }
            &Expr::Binary { operator, lhs, rhs } => match (operator.value, &ast[lhs].value) {
                (Operator::Assign, Expr::Var(_)) => {
                    let diverges = self.expr(rhs, state);

                    if !diverges {
                        self.assign(lhs, state);
                    }

                    diverges
                }
                // The right operand may not be evaluated.
                (Operator::And | Operator::Or, _) => {
                    if self.expr(lhs, state) {
                        return true;
                    }

                    let mut right = state.clone();

                    self.expr(rhs, &mut right);
                    state.maybe.extend(right.maybe);

                    false
                }
                // `x += 1` reads `x` too.
                _ => self.expr(lhs, state) || self.expr(rhs, state),
            },
            Expr::IsPattern { target, pattern } => self.expr(*target, state) || self.pattern(pattern, state),
        }
    }

    fn repeated(&mut self, block: BlockRef, state: &State) -> State {
        let mut body = state.clone();

        self.depth += 1;
        self.block(block, &mut body);
        self.depth -= 1;

        body
    }

    fn all(&mut self, exprs: &[ExprRef], state: &mut State) -> bool {
        exprs.iter().any(|&expr| self.expr(expr, state))
    }

    fn pattern(&mut self, pattern: &IsPattern<FirstPass>, state: &mut State) -> bool {
        match pattern {
            &IsPattern::Literal(value) => self.expr(value, state),
            IsPattern::EnumVariant { values, .. } => values
                .iter()
                .any(|(.., pattern)| pattern.as_ref().is_some_and(|pattern| self.pattern(pattern, state))),
            IsPattern::Wildcard | IsPattern::Binding { .. } | IsPattern::TypeName { .. } => false,
        }
    }

    fn read(&mut self, var: ExprRef, state: &State) {
        let span = self.ast[var].span;

        if let Some(&declaration) = self.uses.get(&span)
            && self.deferred.contains_key(&declaration)
            && !state.assigned.contains(&declaration)
            && self.reported.insert(declaration)
            && let Expr::Var(name) = &self.ast[var].value
        {
            self.context
                .type_solver
                .error(TypeError::UsedBeforeAssigned { name: name.clone() }, ModuleSpan(self.ast.module, span));
        }
    }

    fn assign(&mut self, var: ExprRef, state: &mut State) {
        let span = self.ast[var].span;
        let Some(&declaration) = self.uses.get(&span) else {
            return;
        };
        let Some(&(mutable, depth)) = self.deferred.get(&declaration) else {
            return;
        };

        // An immutable variable may be assigned already here, or by a previous
        // run of the loop or closure around.
        if !mutable
            && (state.maybe.contains(&declaration) || depth < self.depth)
            && let Expr::Var(name) = &self.ast[var].value
        {
            self.context
                .type_solver
                .error(TypeError::AssignToImmutable { name: name.clone() }, ModuleSpan(self.ast.module, span));
        }

        state.assigned.insert(declaration);
        state.maybe.insert(declaration);
    }
}
