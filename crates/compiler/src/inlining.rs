use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
};

use cranelift::{
    codegen::{
        inline::{Inline, InlineCommand},
        ir,
    },
    module::FuncId,
};
use indexmap::IndexMap;

pub const MAX_BODY: usize = 150;

const MAX_INLINED: usize = 3000;

pub fn instructions(func: &ir::Function) -> usize {
    func.layout.blocks().map(|block| func.layout.block_insts(block).count()).sum()
}

fn callees(func: &ir::Function) -> impl Iterator<Item = FuncId> {
    func.dfg.ext_funcs.values().filter_map(|ext| match ext.name {
        ir::ExternalName::User(name) => {
            let name = &func.params.user_named_funcs()[name];

            (name.namespace == 0).then_some(FuncId::from_u32(name.index))
        }
        _ => None,
    })
}

pub fn callees_first<T>(bodies: &IndexMap<FuncId, (ir::Function, T)>) -> Vec<FuncId> {
    let mut order = Vec::with_capacity(bodies.len());
    let mut visited = HashSet::new();

    for &root in bodies.keys() {
        if !visited.insert(root) {
            continue;
        }

        // Functions being visited, with their callees left to visit.
        let mut stack = vec![(root, callees(&bodies[&root].0).collect::<Vec<_>>())];

        while let Some((id, left)) = stack.last_mut() {
            let id = *id;

            if let Some(callee) = left.pop() {
                if let Some((body, _)) = bodies.get(&callee)
                    && visited.insert(callee)
                {
                    stack.push((callee, callees(body).collect()));
                }
            } else {
                order.push(id);
                stack.pop();
            }
        }
    }

    order
}

pub struct Inliner<'a> {
    bodies: &'a HashMap<FuncId, ir::Function>,
    budget: usize,
    inlined: Vec<&'a ir::Function>,
}

impl<'a> Inliner<'a> {
    pub const fn new(bodies: &'a HashMap<FuncId, ir::Function>) -> Self {
        Self {
            bodies,
            budget: MAX_INLINED,
            inlined: Vec::new(),
        }
    }

    pub fn fix_symbols(&self, func: &mut ir::Function, globals: usize) {
        let mut first = globals;

        for body in &self.inlined {
            for (index, global) in body.global_values.values().enumerate() {
                if let ir::GlobalValueData::Symbol {
                    name: ir::ExternalName::User(name),
                    ..
                } = global
                {
                    let name = func.declare_imported_user_function(body.params.user_named_funcs()[*name].clone());
                    let inlined = ir::GlobalValue::from_u32(u32::try_from(first + index).unwrap_or(u32::MAX));

                    if let ir::GlobalValueData::Symbol { name: inlined_name, .. } = &mut func.global_values[inlined] {
                        *inlined_name = ir::ExternalName::User(name);
                    }
                }
            }

            first += body.global_values.len();
        }
    }
}

impl Inline for Inliner<'_> {
    fn inline(&mut self, caller: &ir::Function, _: ir::Inst, opcode: ir::Opcode, target: ir::FuncRef, _: &[ir::Value]) -> InlineCommand<'_> {
        let ir::ExternalName::User(name) = caller.dfg.ext_funcs[target].name else {
            return InlineCommand::KeepCall;
        };
        let name = &caller.params.user_named_funcs()[name];
        let bodies = self.bodies;
        let body = (opcode == ir::Opcode::Call && name.namespace == 0)
            .then(|| bodies.get(&FuncId::from_u32(name.index)))
            .flatten()
            .filter(|body| instructions(body) <= self.budget);

        match body {
            Some(body) => {
                self.budget -= instructions(body);
                self.inlined.push(body);

                InlineCommand::Inline {
                    callee: Cow::Borrowed(body),
                    // Its calls were considered when it was defined.
                    visit_callee: false,
                }
            }
            None => InlineCommand::KeepCall,
        }
    }
}
