//! Fixed-width expression optimization for Reflex.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::Arc;
use std::time::Instant;

use rayon::prelude::*;
use reflex::domain::{
    ReplayVerdictWriter, StructuralSchema, StructuralView, SymbolId, VerificationReplayRequest,
};
use reflex::{
    ApplicationWriter, CandidateWriter, ConstructorDescriptor, DomainDefinition, Incomparable,
    KernelRevision, MeasurementDescriptor, MeasurementEnvironment, MeasurementSpace,
    MeasurementWriter, MetricOrdering, NonEmpty, OperatorAlgebra, OperatorDescriptor,
    OperatorEnumerationBatch, Seed, SeedPage, SeedSource, SeedWriter, SemanticIdentity,
    StructuralProtocol, Verdict, VerdictWriter, VerificationBatch, VerificationBatchOutcome,
    VerificationBatchReport, VerificationKernel, VerificationRecord, VerificationReplayBatch,
    VerifiedBatch,
};

const U8_ROTATIONS: u8 = 8;
const MIN_ENCODED_SEED_BYTES: usize = 4 + 4 + 1 + 4;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Expression {
    nodes: Vec<Node>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum Node {
    Input,
    Constant(u8),
    Xor(u32, u32),
    Add(u32, u32),
    RotateLeft(u32, u8),
    Subtract(u32, u32),
    Multiply(u32, u32),
    And(u32, u32),
    Or(u32, u32),
    Not(u32),
    ShiftLeft(u32, u8),
    ShiftRight(u32, u8),
    RotateRight(u32, u8),
    Select(u32, u32, u32),
}

impl Expression {
    #[must_use]
    pub fn input() -> Self {
        Self {
            nodes: vec![Node::Input],
        }
    }

    #[must_use]
    pub fn constant(value: u8) -> Self {
        Self {
            nodes: vec![Node::Constant(value)],
        }
    }

    #[must_use]
    ///
    /// # Panics
    ///
    /// Panics if the combined expression exceeds the `u32` node-ID space.
    pub fn xor(left: Self, right: Self) -> Self {
        Self::binary(left, right, Node::Xor)
    }

    #[must_use]
    ///
    /// # Panics
    ///
    /// Panics if the combined expression exceeds the `u32` node-ID space.
    pub fn wrapping_add(left: Self, right: Self) -> Self {
        Self::binary(left, right, Node::Add)
    }

    #[must_use]
    pub fn wrapping_subtract(left: Self, right: Self) -> Self {
        Self::binary(left, right, Node::Subtract)
    }

    #[must_use]
    pub fn wrapping_multiply(left: Self, right: Self) -> Self {
        Self::binary(left, right, Node::Multiply)
    }

    #[must_use]
    pub fn bitwise_and(left: Self, right: Self) -> Self {
        Self::binary(left, right, Node::And)
    }

    #[must_use]
    pub fn bitwise_or(left: Self, right: Self) -> Self {
        Self::binary(left, right, Node::Or)
    }

    #[must_use]
    pub fn bitwise_not(value: Self) -> Self {
        Self::unary(value, Node::Not)
    }

    #[must_use]
    pub fn shift_left(value: Self, amount: u8) -> Self {
        Self::unary(value, |value| Node::ShiftLeft(value, amount % U8_ROTATIONS))
    }

    #[must_use]
    pub fn shift_right(value: Self, amount: u8) -> Self {
        Self::unary(value, |value| {
            Node::ShiftRight(value, amount % U8_ROTATIONS)
        })
    }

    #[must_use]
    ///
    /// # Panics
    ///
    /// Panics if the expression exceeds the `u32` node-ID space.
    pub fn rotate_left(value: Self, amount: u8) -> Self {
        Self::unary(value, |value| {
            Node::RotateLeft(value, amount % U8_ROTATIONS)
        })
    }

    #[must_use]
    pub fn rotate_right(value: Self, amount: u8) -> Self {
        Self::unary(value, |value| {
            Node::RotateRight(value, amount % U8_ROTATIONS)
        })
    }

    #[must_use]
    ///
    /// # Panics
    ///
    /// Panics if the combined expression exceeds the `u32` node-ID space.
    pub fn select(condition: Self, if_nonzero: Self, if_zero: Self) -> Self {
        let mut nodes = condition.nodes;
        let condition_root =
            u32::try_from(nodes.len() - 1).expect("expression exceeds u32 node IDs");
        let mut interned = Self::interned(&nodes);
        let nonzero_root = Self::append_expression(&mut nodes, &mut interned, if_nonzero);
        let zero_root = Self::append_expression(&mut nodes, &mut interned, if_zero);
        let root = Node::Select(condition_root, nonzero_root, zero_root);
        if !interned.contains_key(&root) {
            nodes.push(root);
        }
        Self { nodes }
    }

    fn unary(value: Self, constructor: impl FnOnce(u32) -> Node) -> Self {
        let mut nodes = value.nodes;
        let value_root = u32::try_from(nodes.len() - 1).expect("expression exceeds u32 node IDs");
        let root = constructor(value_root);
        if !nodes.contains(&root) {
            nodes.push(root);
        }
        Self { nodes }
    }

    fn binary(left: Self, right: Self, constructor: fn(u32, u32) -> Node) -> Self {
        let mut nodes = left.nodes;
        let left_root = u32::try_from(nodes.len() - 1).expect("expression exceeds u32 node IDs");
        let mut interned = Self::interned(&nodes);
        let right_root = Self::append_expression(&mut nodes, &mut interned, right);
        let root = constructor(left_root, right_root);
        if !interned.contains_key(&root) {
            nodes.push(root);
        }
        Self { nodes }
    }

    fn interned(nodes: &[Node]) -> HashMap<Node, u32> {
        nodes
            .iter()
            .copied()
            .enumerate()
            .map(|(index, node)| {
                (
                    node,
                    u32::try_from(index).expect("expression exceeds u32 node IDs"),
                )
            })
            .collect()
    }

    fn append_expression(
        nodes: &mut Vec<Node>,
        interned: &mut HashMap<Node, u32>,
        expression: Self,
    ) -> u32 {
        let mut right_ids = Vec::with_capacity(expression.nodes.len());
        for node in expression.nodes {
            let remapped = match node {
                Node::Input => Node::Input,
                Node::Constant(value) => Node::Constant(value),
                Node::Xor(left, right) => {
                    Node::Xor(right_ids[left as usize], right_ids[right as usize])
                }
                Node::Add(left, right) => {
                    Node::Add(right_ids[left as usize], right_ids[right as usize])
                }
                Node::RotateLeft(value, amount) => {
                    Node::RotateLeft(right_ids[value as usize], amount)
                }
                Node::Subtract(left, right) => {
                    Node::Subtract(right_ids[left as usize], right_ids[right as usize])
                }
                Node::Multiply(left, right) => {
                    Node::Multiply(right_ids[left as usize], right_ids[right as usize])
                }
                Node::And(left, right) => {
                    Node::And(right_ids[left as usize], right_ids[right as usize])
                }
                Node::Or(left, right) => {
                    Node::Or(right_ids[left as usize], right_ids[right as usize])
                }
                Node::Not(value) => Node::Not(right_ids[value as usize]),
                Node::ShiftLeft(value, amount) => {
                    Node::ShiftLeft(right_ids[value as usize], amount)
                }
                Node::ShiftRight(value, amount) => {
                    Node::ShiftRight(right_ids[value as usize], amount)
                }
                Node::RotateRight(value, amount) => {
                    Node::RotateRight(right_ids[value as usize], amount)
                }
                Node::Select(condition, nonzero, zero) => Node::Select(
                    right_ids[condition as usize],
                    right_ids[nonzero as usize],
                    right_ids[zero as usize],
                ),
            };
            let id = if let Some(id) = interned.get(&remapped) {
                *id
            } else {
                let id = u32::try_from(nodes.len()).expect("expression exceeds u32 node IDs");
                nodes.push(remapped);
                interned.insert(remapped, id);
                id
            };
            right_ids.push(id);
        }
        *right_ids.last().expect("Expression is never empty")
    }

    #[must_use]
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    #[must_use]
    ///
    /// # Panics
    ///
    /// Panics only if an `Expression` violates its private nonempty,
    /// topologically ordered representation invariant.
    pub fn evaluate(&self, input: u8) -> u8 {
        let mut values: Vec<u8> = Vec::with_capacity(self.nodes.len());
        for node in &self.nodes {
            let value = match *node {
                Node::Input => input,
                Node::Constant(value) => value,
                Node::Xor(left, right) => values[left as usize] ^ values[right as usize],
                Node::Add(left, right) => {
                    values[left as usize].wrapping_add(values[right as usize])
                }
                Node::RotateLeft(value, amount) => {
                    values[value as usize].rotate_left(amount.into())
                }
                Node::Subtract(left, right) => {
                    values[left as usize].wrapping_sub(values[right as usize])
                }
                Node::Multiply(left, right) => {
                    values[left as usize].wrapping_mul(values[right as usize])
                }
                Node::And(left, right) => values[left as usize] & values[right as usize],
                Node::Or(left, right) => values[left as usize] | values[right as usize],
                Node::Not(value) => !values[value as usize],
                Node::ShiftLeft(value, amount) => {
                    values[value as usize].wrapping_shl(amount.into())
                }
                Node::ShiftRight(value, amount) => {
                    values[value as usize].wrapping_shr(amount.into())
                }
                Node::RotateRight(value, amount) => {
                    values[value as usize].rotate_right(amount.into())
                }
                Node::Select(condition, nonzero, zero) => {
                    if values[condition as usize] == 0 {
                        values[zero as usize]
                    } else {
                        values[nonzero as usize]
                    }
                }
            };
            values.push(value);
        }
        values.last().copied().expect("Expression is never empty")
    }

    fn evaluate_all(&self) -> TruthTable {
        let last_use = self.last_uses();
        let mut slots = Vec::<TruthTable>::with_capacity(
            usize::try_from(self.peak_live_temporaries())
                .expect("live temporaries cannot exceed the usize-sized node collection"),
        );
        let mut node_slots = Vec::<usize>::with_capacity(self.nodes.len());
        let mut free_slots = Vec::new();
        for (index, node) in self.nodes.iter().copied().enumerate() {
            let value_at = |node: u32| &slots[node_slots[node as usize]];
            let value = match node {
                Node::Input => std::array::from_fn(|input| {
                    u8::try_from(input).expect("truth table has exactly 256 lanes")
                }),
                Node::Constant(value) => [value; 256],
                Node::Xor(left, right) => {
                    std::array::from_fn(|lane| value_at(left)[lane] ^ value_at(right)[lane])
                }
                Node::Add(left, right) => std::array::from_fn(|lane| {
                    value_at(left)[lane].wrapping_add(value_at(right)[lane])
                }),
                Node::RotateLeft(value, amount) => {
                    std::array::from_fn(|lane| value_at(value)[lane].rotate_left(amount.into()))
                }
                Node::Subtract(left, right) => std::array::from_fn(|lane| {
                    value_at(left)[lane].wrapping_sub(value_at(right)[lane])
                }),
                Node::Multiply(left, right) => std::array::from_fn(|lane| {
                    value_at(left)[lane].wrapping_mul(value_at(right)[lane])
                }),
                Node::And(left, right) => {
                    std::array::from_fn(|lane| value_at(left)[lane] & value_at(right)[lane])
                }
                Node::Or(left, right) => {
                    std::array::from_fn(|lane| value_at(left)[lane] | value_at(right)[lane])
                }
                Node::Not(value) => std::array::from_fn(|lane| !value_at(value)[lane]),
                Node::ShiftLeft(value, amount) => {
                    std::array::from_fn(|lane| value_at(value)[lane].wrapping_shl(amount.into()))
                }
                Node::ShiftRight(value, amount) => {
                    std::array::from_fn(|lane| value_at(value)[lane].wrapping_shr(amount.into()))
                }
                Node::RotateRight(value, amount) => {
                    std::array::from_fn(|lane| value_at(value)[lane].rotate_right(amount.into()))
                }
                Node::Select(condition, nonzero, zero) => std::array::from_fn(|lane| {
                    if value_at(condition)[lane] == 0 {
                        value_at(zero)[lane]
                    } else {
                        value_at(nonzero)[lane]
                    }
                }),
            };
            let mut released = [None; 3];
            for operand in node_operands(node).into_iter().flatten() {
                if last_use[operand as usize] == index && !released.contains(&Some(operand)) {
                    free_slots.push(node_slots[operand as usize]);
                    if let Some(slot) = released.iter_mut().find(|slot| slot.is_none()) {
                        *slot = Some(operand);
                    }
                }
            }
            let slot = free_slots.pop().unwrap_or_else(|| {
                slots.push([0; 256]);
                slots.len() - 1
            });
            slots[slot] = value;
            node_slots.push(slot);
        }
        slots[*node_slots.last().expect("Expression is never empty")]
    }

    fn depth(&self) -> u64 {
        let mut depths: Vec<u64> = Vec::with_capacity(self.nodes.len());
        for node in &self.nodes {
            let depth = match *node {
                Node::Input | Node::Constant(_) => 1,
                Node::Xor(left, right)
                | Node::Add(left, right)
                | Node::Subtract(left, right)
                | Node::Multiply(left, right)
                | Node::And(left, right)
                | Node::Or(left, right) => 1 + depths[left as usize].max(depths[right as usize]),
                Node::RotateLeft(value, _)
                | Node::RotateRight(value, _)
                | Node::ShiftLeft(value, _)
                | Node::ShiftRight(value, _)
                | Node::Not(value) => 1 + depths[value as usize],
                Node::Select(condition, nonzero, zero) => {
                    1 + depths[condition as usize]
                        .max(depths[nonzero as usize])
                        .max(depths[zero as usize])
                }
            };
            depths.push(depth);
        }
        depths.last().copied().unwrap_or(0)
    }

    fn peak_live_temporaries(&self) -> u64 {
        let last_use = self.last_uses();
        let mut live = 0_u64;
        let mut peak = 0_u64;
        for (index, node) in self.nodes.iter().copied().enumerate() {
            live += 1;
            peak = peak.max(live);
            let mut released = [None; 3];
            for operand in node_operands(node).into_iter().flatten() {
                if last_use[operand as usize] == index && !released.contains(&Some(operand)) {
                    live -= 1;
                    if let Some(slot) = released.iter_mut().find(|slot| slot.is_none()) {
                        *slot = Some(operand);
                    }
                }
            }
        }
        peak
    }

    fn last_uses(&self) -> Vec<usize> {
        let mut last_use = vec![0_usize; self.nodes.len()];
        for (index, node) in self.nodes.iter().copied().enumerate() {
            for operand in node_operands(node).into_iter().flatten() {
                last_use[operand as usize] = index;
            }
        }
        last_use
    }

    fn simplify_root(&self) -> Option<Self> {
        let root = *self.nodes.last()?;
        match root {
            Node::Xor(left, right) | Node::Add(left, right) | Node::Or(left, right) => {
                match (self.nodes[left as usize], self.nodes[right as usize]) {
                    (_, Node::Constant(0)) => Some(self.subexpression(left)),
                    (Node::Constant(0), _) => Some(self.subexpression(right)),
                    _ if left == right && matches!(root, Node::Xor(_, _)) => {
                        Some(Self::constant(0))
                    }
                    _ if left == right && matches!(root, Node::Or(_, _)) => {
                        Some(self.subexpression(left))
                    }
                    _ => None,
                }
            }
            Node::Subtract(left, right) => {
                if left == right {
                    Some(Self::constant(0))
                } else if self.nodes[right as usize] == Node::Constant(0) {
                    Some(self.subexpression(left))
                } else {
                    None
                }
            }
            Node::Multiply(left, right) => {
                match (self.nodes[left as usize], self.nodes[right as usize]) {
                    (_, Node::Constant(1)) => Some(self.subexpression(left)),
                    (Node::Constant(1), _) => Some(self.subexpression(right)),
                    (_, Node::Constant(0)) | (Node::Constant(0), _) => Some(Self::constant(0)),
                    _ => None,
                }
            }
            Node::And(left, right) => match (self.nodes[left as usize], self.nodes[right as usize])
            {
                (_, Node::Constant(u8::MAX)) => Some(self.subexpression(left)),
                (Node::Constant(u8::MAX), _) => Some(self.subexpression(right)),
                (_, Node::Constant(0)) | (Node::Constant(0), _) => Some(Self::constant(0)),
                _ if left == right => Some(self.subexpression(left)),
                _ => None,
            },
            Node::Not(value) => match self.nodes[value as usize] {
                Node::Not(inner) => Some(self.subexpression(inner)),
                _ => None,
            },
            Node::RotateLeft(value, 0)
            | Node::RotateRight(value, 0)
            | Node::ShiftLeft(value, 0)
            | Node::ShiftRight(value, 0) => Some(self.subexpression(value)),
            Node::Select(_, nonzero, zero) if nonzero == zero => Some(self.subexpression(nonzero)),
            _ => None,
        }
    }

    fn fold_constant_root(&self) -> Option<Self> {
        let root_index = u32::try_from(self.nodes.len() - 1).ok()?;
        if matches!(self.nodes[root_index as usize], Node::Constant(_)) {
            return None;
        }
        let value = self.evaluate(0);
        let input_free = self.nodes.iter().all(|node| !matches!(node, Node::Input));
        input_free.then(|| Self::constant(value))
    }

    fn reassociate_constants_root(&self) -> Option<Self> {
        match *self.nodes.last()? {
            Node::Xor(left, right) => {
                let Node::Constant(outer) = self.nodes[right as usize] else {
                    return None;
                };
                let Node::Xor(value, inner) = self.nodes[left as usize] else {
                    return None;
                };
                let Node::Constant(inner) = self.nodes[inner as usize] else {
                    return None;
                };
                Some(Self::xor(
                    self.subexpression(value),
                    Self::constant(inner ^ outer),
                ))
            }
            Node::Add(left, right) => {
                let Node::Constant(outer) = self.nodes[right as usize] else {
                    return None;
                };
                let Node::Add(value, inner) = self.nodes[left as usize] else {
                    return None;
                };
                let Node::Constant(inner) = self.nodes[inner as usize] else {
                    return None;
                };
                Some(Self::wrapping_add(
                    self.subexpression(value),
                    Self::constant(inner.wrapping_add(outer)),
                ))
            }
            Node::RotateLeft(value, outer) => {
                let Node::RotateLeft(value, inner) = self.nodes[value as usize] else {
                    return None;
                };
                Some(Self::rotate_left(
                    self.subexpression(value),
                    inner.wrapping_add(outer),
                ))
            }
            Node::RotateRight(value, outer) => {
                let Node::RotateRight(value, inner) = self.nodes[value as usize] else {
                    return None;
                };
                Some(Self::rotate_right(
                    self.subexpression(value),
                    inner.wrapping_add(outer),
                ))
            }
            _ => None,
        }
    }

    fn commute_root(&self) -> Option<Self> {
        match *self.nodes.last()? {
            Node::Xor(left, right)
                if matches!(self.nodes[left as usize], Node::Constant(_))
                    && !matches!(self.nodes[right as usize], Node::Constant(_)) =>
            {
                Some(Self::xor(
                    self.subexpression(right),
                    self.subexpression(left),
                ))
            }
            Node::Add(left, right)
                if matches!(self.nodes[left as usize], Node::Constant(_))
                    && !matches!(self.nodes[right as usize], Node::Constant(_)) =>
            {
                Some(Self::wrapping_add(
                    self.subexpression(right),
                    self.subexpression(left),
                ))
            }
            Node::Multiply(left, right)
                if matches!(self.nodes[left as usize], Node::Constant(_))
                    && !matches!(self.nodes[right as usize], Node::Constant(_)) =>
            {
                Some(Self::wrapping_multiply(
                    self.subexpression(right),
                    self.subexpression(left),
                ))
            }
            Node::And(left, right)
                if matches!(self.nodes[left as usize], Node::Constant(_))
                    && !matches!(self.nodes[right as usize], Node::Constant(_)) =>
            {
                Some(Self::bitwise_and(
                    self.subexpression(right),
                    self.subexpression(left),
                ))
            }
            Node::Or(left, right)
                if matches!(self.nodes[left as usize], Node::Constant(_))
                    && !matches!(self.nodes[right as usize], Node::Constant(_)) =>
            {
                Some(Self::bitwise_or(
                    self.subexpression(right),
                    self.subexpression(left),
                ))
            }
            _ => None,
        }
    }

    fn collapse_constant_function(&self) -> Option<Self> {
        let values = self.evaluate_all();
        values
            .iter()
            .all(|value| *value == values[0])
            .then(|| Self::constant(values[0]))
    }

    fn probes_zero_from_nonzero_xor_constant(&self) -> bool {
        let Some(Node::Xor(left, right)) = self.nodes.last().copied() else {
            return false;
        };
        matches!(self.nodes[left as usize], Node::Constant(value) if value != 0)
            || matches!(self.nodes[right as usize], Node::Constant(value) if value != 0)
    }

    fn apply_operator_at(&self, node: u32, operator: PrimitiveOperator) -> Option<Self> {
        let selected = self.subexpression(node);
        let replacement = match operator {
            PrimitiveOperator::SimplifyKnownIdentity => selected.simplify_root(),
            PrimitiveOperator::FoldConstant => selected.fold_constant_root(),
            PrimitiveOperator::ReassociateConstants => selected.reassociate_constants_root(),
            PrimitiveOperator::NormalizeCommutativeConstant => selected.commute_root(),
            PrimitiveOperator::CollapseConstantFunction => selected.collapse_constant_function(),
            PrimitiveOperator::ProbeZero if selected.probes_zero_from_nonzero_xor_constant() => {
                Some(Self::constant(0))
            }
            PrimitiveOperator::ProbeZero => None,
        }?;
        let candidate = self.replace_subexpression(node, &replacement);
        (candidate != *self).then_some(candidate)
    }

    fn subexpression(&self, root: u32) -> Self {
        fn copy_node(
            source: &Expression,
            id: u32,
            output: &mut Vec<Node>,
            copied: &mut HashMap<u32, u32>,
        ) -> u32 {
            if let Some(copied) = copied.get(&id) {
                return *copied;
            }
            let node = match source.nodes[id as usize] {
                Node::Input => Node::Input,
                Node::Constant(value) => Node::Constant(value),
                Node::Xor(left, right) => {
                    let new_left = copy_node(source, left, output, copied);
                    let new_right = copy_node(source, right, output, copied);
                    Node::Xor(new_left, new_right)
                }
                Node::Add(left, right) => {
                    let new_left = copy_node(source, left, output, copied);
                    let new_right = copy_node(source, right, output, copied);
                    Node::Add(new_left, new_right)
                }
                Node::RotateLeft(value, amount) => {
                    let new_value = copy_node(source, value, output, copied);
                    Node::RotateLeft(new_value, amount)
                }
                Node::Subtract(left, right) => {
                    let new_left = copy_node(source, left, output, copied);
                    let new_right = copy_node(source, right, output, copied);
                    Node::Subtract(new_left, new_right)
                }
                Node::Multiply(left, right) => {
                    let new_left = copy_node(source, left, output, copied);
                    let new_right = copy_node(source, right, output, copied);
                    Node::Multiply(new_left, new_right)
                }
                Node::And(left, right) => {
                    let new_left = copy_node(source, left, output, copied);
                    let new_right = copy_node(source, right, output, copied);
                    Node::And(new_left, new_right)
                }
                Node::Or(left, right) => {
                    let new_left = copy_node(source, left, output, copied);
                    let new_right = copy_node(source, right, output, copied);
                    Node::Or(new_left, new_right)
                }
                Node::Not(value) => Node::Not(copy_node(source, value, output, copied)),
                Node::ShiftLeft(value, amount) => {
                    Node::ShiftLeft(copy_node(source, value, output, copied), amount)
                }
                Node::ShiftRight(value, amount) => {
                    Node::ShiftRight(copy_node(source, value, output, copied), amount)
                }
                Node::RotateRight(value, amount) => {
                    Node::RotateRight(copy_node(source, value, output, copied), amount)
                }
                Node::Select(condition, nonzero, zero) => Node::Select(
                    copy_node(source, condition, output, copied),
                    copy_node(source, nonzero, output, copied),
                    copy_node(source, zero, output, copied),
                ),
            };
            let new_id = u32::try_from(output.len()).expect("expression exceeds u32 node IDs");
            output.push(node);
            copied.insert(id, new_id);
            new_id
        }

        let mut nodes = Vec::new();
        copy_node(self, root, &mut nodes, &mut HashMap::new());
        Self { nodes }
    }

    fn replace_subexpression(&self, target: u32, replacement: &Self) -> Self {
        fn rebuild(
            source: &Expression,
            id: u32,
            target: u32,
            replacement: &Expression,
            output: &mut Vec<Node>,
            interned: &mut HashMap<Node, u32>,
            rebuilt: &mut HashMap<u32, u32>,
        ) -> u32 {
            if let Some(existing) = rebuilt.get(&id) {
                return *existing;
            }
            let new_id = if id == target {
                Expression::append_expression(output, interned, replacement.clone())
            } else {
                let remap = |child,
                             output: &mut Vec<Node>,
                             interned: &mut HashMap<Node, u32>,
                             rebuilt: &mut HashMap<u32, u32>| {
                    rebuild(
                        source,
                        child,
                        target,
                        replacement,
                        output,
                        interned,
                        rebuilt,
                    )
                };
                let node = match source.nodes[id as usize] {
                    Node::Input => Node::Input,
                    Node::Constant(value) => Node::Constant(value),
                    Node::Xor(left, right) => Node::Xor(
                        remap(left, output, interned, rebuilt),
                        remap(right, output, interned, rebuilt),
                    ),
                    Node::Add(left, right) => Node::Add(
                        remap(left, output, interned, rebuilt),
                        remap(right, output, interned, rebuilt),
                    ),
                    Node::RotateLeft(value, amount) => {
                        Node::RotateLeft(remap(value, output, interned, rebuilt), amount)
                    }
                    Node::Subtract(left, right) => Node::Subtract(
                        remap(left, output, interned, rebuilt),
                        remap(right, output, interned, rebuilt),
                    ),
                    Node::Multiply(left, right) => Node::Multiply(
                        remap(left, output, interned, rebuilt),
                        remap(right, output, interned, rebuilt),
                    ),
                    Node::And(left, right) => Node::And(
                        remap(left, output, interned, rebuilt),
                        remap(right, output, interned, rebuilt),
                    ),
                    Node::Or(left, right) => Node::Or(
                        remap(left, output, interned, rebuilt),
                        remap(right, output, interned, rebuilt),
                    ),
                    Node::Not(value) => Node::Not(remap(value, output, interned, rebuilt)),
                    Node::ShiftLeft(value, amount) => {
                        Node::ShiftLeft(remap(value, output, interned, rebuilt), amount)
                    }
                    Node::ShiftRight(value, amount) => {
                        Node::ShiftRight(remap(value, output, interned, rebuilt), amount)
                    }
                    Node::RotateRight(value, amount) => {
                        Node::RotateRight(remap(value, output, interned, rebuilt), amount)
                    }
                    Node::Select(condition, nonzero, zero) => Node::Select(
                        remap(condition, output, interned, rebuilt),
                        remap(nonzero, output, interned, rebuilt),
                        remap(zero, output, interned, rebuilt),
                    ),
                };
                if let Some(existing) = interned.get(&node) {
                    *existing
                } else {
                    let new_id =
                        u32::try_from(output.len()).expect("expression exceeds u32 node IDs");
                    output.push(node);
                    interned.insert(node, new_id);
                    new_id
                }
            };
            rebuilt.insert(id, new_id);
            new_id
        }

        let mut nodes = Vec::new();
        let mut interned = HashMap::new();
        let root = rebuild(
            self,
            u32::try_from(self.nodes.len() - 1).expect("expression exceeds u32 node IDs"),
            target,
            replacement,
            &mut nodes,
            &mut interned,
            &mut HashMap::new(),
        );
        Self { nodes }.subexpression(root)
    }

    fn encode(&self, output: &mut Vec<u8>) {
        let node_count = u32::try_from(self.nodes.len()).expect("expression exceeds u32 node IDs");
        output.extend_from_slice(&node_count.to_le_bytes());
        for node in &self.nodes {
            match *node {
                Node::Input => output.push(0),
                Node::Constant(value) => {
                    output.push(1);
                    output.push(value);
                }
                Node::Xor(left, right) => {
                    output.push(2);
                    output.extend_from_slice(&left.to_le_bytes());
                    output.extend_from_slice(&right.to_le_bytes());
                }
                Node::Add(left, right) => {
                    output.push(3);
                    output.extend_from_slice(&left.to_le_bytes());
                    output.extend_from_slice(&right.to_le_bytes());
                }
                Node::RotateLeft(value, amount) => {
                    output.push(4);
                    output.extend_from_slice(&value.to_le_bytes());
                    output.push(amount);
                }
                Node::Subtract(left, right) => encode_binary_node(output, 5, left, right),
                Node::Multiply(left, right) => encode_binary_node(output, 6, left, right),
                Node::And(left, right) => encode_binary_node(output, 7, left, right),
                Node::Or(left, right) => encode_binary_node(output, 8, left, right),
                Node::Not(value) => encode_unary_node(output, 9, value),
                Node::ShiftLeft(value, amount) => encode_immediate_node(output, 10, value, amount),
                Node::ShiftRight(value, amount) => {
                    encode_immediate_node(output, 11, value, amount);
                }
                Node::RotateRight(value, amount) => {
                    encode_immediate_node(output, 12, value, amount);
                }
                Node::Select(condition, nonzero, zero) => {
                    output.push(13);
                    output.extend_from_slice(&condition.to_le_bytes());
                    output.extend_from_slice(&nonzero.to_le_bytes());
                    output.extend_from_slice(&zero.to_le_bytes());
                }
            }
        }
    }

    fn encoded_len(&self) -> usize {
        4 + self
            .nodes
            .iter()
            .map(|node| match node {
                Node::Input => 1,
                Node::Constant(_) => 2,
                Node::Xor(_, _)
                | Node::Add(_, _)
                | Node::Subtract(_, _)
                | Node::Multiply(_, _)
                | Node::And(_, _)
                | Node::Or(_, _) => 9,
                Node::RotateLeft(_, _)
                | Node::RotateRight(_, _)
                | Node::ShiftLeft(_, _)
                | Node::ShiftRight(_, _) => 6,
                Node::Not(_) => 5,
                Node::Select(_, _, _) => 13,
            })
            .sum::<usize>()
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the canonical decoder keeps every stable node tag in one auditable table"
    )]
    fn decode(mut bytes: &[u8]) -> Result<Self, BitVecError> {
        let count = read_u32(&mut bytes)? as usize;
        if count == 0 || count > bytes.len() {
            return Err(BitVecError::InvalidEncoding);
        }
        let mut nodes = Vec::with_capacity(count);
        let mut seen = HashSet::with_capacity(count);
        for index in 0..count {
            let tag = take(&mut bytes, 1)?[0];
            let node = match tag {
                0 => Node::Input,
                1 => Node::Constant(take(&mut bytes, 1)?[0]),
                2 => {
                    let left = read_u32(&mut bytes)?;
                    let right = read_u32(&mut bytes)?;
                    if left as usize >= index || right as usize >= index {
                        return Err(BitVecError::InvalidEncoding);
                    }
                    Node::Xor(left, right)
                }
                3 => {
                    let left = read_u32(&mut bytes)?;
                    let right = read_u32(&mut bytes)?;
                    if left as usize >= index || right as usize >= index {
                        return Err(BitVecError::InvalidEncoding);
                    }
                    Node::Add(left, right)
                }
                4 => {
                    let value = read_u32(&mut bytes)?;
                    let amount = take(&mut bytes, 1)?[0];
                    if value as usize >= index || amount >= U8_ROTATIONS {
                        return Err(BitVecError::InvalidEncoding);
                    }
                    Node::RotateLeft(value, amount)
                }
                5 => decode_binary_node(&mut bytes, index, Node::Subtract)?,
                6 => decode_binary_node(&mut bytes, index, Node::Multiply)?,
                7 => decode_binary_node(&mut bytes, index, Node::And)?,
                8 => decode_binary_node(&mut bytes, index, Node::Or)?,
                9 => {
                    let value = read_u32(&mut bytes)?;
                    if value as usize >= index {
                        return Err(BitVecError::InvalidEncoding);
                    }
                    Node::Not(value)
                }
                10 => decode_immediate_node(&mut bytes, index, Node::ShiftLeft)?,
                11 => decode_immediate_node(&mut bytes, index, Node::ShiftRight)?,
                12 => decode_immediate_node(&mut bytes, index, Node::RotateRight)?,
                13 => {
                    let condition = read_u32(&mut bytes)?;
                    let nonzero = read_u32(&mut bytes)?;
                    let zero = read_u32(&mut bytes)?;
                    if [condition, nonzero, zero]
                        .iter()
                        .any(|operand| *operand as usize >= index)
                    {
                        return Err(BitVecError::InvalidEncoding);
                    }
                    Node::Select(condition, nonzero, zero)
                }
                _ => return Err(BitVecError::InvalidEncoding),
            };
            if !seen.insert(node) {
                return Err(BitVecError::InvalidEncoding);
            }
            nodes.push(node);
        }
        if !bytes.is_empty() {
            return Err(BitVecError::InvalidEncoding);
        }
        let mut reachable = vec![false; nodes.len()];
        let mut pending = vec![nodes.len() - 1];
        while let Some(index) = pending.pop() {
            if std::mem::replace(&mut reachable[index], true) {
                continue;
            }
            match nodes[index] {
                Node::Input | Node::Constant(_) => {}
                Node::Xor(left, right)
                | Node::Add(left, right)
                | Node::Subtract(left, right)
                | Node::Multiply(left, right)
                | Node::And(left, right)
                | Node::Or(left, right) => {
                    pending.push(left as usize);
                    pending.push(right as usize);
                }
                Node::RotateLeft(value, _)
                | Node::RotateRight(value, _)
                | Node::ShiftLeft(value, _)
                | Node::ShiftRight(value, _)
                | Node::Not(value) => pending.push(value as usize),
                Node::Select(condition, nonzero, zero) => {
                    pending.push(condition as usize);
                    pending.push(nonzero as usize);
                    pending.push(zero as usize);
                }
            }
        }
        if reachable.contains(&false) {
            return Err(BitVecError::InvalidEncoding);
        }
        Ok(Self { nodes })
    }
}

fn encode_binary_node(output: &mut Vec<u8>, tag: u8, left: u32, right: u32) {
    output.push(tag);
    output.extend_from_slice(&left.to_le_bytes());
    output.extend_from_slice(&right.to_le_bytes());
}

fn node_operands(node: Node) -> [Option<u32>; 3] {
    match node {
        Node::Input | Node::Constant(_) => [None, None, None],
        Node::Xor(left, right)
        | Node::Add(left, right)
        | Node::Subtract(left, right)
        | Node::Multiply(left, right)
        | Node::And(left, right)
        | Node::Or(left, right) => [Some(left), Some(right), None],
        Node::Not(value)
        | Node::ShiftLeft(value, _)
        | Node::ShiftRight(value, _)
        | Node::RotateLeft(value, _)
        | Node::RotateRight(value, _) => [Some(value), None, None],
        Node::Select(condition, nonzero, zero) => [Some(condition), Some(nonzero), Some(zero)],
    }
}

fn encode_unary_node(output: &mut Vec<u8>, tag: u8, value: u32) {
    output.push(tag);
    output.extend_from_slice(&value.to_le_bytes());
}

fn encode_immediate_node(output: &mut Vec<u8>, tag: u8, value: u32, amount: u8) {
    encode_unary_node(output, tag, value);
    output.push(amount);
}

fn decode_binary_node(
    bytes: &mut &[u8],
    index: usize,
    constructor: fn(u32, u32) -> Node,
) -> Result<Node, BitVecError> {
    let left = read_u32(bytes)?;
    let right = read_u32(bytes)?;
    if left as usize >= index || right as usize >= index {
        return Err(BitVecError::InvalidEncoding);
    }
    Ok(constructor(left, right))
}

fn decode_immediate_node(
    bytes: &mut &[u8],
    index: usize,
    constructor: fn(u32, u8) -> Node,
) -> Result<Node, BitVecError> {
    let value = read_u32(bytes)?;
    let amount = take(bytes, 1)?[0];
    if value as usize >= index || amount >= U8_ROTATIONS {
        return Err(BitVecError::InvalidEncoding);
    }
    Ok(constructor(value, amount))
}

fn take<'a>(bytes: &mut &'a [u8], count: usize) -> Result<&'a [u8], BitVecError> {
    if bytes.len() < count {
        return Err(BitVecError::InvalidEncoding);
    }
    let (value, remainder) = bytes.split_at(count);
    *bytes = remainder;
    Ok(value)
}

fn read_u32(bytes: &mut &[u8]) -> Result<u32, BitVecError> {
    let value = take(bytes, 4)?;
    Ok(u32::from_le_bytes(value.try_into().unwrap()))
}

#[derive(Clone, Debug)]
pub struct SeedScope {
    entries: Arc<[SeedEntry]>,
}

#[derive(Clone, Debug)]
struct SeedEntry {
    expression: Expression,
    provenance: Vec<u8>,
}

impl SeedScope {
    #[must_use]
    pub fn new(expressions: NonEmpty<Expression>) -> Self {
        Self {
            entries: expressions
                .into_vec()
                .into_iter()
                .map(|expression| SeedEntry {
                    expression,
                    provenance: b"caller-seed".to_vec(),
                })
                .collect::<Vec<_>>()
                .into(),
        }
    }

    #[must_use]
    pub fn one(expression: Expression) -> Self {
        Self::new(NonEmpty::one(expression))
    }

    #[must_use]
    pub fn curated() -> Self {
        let entries = [
            Expression::xor(Expression::input(), Expression::constant(0)),
            Expression::wrapping_add(Expression::input(), Expression::constant(0)),
            Expression::rotate_left(Expression::input(), 0),
        ]
        .into_iter()
        .enumerate()
        .map(|(index, expression)| SeedEntry {
            expression,
            provenance: format!("reflex-bitvec/curated-v1/{index}").into_bytes(),
        })
        .collect::<Vec<_>>();
        Self {
            entries: entries.into(),
        }
    }

    #[must_use]
    pub fn generated_xor_constants() -> Self {
        let entries = (u8::MIN..=u8::MAX)
            .map(|constant| SeedEntry {
                expression: Expression::xor(Expression::input(), Expression::constant(constant)),
                provenance: format!("reflex-bitvec/generated-xor-v1/{constant}").into_bytes(),
            })
            .collect::<Vec<_>>();
        Self {
            entries: entries.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Metric {
    NodeCount,
    Depth,
    PeakLiveTemporaries,
    EncodedBytes,
    EvaluatorOperations,
    EvaluationNanoseconds,
}

#[derive(Debug)]
pub enum BitVecError {
    InvalidEncoding,
}

impl fmt::Display for BitVecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("invalid fixed-width expression encoding")
    }
}

impl std::error::Error for BitVecError {}

pub struct BitVecDomain {
    structure: ExpressionStructure,
    seeds: ExpressionSeeds,
    operators: ExpressionOperators,
    kernel: ExhaustiveKernel,
    measurements: ExpressionMeasurements,
}

impl BitVecDomain {
    #[must_use]
    pub fn unary_u8() -> Self {
        Self {
            structure: ExpressionStructure::new(),
            seeds: ExpressionSeeds,
            operators: ExpressionOperators::new(),
            kernel: ExhaustiveKernel,
            measurements: ExpressionMeasurements::new(),
        }
    }
}

impl DomainDefinition for BitVecDomain {
    type Artifact = Expression;
    type Error = BitVecError;
    type SeedScope = SeedScope;
    type Metric = Metric;
    type Observation = u64;
    type Structure = ExpressionStructure;
    type Seeds = ExpressionSeeds;
    type Operators = ExpressionOperators;
    type Kernel = ExhaustiveKernel;
    type Measurements = ExpressionMeasurements;

    fn semantic_identity(&self) -> SemanticIdentity {
        SemanticIdentity::new(
            "reflex-bitvec/u8/unary/full-ops/masked-shifts/select-nonzero/canonical-dag/v3",
        )
    }

    fn structure(&self) -> &Self::Structure {
        &self.structure
    }

    fn seeds(&self) -> &Self::Seeds {
        &self.seeds
    }

    fn operators(&self) -> &Self::Operators {
        &self.operators
    }

    fn kernel(&self) -> &Self::Kernel {
        &self.kernel
    }

    fn measurements(&self) -> &Self::Measurements {
        &self.measurements
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Sort {
    U8,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Constructor {
    Input,
    Constant,
    Xor,
    Add,
    RotateLeft,
    Subtract,
    Multiply,
    And,
    Or,
    Not,
    ShiftLeft,
    ShiftRight,
    RotateRight,
    Select,
}

pub struct ExpressionView<'a>(&'a Expression);

impl StructuralView for ExpressionView<'_> {
    type Sort = Sort;
    type Constructor = Constructor;

    fn root_sort(&self) -> Self::Sort {
        Sort::U8
    }

    fn node_count(&self) -> usize {
        self.0.node_count()
    }

    fn node_sort(&self, node: usize) -> Option<Self::Sort> {
        (node < self.0.nodes.len()).then_some(Sort::U8)
    }

    fn node_constructor(&self, node: usize) -> Option<Self::Constructor> {
        self.0.nodes.get(node).map(|node| match node {
            Node::Input => Constructor::Input,
            Node::Constant(_) => Constructor::Constant,
            Node::Xor(_, _) => Constructor::Xor,
            Node::Add(_, _) => Constructor::Add,
            Node::RotateLeft(_, _) => Constructor::RotateLeft,
            Node::Subtract(_, _) => Constructor::Subtract,
            Node::Multiply(_, _) => Constructor::Multiply,
            Node::And(_, _) => Constructor::And,
            Node::Or(_, _) => Constructor::Or,
            Node::Not(_) => Constructor::Not,
            Node::ShiftLeft(_, _) => Constructor::ShiftLeft,
            Node::ShiftRight(_, _) => Constructor::ShiftRight,
            Node::RotateRight(_, _) => Constructor::RotateRight,
            Node::Select(_, _, _) => Constructor::Select,
        })
    }

    fn write_children(&self, node: usize, output: &mut Vec<usize>) -> bool {
        output.clear();
        let Some(node) = self.0.nodes.get(node).copied() else {
            return false;
        };
        output.extend(
            node_operands(node)
                .into_iter()
                .flatten()
                .map(|child| child as usize),
        );
        true
    }

    fn write_immediates(&self, node: usize, output: &mut Vec<u64>) -> bool {
        output.clear();
        let Some(node) = self.0.nodes.get(node).copied() else {
            return false;
        };
        match node {
            Node::Constant(value) => output.push(value.into()),
            Node::RotateLeft(_, amount)
            | Node::ShiftLeft(_, amount)
            | Node::ShiftRight(_, amount)
            | Node::RotateRight(_, amount) => output.push(amount.into()),
            _ => {}
        }
        true
    }

    fn dynamic_resident_bytes(&self) -> u64 {
        (self.0.nodes.capacity() as u64).saturating_mul(std::mem::size_of::<Node>() as u64)
    }
}

pub struct ExpressionStructure {
    schema: StructuralSchema<Sort, Constructor>,
}

impl ExpressionStructure {
    fn new() -> Self {
        let descriptor = |constructor, symbol, children, immediates| {
            ConstructorDescriptor::new(
                constructor,
                SymbolId::new(symbol),
                Sort::U8,
                vec![Sort::U8; children],
                immediates,
                vec![0; children],
            )
        };
        Self {
            schema: StructuralSchema {
                sorts: vec![(Sort::U8, SymbolId::new("u8"))],
                constructors: vec![
                    descriptor(Constructor::Input, "input", 0, 0),
                    descriptor(Constructor::Constant, "constant", 0, 1),
                    descriptor(Constructor::Xor, "xor", 2, 0),
                    descriptor(Constructor::Add, "wrapping-add", 2, 0),
                    descriptor(Constructor::RotateLeft, "rotate-left", 1, 1),
                    descriptor(Constructor::Subtract, "wrapping-subtract", 2, 0),
                    descriptor(Constructor::Multiply, "wrapping-multiply", 2, 0),
                    descriptor(Constructor::And, "bitwise-and", 2, 0),
                    descriptor(Constructor::Or, "bitwise-or", 2, 0),
                    descriptor(Constructor::Not, "bitwise-not", 1, 0),
                    descriptor(Constructor::ShiftLeft, "masked-shift-left", 1, 1),
                    descriptor(Constructor::ShiftRight, "masked-shift-right", 1, 1),
                    descriptor(Constructor::RotateRight, "rotate-right", 1, 1),
                    descriptor(Constructor::Select, "select-nonzero", 3, 0),
                ],
            },
        }
    }
}

impl StructuralProtocol<BitVecDomain> for ExpressionStructure {
    type Sort = Sort;
    type Constructor = Constructor;
    type View<'a> = ExpressionView<'a>;
    type Scratch = ();

    fn schema(&self) -> &StructuralSchema<Self::Sort, Self::Constructor> {
        &self.schema
    }

    fn view<'a>(&'a self, artifact: &'a Expression) -> Self::View<'a> {
        ExpressionView(artifact)
    }

    fn compose(
        &self,
        constructor: Self::Constructor,
        children: &[&Expression],
        immediates: &[u64],
        (): &mut Self::Scratch,
    ) -> Result<Expression, BitVecError> {
        let child = |index: usize| {
            children
                .get(index)
                .map(|expression| (*expression).clone())
                .ok_or(BitVecError::InvalidEncoding)
        };
        let immediate = |index: usize, maximum: u64| {
            immediates
                .get(index)
                .copied()
                .filter(|value| *value <= maximum)
                .and_then(|value| u8::try_from(value).ok())
                .ok_or(BitVecError::InvalidEncoding)
        };
        let expression = match constructor {
            Constructor::Input if children.is_empty() && immediates.is_empty() => {
                Expression::input()
            }
            Constructor::Constant if children.is_empty() && immediates.len() == 1 => {
                Expression::constant(immediate(0, u8::MAX.into())?)
            }
            Constructor::Xor if children.len() == 2 && immediates.is_empty() => {
                Expression::xor(child(0)?, child(1)?)
            }
            Constructor::Add if children.len() == 2 && immediates.is_empty() => {
                Expression::wrapping_add(child(0)?, child(1)?)
            }
            Constructor::RotateLeft if children.len() == 1 && immediates.len() == 1 => {
                Expression::rotate_left(child(0)?, immediate(0, 7)?)
            }
            Constructor::Subtract if children.len() == 2 && immediates.is_empty() => {
                Expression::wrapping_subtract(child(0)?, child(1)?)
            }
            Constructor::Multiply if children.len() == 2 && immediates.is_empty() => {
                Expression::wrapping_multiply(child(0)?, child(1)?)
            }
            Constructor::And if children.len() == 2 && immediates.is_empty() => {
                Expression::bitwise_and(child(0)?, child(1)?)
            }
            Constructor::Or if children.len() == 2 && immediates.is_empty() => {
                Expression::bitwise_or(child(0)?, child(1)?)
            }
            Constructor::Not if children.len() == 1 && immediates.is_empty() => {
                Expression::bitwise_not(child(0)?)
            }
            Constructor::ShiftLeft if children.len() == 1 && immediates.len() == 1 => {
                Expression::shift_left(child(0)?, immediate(0, 7)?)
            }
            Constructor::ShiftRight if children.len() == 1 && immediates.len() == 1 => {
                Expression::shift_right(child(0)?, immediate(0, 7)?)
            }
            Constructor::RotateRight if children.len() == 1 && immediates.len() == 1 => {
                Expression::rotate_right(child(0)?, immediate(0, 7)?)
            }
            Constructor::Select if children.len() == 3 && immediates.is_empty() => {
                Expression::select(child(0)?, child(1)?, child(2)?)
            }
            _ => return Err(BitVecError::InvalidEncoding),
        };
        Ok(expression)
    }

    fn extract(
        &self,
        artifact: &Expression,
        node: usize,
        (): &mut Self::Scratch,
    ) -> Result<Expression, BitVecError> {
        let node = u32::try_from(node).map_err(|_| BitVecError::InvalidEncoding)?;
        ((node as usize) < artifact.nodes.len())
            .then(|| artifact.subexpression(node))
            .ok_or(BitVecError::InvalidEncoding)
    }

    fn replace(
        &self,
        artifact: &Expression,
        node: usize,
        replacement: &Expression,
        (): &mut Self::Scratch,
    ) -> Result<Expression, BitVecError> {
        let node = u32::try_from(node).map_err(|_| BitVecError::InvalidEncoding)?;
        ((node as usize) < artifact.nodes.len())
            .then(|| artifact.replace_subexpression(node, replacement))
            .ok_or(BitVecError::InvalidEncoding)
    }

    fn encode_canonical(
        &self,
        artifact: &Expression,
        output: &mut Vec<u8>,
        (): &mut Self::Scratch,
    ) -> Result<(), BitVecError> {
        artifact.encode(output);
        Ok(())
    }

    fn decode_canonical(
        &self,
        bytes: &[u8],
        (): &mut Self::Scratch,
    ) -> Result<Expression, BitVecError> {
        Expression::decode(bytes)
    }
}

pub struct SeedCursor {
    entries: Arc<[SeedEntry]>,
    next: usize,
}

pub struct ExpressionSeeds;

impl SeedSource<BitVecDomain> for ExpressionSeeds {
    type Cursor = SeedCursor;
    type Scratch = ();

    fn open(&self, scope: &SeedScope) -> Result<Self::Cursor, BitVecError> {
        Ok(SeedCursor {
            entries: Arc::clone(&scope.entries),
            next: 0,
        })
    }

    fn read_batch(
        &self,
        cursor: &mut Self::Cursor,
        limit: usize,
        output: &mut SeedWriter<'_, BitVecDomain>,
        (): &mut Self::Scratch,
    ) -> Result<SeedPage, BitVecError> {
        let start = cursor.next;
        let end = cursor.entries.len().min(start.saturating_add(limit));
        for entry in &cursor.entries[start..end] {
            let artifact = entry.expression.clone();
            let truth = truth_table(&artifact);
            output.push(Seed {
                artifact,
                verification: VerificationRecord {
                    claim: truth,
                    evidence: truth,
                    kernel_revision: KernelRevision(2),
                },
                provenance: entry.provenance.clone(),
            });
        }
        cursor.next = end;
        Ok(SeedPage {
            emitted: end - start,
            exhausted: end == cursor.entries.len(),
        })
    }

    fn encode_scope(&self, scope: &SeedScope, output: &mut Vec<u8>) -> Result<(), BitVecError> {
        let expression_count =
            u32::try_from(scope.entries.len()).map_err(|_| BitVecError::InvalidEncoding)?;
        output.extend_from_slice(&expression_count.to_le_bytes());
        for entry in scope.entries.iter() {
            let mut encoded = Vec::new();
            entry.expression.encode(&mut encoded);
            let encoded_len =
                u32::try_from(encoded.len()).map_err(|_| BitVecError::InvalidEncoding)?;
            output.extend_from_slice(&encoded_len.to_le_bytes());
            output.extend_from_slice(&encoded);
            let provenance_len =
                u32::try_from(entry.provenance.len()).map_err(|_| BitVecError::InvalidEncoding)?;
            output.extend_from_slice(&provenance_len.to_le_bytes());
            output.extend_from_slice(&entry.provenance);
        }
        Ok(())
    }

    fn decode_scope(&self, mut bytes: &[u8]) -> Result<SeedScope, BitVecError> {
        let count = read_u32(&mut bytes)? as usize;
        if count == 0 || count > bytes.len().saturating_div(MIN_ENCODED_SEED_BYTES) {
            return Err(BitVecError::InvalidEncoding);
        }
        let mut entries = Vec::with_capacity(count);
        for _ in 0..count {
            let length = read_u32(&mut bytes)? as usize;
            let expression = Expression::decode(take(&mut bytes, length)?)?;
            let provenance_length = read_u32(&mut bytes)? as usize;
            let provenance = take(&mut bytes, provenance_length)?.to_vec();
            if provenance.is_empty() {
                return Err(BitVecError::InvalidEncoding);
            }
            entries.push(SeedEntry {
                expression,
                provenance,
            });
        }
        if !bytes.is_empty() {
            return Err(BitVecError::InvalidEncoding);
        }
        Ok(SeedScope {
            entries: entries.into(),
        })
    }

    fn encode_cursor(
        &self,
        cursor: &Self::Cursor,
        output: &mut Vec<u8>,
    ) -> Result<(), BitVecError> {
        self.encode_scope(
            &SeedScope {
                entries: Arc::clone(&cursor.entries),
            },
            output,
        )?;
        output.extend_from_slice(&(cursor.next as u64).to_le_bytes());
        Ok(())
    }

    fn decode_cursor(&self, bytes: &[u8]) -> Result<Self::Cursor, BitVecError> {
        if bytes.len() < 8 {
            return Err(BitVecError::InvalidEncoding);
        }
        let split = bytes.len() - 8;
        let scope = self.decode_scope(&bytes[..split])?;
        let next = usize::try_from(u64::from_le_bytes(bytes[split..].try_into().unwrap()))
            .map_err(|_| BitVecError::InvalidEncoding)?;
        if next > scope.entries.len() {
            return Err(BitVecError::InvalidEncoding);
        }
        Ok(SeedCursor {
            entries: scope.entries,
            next,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PrimitiveOperator {
    NormalizeCommutativeConstant,
    SimplifyKnownIdentity,
    FoldConstant,
    ReassociateConstants,
    CollapseConstantFunction,
    ProbeZero,
}

#[derive(Clone)]
pub struct Application {
    source_index: usize,
    candidate: Expression,
}

pub struct ExpressionOperators {
    catalog: Vec<OperatorDescriptor<PrimitiveOperator>>,
}

impl ExpressionOperators {
    fn new() -> Self {
        Self {
            catalog: vec![
                OperatorDescriptor::new(PrimitiveOperator::ProbeZero, SymbolId::new("probe-zero")),
                OperatorDescriptor::new(
                    PrimitiveOperator::SimplifyKnownIdentity,
                    SymbolId::new("simplify-known-identity"),
                ),
                OperatorDescriptor::new(
                    PrimitiveOperator::FoldConstant,
                    SymbolId::new("fold-constant"),
                ),
                OperatorDescriptor::new(
                    PrimitiveOperator::ReassociateConstants,
                    SymbolId::new("reassociate-constants"),
                ),
                OperatorDescriptor::new(
                    PrimitiveOperator::CollapseConstantFunction,
                    SymbolId::new("collapse-constant-function"),
                ),
                OperatorDescriptor::new(
                    PrimitiveOperator::NormalizeCommutativeConstant,
                    SymbolId::new("normalize-commutative-constant"),
                ),
            ],
        }
    }
}

impl OperatorAlgebra<BitVecDomain> for ExpressionOperators {
    type Operator = PrimitiveOperator;
    type Application = Application;
    type Scratch = ();

    fn catalog(&self) -> &[OperatorDescriptor<Self::Operator>] {
        &self.catalog
    }

    fn enumerate_legal(
        &self,
        requests: OperatorEnumerationBatch<'_, BitVecDomain, Self::Operator>,
        output: &mut ApplicationWriter<'_, Self::Application>,
        (): &mut Self::Scratch,
    ) -> Result<(), BitVecError> {
        for location in requests.locations() {
            for operator in requests.operators() {
                if output.is_full() {
                    return Ok(());
                }
                let Some(artifact) = requests.artifacts().get(location.artifact_index()) else {
                    return Err(BitVecError::InvalidEncoding);
                };
                let node = u32::try_from(location.node_index())
                    .map_err(|_| BitVecError::InvalidEncoding)?;
                if node as usize >= artifact.nodes.len() {
                    return Err(BitVecError::InvalidEncoding);
                }
                if let Some(candidate) = artifact.apply_operator_at(node, *operator) {
                    output.push(Application {
                        source_index: location.artifact_index(),
                        candidate,
                    });
                }
            }
        }
        Ok(())
    }

    fn apply_batch(
        &self,
        applications: &[Self::Application],
        output: &mut CandidateWriter<'_, BitVecDomain>,
        (): &mut Self::Scratch,
    ) -> Result<(), BitVecError> {
        for application in applications {
            output.push(application.source_index, application.candidate.clone());
        }
        Ok(())
    }
}

pub type TruthTable = [u8; 256];

pub struct ExhaustiveKernel;

impl VerificationKernel<BitVecDomain> for ExhaustiveKernel {
    type Claim = TruthTable;
    type Evidence = TruthTable;
    type Scratch = ();

    fn revision(&self) -> KernelRevision {
        KernelRevision(2)
    }

    fn claim_for_candidate(
        &self,
        seed: &Expression,
        _candidate: &Expression,
    ) -> Result<Self::Claim, BitVecError> {
        Ok(truth_table(seed))
    }

    fn verify_batch(
        &self,
        requests: VerificationBatch<'_, BitVecDomain, Self::Claim>,
        output: &mut VerdictWriter<'_, Self::Evidence>,
        (): &mut Self::Scratch,
    ) -> VerificationBatchOutcome<BitVecError> {
        let verdicts = requests
            .requests()
            .iter()
            .map(|request| {
                let expected = truth_table(request.seed);
                let evidence = truth_table(request.candidate);
                if &expected == request.claim && evidence == expected {
                    Verdict::Accepted { evidence }
                } else {
                    Verdict::Refuted
                }
            })
            .collect::<Vec<_>>();
        for verdict in verdicts {
            output.push(verdict);
        }
        VerificationBatchOutcome::completed(VerificationBatchReport::in_process())
    }

    fn replay_batch(
        &self,
        records: VerificationReplayBatch<'_, BitVecDomain, Self::Claim, Self::Evidence>,
        output: &mut ReplayVerdictWriter<'_>,
        (): &mut Self::Scratch,
    ) -> VerificationBatchOutcome<BitVecError> {
        let replayed = records
            .requests()
            .iter()
            .map(
                |VerificationReplayRequest {
                     artifact,
                     claim,
                     evidence,
                     kernel_revision,
                 }| {
                    let actual = truth_table(artifact);
                    *kernel_revision == self.revision() && &actual == *claim && &actual == *evidence
                },
            )
            .collect::<Vec<_>>();
        for accepted in replayed {
            output.push(accepted);
        }
        VerificationBatchOutcome::completed(VerificationBatchReport::in_process())
    }

    fn encode_claim(&self, claim: &Self::Claim, output: &mut Vec<u8>) -> Result<(), BitVecError> {
        output.extend_from_slice(claim);
        Ok(())
    }

    fn decode_claim(&self, bytes: &[u8]) -> Result<Self::Claim, BitVecError> {
        bytes.try_into().map_err(|_| BitVecError::InvalidEncoding)
    }

    fn encode_evidence(
        &self,
        evidence: &Self::Evidence,
        output: &mut Vec<u8>,
    ) -> Result<(), BitVecError> {
        output.extend_from_slice(evidence);
        Ok(())
    }

    fn decode_evidence(&self, bytes: &[u8]) -> Result<Self::Evidence, BitVecError> {
        bytes.try_into().map_err(|_| BitVecError::InvalidEncoding)
    }
}

fn truth_table(expression: &Expression) -> TruthTable {
    expression.evaluate_all()
}

pub struct ExpressionMeasurements {
    schema: Vec<MeasurementDescriptor<Metric>>,
}

impl ExpressionMeasurements {
    fn new() -> Self {
        Self {
            schema: vec![
                MeasurementDescriptor::new(Metric::NodeCount, SymbolId::new("node-count")),
                MeasurementDescriptor::new(Metric::Depth, SymbolId::new("depth")),
                MeasurementDescriptor::new(
                    Metric::PeakLiveTemporaries,
                    SymbolId::new("peak-live-temporaries"),
                ),
                MeasurementDescriptor::new(Metric::EncodedBytes, SymbolId::new("encoded-bytes")),
                MeasurementDescriptor::new(
                    Metric::EvaluatorOperations,
                    SymbolId::new("evaluator-operations"),
                ),
                MeasurementDescriptor::new(
                    Metric::EvaluationNanoseconds,
                    SymbolId::new("evaluation-nanoseconds"),
                ),
            ],
        }
    }
}

impl MeasurementSpace<BitVecDomain> for ExpressionMeasurements {
    type Metric = Metric;
    type Observation = u64;
    type Scratch = Vec<u8>;

    fn schema(&self) -> &[MeasurementDescriptor<Self::Metric>] {
        &self.schema
    }

    fn measure_batch(
        &self,
        artifacts: VerifiedBatch<'_, BitVecDomain>,
        _environment: &MeasurementEnvironment,
        output: &mut MeasurementWriter<'_, Self::Metric, Self::Observation>,
        _scratch: &mut Self::Scratch,
    ) -> Result<(), BitVecError> {
        let measured = artifacts
            .artifacts()
            .par_iter()
            .enumerate()
            .map(|(artifact_index, artifact)| {
                let started = Instant::now();
                std::hint::black_box(truth_table(artifact));
                (
                    artifact_index,
                    artifact.node_count() as u64,
                    artifact.depth(),
                    artifact.peak_live_temporaries(),
                    artifact.encoded_len() as u64,
                    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX),
                )
            })
            .collect::<Vec<_>>();
        for (
            artifact_index,
            node_count,
            depth,
            live_temporaries,
            encoded_bytes,
            evaluation_nanoseconds,
        ) in measured
        {
            output.push(artifact_index, Metric::NodeCount, node_count);
            output.push(artifact_index, Metric::Depth, depth);
            output.push(
                artifact_index,
                Metric::PeakLiveTemporaries,
                live_temporaries,
            );
            output.push(artifact_index, Metric::EncodedBytes, encoded_bytes);
            output.push(artifact_index, Metric::EvaluatorOperations, node_count);
            output.push(
                artifact_index,
                Metric::EvaluationNanoseconds,
                evaluation_nanoseconds,
            );
        }
        Ok(())
    }

    fn compare(
        &self,
        _metric: Self::Metric,
        left: &Self::Observation,
        right: &Self::Observation,
    ) -> Result<MetricOrdering, Incomparable> {
        Ok(match left.cmp(right) {
            std::cmp::Ordering::Less => MetricOrdering::Less,
            std::cmp::Ordering::Equal => MetricOrdering::Equal,
            std::cmp::Ordering::Greater => MetricOrdering::Greater,
        })
    }

    fn environments_compatible(
        &self,
        metric: Self::Metric,
        left: &MeasurementEnvironment,
        right: &MeasurementEnvironment,
    ) -> bool {
        metric != Metric::EvaluationNanoseconds || left == right
    }

    fn within_tolerance(
        &self,
        _metric: Self::Metric,
        left: &Self::Observation,
        right: &Self::Observation,
        tolerance: &Self::Observation,
    ) -> Result<bool, Incomparable> {
        Ok(left.abs_diff(*right) <= *tolerance)
    }

    fn encode_observation(
        &self,
        _metric: Self::Metric,
        observation: &Self::Observation,
        output: &mut Vec<u8>,
    ) -> Result<(), BitVecError> {
        output.extend_from_slice(&observation.to_le_bytes());
        Ok(())
    }

    fn decode_observation(
        &self,
        _metric: Self::Metric,
        bytes: &[u8],
    ) -> Result<Self::Observation, BitVecError> {
        Ok(u64::from_le_bytes(
            bytes.try_into().map_err(|_| BitVecError::InvalidEncoding)?,
        ))
    }
}
