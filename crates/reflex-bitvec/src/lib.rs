//! Fixed-width expression optimization for Reflex.

use std::collections::{HashMap, HashSet};
use std::fmt;

use reflex::domain::{
    ReplayVerdictWriter, StructuralSchema, StructuralView, SymbolId, VerificationReplayRequest,
};
use reflex::{
    ApplicationWriter, CandidateWriter, DomainDefinition, Incomparable, KernelRevision,
    MeasurementDescriptor, MeasurementEnvironment, MeasurementSpace, MeasurementWriter,
    MetricOrdering, NonEmpty, OperatorAlgebra, OperatorDescriptor, OperatorEnumerationBatch, Seed,
    SeedPage, SeedSource, SeedWriter, SemanticIdentity, StructuralProtocol, Verdict, VerdictWriter,
    VerificationBatch, VerificationKernel, VerificationRecord, VerificationReplayBatch,
    VerifiedBatch,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Expression {
    nodes: Vec<Node>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum Node {
    Input,
    Constant(u8),
    Xor(u32, u32),
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
        let mut nodes = left.nodes;
        let left_root = u32::try_from(nodes.len() - 1).expect("expression exceeds u32 node IDs");
        let mut interned = nodes
            .iter()
            .copied()
            .enumerate()
            .map(|(index, node)| {
                (
                    node,
                    u32::try_from(index).expect("expression exceeds u32 node IDs"),
                )
            })
            .collect::<HashMap<_, _>>();
        let mut right_ids = Vec::with_capacity(right.nodes.len());
        for node in right.nodes {
            let remapped = match node {
                Node::Input => Node::Input,
                Node::Constant(value) => Node::Constant(value),
                Node::Xor(left, right) => {
                    Node::Xor(right_ids[left as usize], right_ids[right as usize])
                }
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
        let right_root = *right_ids.last().expect("Expression is never empty");
        let root = Node::Xor(left_root, right_root);
        if !interned.contains_key(&root) {
            nodes.push(root);
        }
        Self { nodes }
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
        let mut values = Vec::with_capacity(self.nodes.len());
        for node in &self.nodes {
            let value = match *node {
                Node::Input => input,
                Node::Constant(value) => value,
                Node::Xor(left, right) => values[left as usize] ^ values[right as usize],
            };
            values.push(value);
        }
        values.last().copied().expect("Expression is never empty")
    }

    fn depth(&self) -> u64 {
        let mut depths: Vec<u64> = Vec::with_capacity(self.nodes.len());
        for node in &self.nodes {
            let depth = match *node {
                Node::Input | Node::Constant(_) => 1,
                Node::Xor(left, right) => 1 + depths[left as usize].max(depths[right as usize]),
            };
            depths.push(depth);
        }
        depths.last().copied().unwrap_or(0)
    }

    fn simplify_root(&self) -> Option<Self> {
        let Node::Xor(left, right) = *self.nodes.last()? else {
            return None;
        };
        match (&self.nodes[left as usize], &self.nodes[right as usize]) {
            (_, Node::Constant(0)) => Some(self.subexpression(left)),
            (Node::Constant(0), _) => Some(self.subexpression(right)),
            _ => None,
        }
    }

    fn subexpression(&self, root: u32) -> Self {
        fn copy_node(source: &Expression, id: u32, output: &mut Vec<Node>) -> u32 {
            let node = match source.nodes[id as usize] {
                Node::Input => Node::Input,
                Node::Constant(value) => Node::Constant(value),
                Node::Xor(left, right) => {
                    let new_left = copy_node(source, left, output);
                    let new_right = copy_node(source, right, output);
                    Node::Xor(new_left, new_right)
                }
            };
            let new_id = u32::try_from(output.len()).expect("expression exceeds u32 node IDs");
            output.push(node);
            new_id
        }

        let mut nodes = Vec::new();
        copy_node(self, root, &mut nodes);
        Self { nodes }
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
            }
        }
    }

    fn decode(mut bytes: &[u8]) -> Result<Self, BitVecError> {
        let count = read_u32(&mut bytes)? as usize;
        if count == 0 {
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
        Ok(Self { nodes })
    }
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
    expressions: Vec<Expression>,
}

impl SeedScope {
    #[must_use]
    pub fn new(expressions: NonEmpty<Expression>) -> Self {
        Self {
            expressions: expressions.into_vec(),
        }
    }

    #[must_use]
    pub fn one(expression: Expression) -> Self {
        Self::new(NonEmpty::one(expression))
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Metric {
    NodeCount,
    Depth,
    EncodedBytes,
    EvaluatorOperations,
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
        SemanticIdentity::new("reflex-bitvec/u8/unary/xor/v1")
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
}

pub struct ExpressionStructure {
    schema: StructuralSchema<Sort, Constructor>,
}

impl ExpressionStructure {
    fn new() -> Self {
        Self {
            schema: StructuralSchema {
                sorts: vec![(Sort::U8, SymbolId::new("u8"))],
                constructors: vec![
                    (Constructor::Input, SymbolId::new("input")),
                    (Constructor::Constant, SymbolId::new("constant")),
                    (Constructor::Xor, SymbolId::new("xor")),
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
    expressions: Vec<Expression>,
    next: usize,
}

pub struct ExpressionSeeds;

impl SeedSource<BitVecDomain> for ExpressionSeeds {
    type Cursor = SeedCursor;
    type Scratch = ();

    fn open(&self, scope: &SeedScope) -> Result<Self::Cursor, BitVecError> {
        Ok(SeedCursor {
            expressions: scope.expressions.clone(),
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
        let end = cursor.expressions.len().min(start.saturating_add(limit));
        for artifact in cursor.expressions[start..end].iter().cloned() {
            let truth = truth_table(&artifact);
            output.push(Seed {
                artifact,
                verification: VerificationRecord {
                    claim: truth,
                    evidence: truth,
                    kernel_revision: KernelRevision(1),
                },
                provenance: b"caller-seed".to_vec(),
            });
        }
        cursor.next = end;
        Ok(SeedPage {
            emitted: end - start,
            exhausted: end == cursor.expressions.len(),
        })
    }

    fn encode_scope(&self, scope: &SeedScope, output: &mut Vec<u8>) -> Result<(), BitVecError> {
        let expression_count =
            u32::try_from(scope.expressions.len()).map_err(|_| BitVecError::InvalidEncoding)?;
        output.extend_from_slice(&expression_count.to_le_bytes());
        for expression in &scope.expressions {
            let mut encoded = Vec::new();
            expression.encode(&mut encoded);
            let encoded_len =
                u32::try_from(encoded.len()).map_err(|_| BitVecError::InvalidEncoding)?;
            output.extend_from_slice(&encoded_len.to_le_bytes());
            output.extend_from_slice(&encoded);
        }
        Ok(())
    }

    fn decode_scope(&self, mut bytes: &[u8]) -> Result<SeedScope, BitVecError> {
        let count = read_u32(&mut bytes)? as usize;
        let mut expressions = Vec::with_capacity(count);
        for _ in 0..count {
            let length = read_u32(&mut bytes)? as usize;
            expressions.push(Expression::decode(take(&mut bytes, length)?)?);
        }
        if !bytes.is_empty() {
            return Err(BitVecError::InvalidEncoding);
        }
        Ok(SeedScope { expressions })
    }

    fn encode_cursor(
        &self,
        cursor: &Self::Cursor,
        output: &mut Vec<u8>,
    ) -> Result<(), BitVecError> {
        self.encode_scope(
            &SeedScope {
                expressions: cursor.expressions.clone(),
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
        if next > scope.expressions.len() {
            return Err(BitVecError::InvalidEncoding);
        }
        Ok(SeedCursor {
            expressions: scope.expressions,
            next,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PrimitiveOperator {
    SimplifyXorIdentity,
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
            catalog: vec![OperatorDescriptor::new(
                PrimitiveOperator::SimplifyXorIdentity,
                SymbolId::new("simplify-xor-identity"),
            )],
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
        if requests
            .operators()
            .contains(&PrimitiveOperator::SimplifyXorIdentity)
        {
            for (source_index, artifact) in requests.artifacts().iter().enumerate() {
                if let Some(candidate) = artifact.simplify_root() {
                    output.push(Application {
                        source_index,
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
        KernelRevision(1)
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
    ) -> Result<(), BitVecError> {
        for request in requests.requests() {
            let expected = truth_table(request.seed);
            let evidence = truth_table(request.candidate);
            if &expected == request.claim && evidence == expected {
                output.push(Verdict::Accepted { evidence });
            } else {
                output.push(Verdict::Refuted);
            }
        }
        Ok(())
    }

    fn replay_batch(
        &self,
        records: VerificationReplayBatch<'_, BitVecDomain, Self::Claim, Self::Evidence>,
        output: &mut ReplayVerdictWriter<'_>,
        (): &mut Self::Scratch,
    ) -> Result<(), BitVecError> {
        for VerificationReplayRequest {
            artifact,
            claim,
            evidence,
            kernel_revision,
        } in records.requests()
        {
            let actual = truth_table(artifact);
            output.push(
                *kernel_revision == self.revision() && &actual == *claim && &actual == *evidence,
            );
        }
        Ok(())
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
    let mut table = [0_u8; 256];
    for (input, output) in table.iter_mut().enumerate() {
        *output = expression.evaluate(u8::try_from(input).expect("truth table has 256 entries"));
    }
    table
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
                MeasurementDescriptor::new(Metric::EncodedBytes, SymbolId::new("encoded-bytes")),
                MeasurementDescriptor::new(
                    Metric::EvaluatorOperations,
                    SymbolId::new("evaluator-operations"),
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
        scratch: &mut Self::Scratch,
    ) -> Result<(), BitVecError> {
        for (artifact_index, artifact) in artifacts.artifacts().iter().enumerate() {
            output.push(
                artifact_index,
                Metric::NodeCount,
                artifact.node_count() as u64,
            );
            output.push(artifact_index, Metric::Depth, artifact.depth());
            scratch.clear();
            artifact.encode(scratch);
            output.push(artifact_index, Metric::EncodedBytes, scratch.len() as u64);
            output.push(
                artifact_index,
                Metric::EvaluatorOperations,
                artifact.node_count() as u64,
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
        _metric: Self::Metric,
        _left: &MeasurementEnvironment,
        _right: &MeasurementEnvironment,
    ) -> bool {
        true
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
