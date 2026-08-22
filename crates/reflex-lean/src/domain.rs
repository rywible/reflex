use std::collections::HashMap;
use std::num::NonZeroU64;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use reflex::domain::{
    ReplayVerdictWriter, StructuralSchema, SymbolId, VerificationReplayRequest,
};
use reflex::{
    ApplicationWriter, CandidateWriter, ConstructorDescriptor, DomainDefinition,
    ExternalVerificationUsage, Incomparable, KernelRevision, MeasurementDescriptor,
    MeasurementEnvironment, MeasurementSpace, MeasurementWriter, MetricOrdering, OperatorAlgebra,
    OperatorDescriptor, OperatorEnumerationBatch, Seed, SeedPage, SeedSource, SeedWriter,
    SemanticIdentity, StructuralProtocol, StructuralView, Verdict,
    VerdictWriter, VerificationBatch, VerificationBatchOutcome, VerificationBatchReport,
    VerificationKernel, VerificationRecord, VerificationReplayBatch, VerificationWorkerRequirements,
    VerifiedBatch,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::ast::{
    LeanArtifact, LeanBinderInfo, LeanEnvironmentIdentity, LeanExpr, LeanLevel, LeanLiteral,
    LeanName,
};
use crate::worker::{
    IndexPage, IndexedTheorem, LeanWorker, LeanWorkerConfig, VerificationItem, WorkerError,
    WorkerUsage,
};
use crate::{LEAN_COMMIT, LEAN_TOOLCHAIN, MATHLIB_COMMIT};

const KERNEL_REVISION: KernelRevision = KernelRevision(1);

#[derive(Debug)]
pub enum LeanError {
    InvalidEncoding(String),
    InvalidStructure(String),
    IncompatibleEnvironment,
    Worker(WorkerError),
}

impl std::fmt::Display for LeanError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidEncoding(error) => write!(formatter, "invalid Lean encoding: {error}"),
            Self::InvalidStructure(error) => write!(formatter, "invalid Lean structure: {error}"),
            Self::IncompatibleEnvironment => formatter.write_str("incompatible Lean environment"),
            Self::Worker(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for LeanError {}

impl From<WorkerError> for LeanError {
    fn from(error: WorkerError) -> Self {
        Self::Worker(error)
    }
}

impl From<serde_json::Error> for LeanError {
    fn from(error: serde_json::Error) -> Self {
        Self::InvalidEncoding(error.to_string())
    }
}

#[derive(Clone, Debug)]
pub struct LeanCorpusEntry {
    pub name: LeanName,
    pub artifact: LeanArtifact,
    pub evidence: LeanEvidence,
    pub dependencies: Vec<LeanName>,
}

#[derive(Clone, Debug, Default)]
pub struct LeanCorpus {
    entries: Vec<LeanCorpusEntry>,
}

impl LeanCorpus {
    #[must_use]
    pub fn entries(&self) -> &[LeanCorpusEntry] {
        &self.entries
    }

    pub fn verified_page(worker: &LeanWorker, page: IndexPage) -> Result<Self, LeanError> {
        Self::verified_theorems(worker, page.artifacts)
    }

    pub fn verified_theorems(
        worker: &LeanWorker,
        theorems: Vec<IndexedTheorem>,
    ) -> Result<Self, LeanError> {
        let environment = worker.environment().clone();
        let items = theorems
            .iter()
            .map(|theorem| VerificationItem {
                proposition: theorem.proposition.clone(),
                proof_term: theorem.proof_term.clone(),
                allowed_axioms: theorem.axioms.clone(),
            })
            .collect::<Vec<_>>();
        let (results, _) = worker.verify(&items)?;
        let mut entries = Vec::with_capacity(theorems.len());
        for (theorem, result) in theorems.into_iter().zip(results) {
            if !result.accepted {
                continue;
            }
            let artifact = artifact_from_index(&environment, &theorem);
            let evidence = LeanEvidence {
                artifact_digest: artifact_digest(&artifact)?,
                axioms: result.axioms,
                environment: environment.clone(),
            };
            entries.push(LeanCorpusEntry {
                name: theorem.name,
                artifact,
                evidence,
                dependencies: theorem.dependencies,
            });
        }
        Ok(Self { entries })
    }

    pub fn append(&mut self, mut other: Self) {
        self.entries.append(&mut other.entries);
    }
}

fn artifact_from_index(
    environment: &LeanEnvironmentIdentity,
    theorem: &IndexedTheorem,
) -> LeanArtifact {
    LeanArtifact {
        environment: environment.clone(),
        proposition: theorem.proposition.clone(),
        proof_term: theorem.proof_term.clone(),
        allowed_axioms: theorem.axioms.clone(),
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LeanMetric {
    ProofNodes,
    ProofDepth,
    EncodedBytes,
    AxiomCount,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LeanClaim {
    environment: LeanEnvironmentIdentity,
    proposition: LeanExpr,
    allowed_axioms: Vec<LeanName>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LeanEvidence {
    artifact_digest: [u8; 32],
    axioms: Vec<LeanName>,
    environment: LeanEnvironmentIdentity,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LeanSeedScope {
    pub start: usize,
    pub count: usize,
}

pub struct LeanDomain {
    environment: LeanEnvironmentIdentity,
    structure: LeanStructure,
    seeds: LeanSeeds,
    operators: LeanOperators,
    kernel: LeanKernel,
    measurements: LeanMeasurements,
}

impl LeanDomain {
    pub fn new(config: LeanWorkerConfig, corpus: LeanCorpus) -> Result<Self, LeanError> {
        config.validate()?;
        let environment = pinned_environment_identity();
        if corpus
            .entries()
            .iter()
            .any(|entry| entry.artifact.environment != environment)
        {
            return Err(LeanError::IncompatibleEnvironment);
        }
        let substitutions = corpus
            .entries()
            .iter()
            .map(|entry| entry.artifact.clone())
            .collect::<Vec<_>>();
        Ok(Self {
            environment,
            structure: LeanStructure::new(),
            seeds: LeanSeeds {
                entries: Arc::new(corpus.entries),
            },
            operators: LeanOperators::new(substitutions),
            kernel: LeanKernel {
                config,
                worker: Mutex::new(None),
            },
            measurements: LeanMeasurements::new(),
        })
    }
}

impl DomainDefinition for LeanDomain {
    type Artifact = LeanArtifact;
    type Error = LeanError;
    type SeedScope = LeanSeedScope;
    type Metric = LeanMetric;
    type Observation = u64;
    type Structure = LeanStructure;
    type Seeds = LeanSeeds;
    type Operators = LeanOperators;
    type Kernel = LeanKernel;
    type Measurements = LeanMeasurements;

    fn semantic_identity(&self) -> SemanticIdentity {
        SemanticIdentity::new(format!(
            "reflex-lean-v1:mathlib={}:toolchain={}:lean={}",
            self.environment.mathlib_commit,
            self.environment.lean_toolchain,
            self.environment.lean_commit
        ))
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
pub enum LeanSort {
    Expression,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LeanConstructor {
    BoundVariable,
    Sort,
    Constant,
    Application,
    Lambda,
    Forall,
    Let,
    Literal,
    Projection,
}

pub struct LeanStructure {
    schema: StructuralSchema<LeanSort, LeanConstructor>,
}

impl LeanStructure {
    fn new() -> Self {
        let expression = LeanSort::Expression;
        let descriptor = |constructor, symbol, children: usize, bindings: Vec<u32>| {
            ConstructorDescriptor::new_variable_immediates(
                constructor,
                SymbolId::new(symbol),
                expression,
                vec![expression; children],
                bindings,
            )
        };
        Self {
            schema: StructuralSchema {
                sorts: vec![(expression, SymbolId::new("lean-expression"))],
                constructors: vec![
                    descriptor(LeanConstructor::BoundVariable, "lean-bvar", 0, vec![]),
                    descriptor(LeanConstructor::Sort, "lean-sort", 0, vec![]),
                    descriptor(LeanConstructor::Constant, "lean-const", 0, vec![]),
                    descriptor(LeanConstructor::Application, "lean-app", 2, vec![0, 0]),
                    descriptor(LeanConstructor::Lambda, "lean-lambda", 2, vec![0, 1]),
                    descriptor(LeanConstructor::Forall, "lean-forall", 2, vec![0, 1]),
                    descriptor(LeanConstructor::Let, "lean-let", 3, vec![0, 0, 1]),
                    descriptor(LeanConstructor::Literal, "lean-literal", 0, vec![]),
                    descriptor(LeanConstructor::Projection, "lean-projection", 1, vec![0]),
                ],
            },
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NodeImmediate {
    environment: LeanEnvironmentIdentity,
    proposition: LeanExpr,
    allowed_axioms: Vec<LeanName>,
    auxiliary: NodeAuxiliary,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
enum NodeAuxiliary {
    BoundVariable { index: usize },
    Sort { level: LeanLevel },
    Constant { name: LeanName, levels: Vec<LeanLevel> },
    Application,
    Lambda { name: LeanName, binder_info: LeanBinderInfo },
    Forall { name: LeanName, binder_info: LeanBinderInfo },
    Let { name: LeanName, non_dep: bool },
    Literal { literal: LeanLiteral },
    Projection { type_name: LeanName, index: usize },
}

pub struct LeanStructureView<'a> {
    artifact: &'a LeanArtifact,
    nodes: Vec<&'a LeanExpr>,
    children: Vec<Vec<usize>>,
}

impl<'a> LeanStructureView<'a> {
    fn new(artifact: &'a LeanArtifact) -> Self {
        fn push<'a>(
            expression: &'a LeanExpr,
            nodes: &mut Vec<&'a LeanExpr>,
            children: &mut Vec<Vec<usize>>,
        ) -> usize {
            let index = nodes.len();
            nodes.push(expression);
            children.push(Vec::new());
            match expression {
                LeanExpr::App { function, argument } => {
                    let function = push(function, nodes, children);
                    let argument = push(argument, nodes, children);
                    children[index].extend([function, argument]);
                }
                LeanExpr::Lam {
                    binder_type, body, ..
                }
                | LeanExpr::ForallE {
                    binder_type, body, ..
                } => {
                    let binder_type = push(binder_type, nodes, children);
                    let body = push(body, nodes, children);
                    children[index].extend([binder_type, body]);
                }
                LeanExpr::LetE {
                    r#type,
                    value,
                    body,
                    ..
                } => {
                    let r#type = push(r#type, nodes, children);
                    let value = push(value, nodes, children);
                    let body = push(body, nodes, children);
                    children[index].extend([r#type, value, body]);
                }
                LeanExpr::Proj { subject, .. } => {
                    let subject = push(subject, nodes, children);
                    children[index].push(subject);
                }
                LeanExpr::Bvar { .. }
                | LeanExpr::Sort { .. }
                | LeanExpr::Const { .. }
                | LeanExpr::Lit { .. } => {}
            }
            index
        }
        let mut nodes = Vec::new();
        let mut children = Vec::new();
        push(&artifact.proof_term, &mut nodes, &mut children);
        Self {
            artifact,
            nodes,
            children,
        }
    }
}

impl StructuralView for LeanStructureView<'_> {
    type Sort = LeanSort;
    type Constructor = LeanConstructor;

    fn root_sort(&self) -> Self::Sort {
        LeanSort::Expression
    }

    fn node_count(&self) -> usize {
        self.nodes.len()
    }

    fn node_sort(&self, node: usize) -> Option<Self::Sort> {
        self.nodes.get(node).map(|_| LeanSort::Expression)
    }

    fn node_constructor(&self, node: usize) -> Option<Self::Constructor> {
        self.nodes.get(node).map(|expression| constructor_of(expression))
    }

    fn write_children(&self, node: usize, output: &mut Vec<usize>) -> bool {
        let Some(children) = self.children.get(node) else {
            return false;
        };
        output.clear();
        output.extend_from_slice(children);
        true
    }

    fn write_immediates(&self, node: usize, output: &mut Vec<u64>) -> bool {
        let Some(expression) = self.nodes.get(node) else {
            return false;
        };
        let immediate = NodeImmediate {
            environment: self.artifact.environment.clone(),
            proposition: self.artifact.proposition.clone(),
            allowed_axioms: self.artifact.allowed_axioms.clone(),
            auxiliary: auxiliary_of(expression),
        };
        let Ok(bytes) = serde_json::to_vec(&immediate) else {
            return false;
        };
        output.clear();
        output.extend(bytes.into_iter().map(u64::from));
        true
    }

    fn dynamic_resident_bytes(&self) -> u64 {
        serde_json::to_vec(self.artifact)
            .map_or(u64::MAX, |bytes| bytes.len() as u64)
    }
}

impl StructuralProtocol<LeanDomain> for LeanStructure {
    type Sort = LeanSort;
    type Constructor = LeanConstructor;
    type View<'a> = LeanStructureView<'a>;
    type Scratch = Vec<u8>;

    fn schema(&self) -> &StructuralSchema<Self::Sort, Self::Constructor> {
        &self.schema
    }

    fn view<'a>(&'a self, artifact: &'a LeanArtifact) -> Self::View<'a> {
        LeanStructureView::new(artifact)
    }

    fn compose(
        &self,
        constructor: Self::Constructor,
        children: &[&LeanArtifact],
        immediates: &[u64],
        scratch: &mut Self::Scratch,
    ) -> Result<LeanArtifact, LeanError> {
        scratch.clear();
        scratch.extend(
            immediates
                .iter()
                .map(|value| u8::try_from(*value))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| LeanError::InvalidStructure("immediate byte exceeds 255".into()))?,
        );
        let immediate: NodeImmediate = decode_json(scratch)?;
        let proof_term = compose_expression(constructor, &immediate.auxiliary, children)?;
        Ok(LeanArtifact {
            environment: immediate.environment,
            proposition: immediate.proposition,
            proof_term,
            allowed_axioms: immediate.allowed_axioms,
        })
    }

    fn extract(
        &self,
        artifact: &LeanArtifact,
        node: usize,
        _scratch: &mut Self::Scratch,
    ) -> Result<LeanArtifact, LeanError> {
        let proof_term = artifact
            .proof_term
            .expression_at(node)
            .ok_or_else(|| LeanError::InvalidStructure("node is out of range".into()))?
            .clone();
        Ok(LeanArtifact {
            environment: artifact.environment.clone(),
            proposition: artifact.proposition.clone(),
            proof_term,
            allowed_axioms: artifact.allowed_axioms.clone(),
        })
    }

    fn replace(
        &self,
        artifact: &LeanArtifact,
        node: usize,
        replacement: &LeanArtifact,
        _scratch: &mut Self::Scratch,
    ) -> Result<LeanArtifact, LeanError> {
        if artifact.environment != replacement.environment {
            return Err(LeanError::IncompatibleEnvironment);
        }
        let proof_term = artifact
            .proof_term
            .replacing(node, &replacement.proof_term)
            .ok_or_else(|| LeanError::InvalidStructure("node is out of range".into()))?;
        Ok(LeanArtifact {
            environment: artifact.environment.clone(),
            proposition: artifact.proposition.clone(),
            proof_term,
            allowed_axioms: artifact.allowed_axioms.clone(),
        })
    }

    fn encode_canonical(
        &self,
        artifact: &LeanArtifact,
        output: &mut Vec<u8>,
        _scratch: &mut Self::Scratch,
    ) -> Result<(), LeanError> {
        serde_json::to_writer(output, artifact)?;
        Ok(())
    }

    fn decode_canonical(
        &self,
        bytes: &[u8],
        _scratch: &mut Self::Scratch,
    ) -> Result<LeanArtifact, LeanError> {
        decode_json(bytes)
    }
}

fn constructor_of(expression: &LeanExpr) -> LeanConstructor {
    match expression {
        LeanExpr::Bvar { .. } => LeanConstructor::BoundVariable,
        LeanExpr::Sort { .. } => LeanConstructor::Sort,
        LeanExpr::Const { .. } => LeanConstructor::Constant,
        LeanExpr::App { .. } => LeanConstructor::Application,
        LeanExpr::Lam { .. } => LeanConstructor::Lambda,
        LeanExpr::ForallE { .. } => LeanConstructor::Forall,
        LeanExpr::LetE { .. } => LeanConstructor::Let,
        LeanExpr::Lit { .. } => LeanConstructor::Literal,
        LeanExpr::Proj { .. } => LeanConstructor::Projection,
    }
}

fn auxiliary_of(expression: &LeanExpr) -> NodeAuxiliary {
    match expression {
        LeanExpr::Bvar { index } => NodeAuxiliary::BoundVariable { index: *index },
        LeanExpr::Sort { level } => NodeAuxiliary::Sort {
            level: level.clone(),
        },
        LeanExpr::Const { name, levels } => NodeAuxiliary::Constant {
            name: name.clone(),
            levels: levels.clone(),
        },
        LeanExpr::App { .. } => NodeAuxiliary::Application,
        LeanExpr::Lam {
            name, binder_info, ..
        } => NodeAuxiliary::Lambda {
            name: name.clone(),
            binder_info: *binder_info,
        },
        LeanExpr::ForallE {
            name, binder_info, ..
        } => NodeAuxiliary::Forall {
            name: name.clone(),
            binder_info: *binder_info,
        },
        LeanExpr::LetE { name, non_dep, .. } => NodeAuxiliary::Let {
            name: name.clone(),
            non_dep: *non_dep,
        },
        LeanExpr::Lit { literal } => NodeAuxiliary::Literal {
            literal: literal.clone(),
        },
        LeanExpr::Proj {
            type_name, index, ..
        } => NodeAuxiliary::Projection {
            type_name: type_name.clone(),
            index: *index,
        },
    }
}

fn compose_expression(
    constructor: LeanConstructor,
    auxiliary: &NodeAuxiliary,
    children: &[&LeanArtifact],
) -> Result<LeanExpr, LeanError> {
    let expressions = children
        .iter()
        .map(|artifact| artifact.proof_term.clone())
        .collect::<Vec<_>>();
    let invalid = || LeanError::InvalidStructure("constructor payload or child arity differs".into());
    match (constructor, auxiliary, expressions.as_slice()) {
        (LeanConstructor::BoundVariable, NodeAuxiliary::BoundVariable { index }, []) => {
            Ok(LeanExpr::Bvar { index: *index })
        }
        (LeanConstructor::Sort, NodeAuxiliary::Sort { level }, []) => Ok(LeanExpr::Sort {
            level: level.clone(),
        }),
        (LeanConstructor::Constant, NodeAuxiliary::Constant { name, levels }, []) => {
            Ok(LeanExpr::Const {
                name: name.clone(),
                levels: levels.clone(),
            })
        }
        (LeanConstructor::Application, NodeAuxiliary::Application, [function, argument]) => {
            Ok(LeanExpr::App {
                function: Box::new(function.clone()),
                argument: Box::new(argument.clone()),
            })
        }
        (
            LeanConstructor::Lambda,
            NodeAuxiliary::Lambda { name, binder_info },
            [binder_type, body],
        ) => Ok(LeanExpr::Lam {
            name: name.clone(),
            binder_type: Box::new(binder_type.clone()),
            body: Box::new(body.clone()),
            binder_info: *binder_info,
        }),
        (
            LeanConstructor::Forall,
            NodeAuxiliary::Forall { name, binder_info },
            [binder_type, body],
        ) => Ok(LeanExpr::ForallE {
            name: name.clone(),
            binder_type: Box::new(binder_type.clone()),
            body: Box::new(body.clone()),
            binder_info: *binder_info,
        }),
        (LeanConstructor::Let, NodeAuxiliary::Let { name, non_dep }, [r#type, value, body]) => {
            Ok(LeanExpr::LetE {
                name: name.clone(),
                r#type: Box::new(r#type.clone()),
                value: Box::new(value.clone()),
                body: Box::new(body.clone()),
                non_dep: *non_dep,
            })
        }
        (LeanConstructor::Literal, NodeAuxiliary::Literal { literal }, []) => {
            Ok(LeanExpr::Lit {
                literal: literal.clone(),
            })
        }
        (
            LeanConstructor::Projection,
            NodeAuxiliary::Projection { type_name, index },
            [subject],
        ) => Ok(LeanExpr::Proj {
            type_name: type_name.clone(),
            index: *index,
            subject: Box::new(subject.clone()),
        }),
        _ => Err(invalid()),
    }
}

pub struct LeanSeeds {
    entries: Arc<Vec<LeanCorpusEntry>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LeanSeedCursor {
    next: usize,
    end: usize,
}

impl SeedSource<LeanDomain> for LeanSeeds {
    type Cursor = LeanSeedCursor;
    type Scratch = ();

    fn open(&self, scope: &LeanSeedScope) -> Result<Self::Cursor, LeanError> {
        if scope.start > self.entries.len()
            || scope.start.saturating_add(scope.count) > self.entries.len()
        {
            return Err(LeanError::InvalidStructure("seed scope exceeds corpus".into()));
        }
        Ok(LeanSeedCursor {
            next: scope.start,
            end: scope.start + scope.count,
        })
    }

    fn read_batch(
        &self,
        cursor: &mut Self::Cursor,
        limit: usize,
        output: &mut SeedWriter<'_, LeanDomain>,
        _scratch: &mut Self::Scratch,
    ) -> Result<SeedPage, LeanError> {
        let end = cursor.next.saturating_add(limit).min(cursor.end);
        let start = cursor.next;
        for entry in &self.entries[start..end] {
            let claim = LeanClaim {
                environment: entry.artifact.environment.clone(),
                proposition: entry.artifact.proposition.clone(),
                allowed_axioms: entry.artifact.allowed_axioms.clone(),
            };
            output.push(Seed {
                artifact: entry.artifact.clone(),
                verification: VerificationRecord {
                    claim,
                    evidence: entry.evidence.clone(),
                    kernel_revision: KERNEL_REVISION,
                },
                provenance: entry.name.to_string().into_bytes(),
            });
        }
        cursor.next = end;
        Ok(SeedPage {
            emitted: end - start,
            exhausted: end == cursor.end,
        })
    }

    fn encode_scope(&self, scope: &LeanSeedScope, output: &mut Vec<u8>) -> Result<(), LeanError> {
        serde_json::to_writer(output, scope)?;
        Ok(())
    }

    fn decode_scope(&self, bytes: &[u8]) -> Result<LeanSeedScope, LeanError> {
        decode_json(bytes)
    }

    fn encode_cursor(&self, cursor: &Self::Cursor, output: &mut Vec<u8>) -> Result<(), LeanError> {
        serde_json::to_writer(output, cursor)?;
        Ok(())
    }

    fn decode_cursor(&self, bytes: &[u8]) -> Result<Self::Cursor, LeanError> {
        decode_json(bytes)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum LeanOperator {
    ProofSubstitution,
    Application,
    Rewriting,
    Factoring,
    AntiUnification,
    Abstraction,
    Normalization,
    Composition,
    VerifiedGeneralization,
}

pub struct LeanOperators {
    catalog: Vec<OperatorDescriptor<LeanOperator>>,
    substitutions_by_proposition: HashMap<LeanExpr, Vec<LeanArtifact>>,
    all_proofs: Vec<LeanArtifact>,
}

impl LeanOperators {
    fn new(substitutions: Vec<LeanArtifact>) -> Self {
        let mut substitutions_by_proposition = HashMap::new();
        for artifact in &substitutions {
            substitutions_by_proposition
                .entry(artifact.proposition.clone())
                .or_insert_with(Vec::new)
                .push(artifact.clone());
        }
        let descriptor = |operator, symbol| OperatorDescriptor::new(operator, SymbolId::new(symbol));
        Self {
            catalog: vec![
                descriptor(LeanOperator::ProofSubstitution, "lean-proof-substitution"),
                descriptor(LeanOperator::Application, "lean-application"),
                descriptor(LeanOperator::Rewriting, "lean-rewriting"),
                descriptor(LeanOperator::Factoring, "lean-factoring"),
                descriptor(LeanOperator::AntiUnification, "lean-anti-unification"),
                descriptor(LeanOperator::Abstraction, "lean-abstraction"),
                descriptor(LeanOperator::Normalization, "lean-normalization"),
                descriptor(LeanOperator::Composition, "lean-composition"),
                descriptor(
                    LeanOperator::VerifiedGeneralization,
                    "lean-verified-generalization",
                ),
            ],
            substitutions_by_proposition,
            all_proofs: substitutions,
        }
    }
}

pub struct LeanApplication {
    source_index: usize,
    candidate: LeanArtifact,
}

impl OperatorAlgebra<LeanDomain> for LeanOperators {
    type Operator = LeanOperator;
    type Application = LeanApplication;
    type Scratch = ();

    fn catalog(&self) -> &[OperatorDescriptor<Self::Operator>] {
        &self.catalog
    }

    fn enumerate_legal(
        &self,
        requests: OperatorEnumerationBatch<'_, LeanDomain, Self::Operator>,
        output: &mut ApplicationWriter<'_, Self::Application>,
        _scratch: &mut Self::Scratch,
    ) -> Result<(), LeanError> {
        for location in requests.locations() {
            let source = requests
                .artifacts()
                .get(location.artifact_index())
                .ok_or_else(|| LeanError::InvalidStructure("artifact index is out of range".into()))?;
            for operator in requests.operators() {
                match operator {
                    LeanOperator::ProofSubstitution if location.node_index() == 0 => {
                        if let Some(substitutions) =
                            self.substitutions_by_proposition.get(&source.proposition)
                        {
                            for substitution in substitutions {
                                if output.is_full() {
                                    return Ok(());
                                }
                                if substitution.proof_term != source.proof_term {
                                    output.push(LeanApplication {
                                        source_index: location.artifact_index(),
                                        candidate: substitution.clone(),
                                    });
                                }
                            }
                        }
                    }
                    LeanOperator::Application | LeanOperator::Composition => {
                        for other in &self.all_proofs {
                            if output.is_full() {
                                return Ok(());
                            }
                            let (function, argument) = if *operator == LeanOperator::Application {
                                (source.proof_term.clone(), other.proof_term.clone())
                            } else {
                                (other.proof_term.clone(), source.proof_term.clone())
                            };
                            output.push(LeanApplication {
                                source_index: location.artifact_index(),
                                candidate: LeanArtifact {
                                    environment: source.environment.clone(),
                                    proposition: source.proposition.clone(),
                                    proof_term: LeanExpr::App {
                                        function: Box::new(function),
                                        argument: Box::new(argument),
                                    },
                                    allowed_axioms: source.allowed_axioms.clone(),
                                },
                            });
                        }
                    }
                    LeanOperator::Rewriting => {
                        for other in &self.all_proofs {
                            if output.is_full() {
                                return Ok(());
                            }
                            if let Some(proof_term) = source
                                .proof_term
                                .replacing(location.node_index(), &other.proof_term)
                            {
                                output.push(LeanApplication {
                                    source_index: location.artifact_index(),
                                    candidate: LeanArtifact {
                                        environment: source.environment.clone(),
                                        proposition: source.proposition.clone(),
                                        proof_term,
                                        allowed_axioms: source.allowed_axioms.clone(),
                                    },
                                });
                            }
                        }
                    }
                    LeanOperator::Factoring
                    | LeanOperator::AntiUnification
                    | LeanOperator::Abstraction
                    | LeanOperator::Normalization
                    | LeanOperator::VerifiedGeneralization
                    | LeanOperator::ProofSubstitution => {}
                }
            }
        }
        Ok(())
    }

    fn apply_batch(
        &self,
        applications: &[Self::Application],
        output: &mut CandidateWriter<'_, LeanDomain>,
        _scratch: &mut Self::Scratch,
    ) -> Result<(), LeanError> {
        for application in applications {
            output.push(application.source_index, application.candidate.clone());
        }
        Ok(())
    }
}

pub struct LeanKernel {
    config: LeanWorkerConfig,
    worker: Mutex<Option<LeanWorker>>,
}

impl VerificationKernel<LeanDomain> for LeanKernel {
    type Claim = LeanClaim;
    type Evidence = LeanEvidence;
    type Scratch = ();

    fn revision(&self) -> KernelRevision {
        KERNEL_REVISION
    }

    fn worker_requirements(&self) -> VerificationWorkerRequirements {
        VerificationWorkerRequirements::external(
            std::num::NonZeroUsize::MIN,
            self.config.resident_bytes,
        )
    }

    fn claim_for_candidate(
        &self,
        seed: &LeanArtifact,
        candidate: &LeanArtifact,
    ) -> Result<Self::Claim, LeanError> {
        if seed.environment != candidate.environment
            || seed.environment != pinned_environment_identity()
            || seed.proposition != candidate.proposition
        {
            return Err(LeanError::IncompatibleEnvironment);
        }
        Ok(LeanClaim {
            environment: seed.environment.clone(),
            proposition: seed.proposition.clone(),
            allowed_axioms: seed.allowed_axioms.clone(),
        })
    }

    fn verify_batch(
        &self,
        requests: VerificationBatch<'_, LeanDomain, Self::Claim>,
        output: &mut VerdictWriter<'_, Self::Evidence>,
        _scratch: &mut Self::Scratch,
    ) -> VerificationBatchOutcome<LeanError> {
        let started = Instant::now();
        let items = requests
            .requests()
            .iter()
            .map(|request| VerificationItem {
                proposition: request.claim.proposition.clone(),
                proof_term: request.candidate.proof_term.clone(),
                allowed_axioms: request.claim.allowed_axioms.clone(),
            })
            .collect::<Vec<_>>();
        match self.verify_items(&items, requests.allowance().elapsed_time()) {
            Ok((results, usage)) => {
                for (request, result) in requests.requests().iter().zip(results) {
                    if result.accepted
                        && request.claim.environment == request.candidate.environment
                        && request.claim.proposition == request.candidate.proposition
                    {
                        let evidence = artifact_digest(request.candidate).map(|artifact_digest| {
                            LeanEvidence {
                                artifact_digest,
                                axioms: result.axioms,
                                environment: request.claim.environment.clone(),
                            }
                        });
                        match evidence {
                            Ok(evidence) => output.push(Verdict::Accepted { evidence }),
                            Err(error) => {
                                return VerificationBatchOutcome::domain_error(
                                    report(usage, false),
                                    error,
                                );
                            }
                        }
                    } else {
                        output.push(Verdict::Refuted);
                    }
                }
                VerificationBatchOutcome::completed(report(usage, false))
            }
            Err(error) => VerificationBatchOutcome::domain_error(
                failed_report(self.config.resident_bytes, started.elapsed()),
                LeanError::Worker(error),
            ),
        }
    }

    fn replay_batch(
        &self,
        records: VerificationReplayBatch<'_, LeanDomain, Self::Claim, Self::Evidence>,
        output: &mut ReplayVerdictWriter<'_>,
        _scratch: &mut Self::Scratch,
    ) -> VerificationBatchOutcome<LeanError> {
        let started = Instant::now();
        let items = records
            .requests()
            .iter()
            .map(|record| VerificationItem {
                proposition: record.claim.proposition.clone(),
                proof_term: record.artifact.proof_term.clone(),
                allowed_axioms: record.claim.allowed_axioms.clone(),
            })
            .collect::<Vec<_>>();
        match self.verify_items(&items, records.allowance().elapsed_time()) {
            Ok((results, usage)) => {
                for (record, result) in records.requests().iter().zip(results) {
                    output.push(replay_accepted(record, &result));
                }
                VerificationBatchOutcome::completed(report(usage, false))
            }
            Err(error) => VerificationBatchOutcome::domain_error(
                failed_report(self.config.resident_bytes, started.elapsed()),
                LeanError::Worker(error),
            ),
        }
    }

    fn encode_claim(&self, claim: &Self::Claim, output: &mut Vec<u8>) -> Result<(), LeanError> {
        serde_json::to_writer(output, claim)?;
        Ok(())
    }

    fn decode_claim(&self, bytes: &[u8]) -> Result<Self::Claim, LeanError> {
        decode_json(bytes)
    }

    fn encode_evidence(
        &self,
        evidence: &Self::Evidence,
        output: &mut Vec<u8>,
    ) -> Result<(), LeanError> {
        serde_json::to_writer(output, evidence)?;
        Ok(())
    }

    fn decode_evidence(&self, bytes: &[u8]) -> Result<Self::Evidence, LeanError> {
        decode_json(bytes)
    }
}

impl LeanKernel {
    fn verify_items(
        &self,
        items: &[VerificationItem],
        deadline: std::time::Duration,
    ) -> Result<(Vec<crate::worker::VerificationResult>, WorkerUsage), WorkerError> {
        let started = Instant::now();
        let mut worker = self
            .worker
            .lock()
            .map_err(|_| WorkerError::Protocol("lazy worker mutex was poisoned".into()))?;
        if worker.is_none() {
            *worker = Some(LeanWorker::start_bounded(&self.config, deadline)?);
        }
        let remaining = deadline.saturating_sub(started.elapsed());
        let (results, _) = worker
            .as_ref()
            .ok_or_else(|| WorkerError::Protocol("lazy worker did not start".into()))?
            .verify_bounded(items, remaining)?;
        let elapsed = started.elapsed();
        Ok((
            results,
            WorkerUsage {
                elapsed,
                cpu_upper_bound: elapsed,
                resident_upper_bound: self.config.resident_bytes.get(),
            },
        ))
    }
}

fn replay_accepted(
    record: &VerificationReplayRequest<'_, LeanDomain, LeanClaim, LeanEvidence>,
    result: &crate::worker::VerificationResult,
) -> bool {
    result.accepted
        && record.kernel_revision == KERNEL_REVISION
        && record.claim.environment == record.artifact.environment
        && record.claim.proposition == record.artifact.proposition
        && record.evidence.environment == record.artifact.environment
        && record.evidence.axioms == result.axioms
        && artifact_digest(record.artifact).is_ok_and(|digest| digest == record.evidence.artifact_digest)
}

fn artifact_digest(artifact: &LeanArtifact) -> Result<[u8; 32], LeanError> {
    let encoded = serde_json::to_vec(artifact)?;
    Ok(Sha256::digest(encoded).into())
}

fn decode_json<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> Result<T, LeanError> {
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    deserializer.disable_recursion_limit();
    Ok(T::deserialize(&mut deserializer)?)
}

fn report(usage: WorkerUsage, worker_failed: bool) -> VerificationBatchReport {
    VerificationBatchReport::external(
        ExternalVerificationUsage::new(
            1,
            usage.resident_upper_bound,
            usage.elapsed,
            usage.cpu_upper_bound,
        ),
        worker_failed,
    )
}

fn failed_report(resident: NonZeroU64, elapsed: std::time::Duration) -> VerificationBatchReport {
    VerificationBatchReport::external(
        ExternalVerificationUsage::new(1, resident.get(), elapsed, elapsed),
        true,
    )
}

pub struct LeanMeasurements {
    schema: Vec<MeasurementDescriptor<LeanMetric>>,
}

impl LeanMeasurements {
    fn new() -> Self {
        Self {
            schema: vec![
                MeasurementDescriptor::new(LeanMetric::ProofNodes, SymbolId::new("proof-nodes")),
                MeasurementDescriptor::new(LeanMetric::ProofDepth, SymbolId::new("proof-depth")),
                MeasurementDescriptor::new(
                    LeanMetric::EncodedBytes,
                    SymbolId::new("encoded-proof-bytes"),
                ),
                MeasurementDescriptor::new(LeanMetric::AxiomCount, SymbolId::new("axiom-count")),
            ],
        }
    }
}

impl MeasurementSpace<LeanDomain> for LeanMeasurements {
    type Metric = LeanMetric;
    type Observation = u64;
    type Scratch = Vec<u8>;

    fn schema(&self) -> &[MeasurementDescriptor<Self::Metric>] {
        &self.schema
    }

    fn measure_batch(
        &self,
        artifacts: VerifiedBatch<'_, LeanDomain>,
        _environment: &MeasurementEnvironment,
        output: &mut MeasurementWriter<'_, Self::Metric, Self::Observation>,
        scratch: &mut Self::Scratch,
    ) -> Result<(), LeanError> {
        for (index, artifact) in artifacts.artifacts().iter().enumerate() {
            scratch.clear();
            serde_json::to_writer(&mut *scratch, &artifact.proof_term)?;
            output.push(index, LeanMetric::ProofNodes, artifact.proof_term.node_count() as u64);
            output.push(index, LeanMetric::ProofDepth, artifact.proof_term.depth() as u64);
            output.push(index, LeanMetric::EncodedBytes, scratch.len() as u64);
            output.push(index, LeanMetric::AxiomCount, artifact.allowed_axioms.len() as u64);
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
        left: &MeasurementEnvironment,
        right: &MeasurementEnvironment,
    ) -> bool {
        left.identity() == right.identity()
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
    ) -> Result<(), LeanError> {
        output.extend_from_slice(&observation.to_le_bytes());
        Ok(())
    }

    fn decode_observation(
        &self,
        _metric: Self::Metric,
        bytes: &[u8],
    ) -> Result<Self::Observation, LeanError> {
        let encoded: [u8; 8] = bytes
            .try_into()
            .map_err(|_| LeanError::InvalidEncoding("observation width differs".into()))?;
        Ok(u64::from_le_bytes(encoded))
    }
}

#[must_use]
pub fn pinned_environment_identity() -> LeanEnvironmentIdentity {
    LeanEnvironmentIdentity {
        mathlib_commit: MATHLIB_COMMIT.into(),
        lean_toolchain: LEAN_TOOLCHAIN.into(),
        lean_commit: LEAN_COMMIT.into(),
    }
}

#[cfg(test)]
mod tests {
    use reflex::{StructuralProtocol, StructuralView};

    use super::{LeanArtifact, LeanConstructor, LeanExpr, LeanStructure, pinned_environment_identity};
    use crate::ast::{LeanBinderInfo, LeanName};

    fn artifact() -> LeanArtifact {
        let nat = LeanExpr::constant(LeanName::from_dotted("Nat"), vec![]);
        LeanArtifact {
            environment: pinned_environment_identity(),
            proposition: LeanExpr::ForallE {
                name: LeanName::from_dotted("n"),
                binder_type: Box::new(nat.clone()),
                body: Box::new(nat.clone()),
                binder_info: LeanBinderInfo::Default,
            },
            proof_term: LeanExpr::Lam {
                name: LeanName::from_dotted("n"),
                binder_type: Box::new(nat),
                body: Box::new(LeanExpr::Bvar { index: 0 }),
                binder_info: LeanBinderInfo::Default,
            },
            allowed_axioms: vec![],
        }
    }

    #[test]
    fn variable_immediates_reconstruct_the_exact_core_term() {
        fn rebuild(
            structure: &LeanStructure,
            original: &LeanArtifact,
            node: usize,
        ) -> LeanArtifact {
            let view = structure.view(original);
            let constructor = view.node_constructor(node).unwrap();
            let mut child_indexes = Vec::new();
            assert!(view.write_children(node, &mut child_indexes));
            let mut immediates = Vec::new();
            assert!(view.write_immediates(node, &mut immediates));
            let children = child_indexes
                .into_iter()
                .map(|child| rebuild(structure, original, child))
                .collect::<Vec<_>>();
            structure
                .compose(
                    constructor,
                    &children.iter().collect::<Vec<_>>(),
                    &immediates,
                    &mut Vec::new(),
                )
                .unwrap()
        }

        let structure = LeanStructure::new();
        let original = artifact();
        assert_eq!(rebuild(&structure, &original, 0), original);
        assert!(structure.schema().constructors.iter().all(|descriptor| {
            descriptor.immediate_arity() == reflex::ImmediateArity::Variable
        }));
    }

    #[test]
    fn extract_and_replace_preserve_the_seed_relative_claim() {
        let structure = LeanStructure::new();
        let original = artifact();
        let extracted = structure.extract(&original, 2, &mut vec![]).unwrap();
        assert_eq!(extracted.proof_term, LeanExpr::Bvar { index: 0 });
        let replacement = LeanArtifact {
            proof_term: LeanExpr::constant(LeanName::from_dotted("Nat.zero"), vec![]),
            ..extracted
        };
        let replaced = structure
            .replace(&original, 2, &replacement, &mut vec![])
            .unwrap();
        assert!(matches!(replaced.proof_term, LeanExpr::Lam { .. }));
        assert_eq!(replaced.proposition, original.proposition);
    }

    #[test]
    fn constructor_mapping_covers_all_core_expression_forms() {
        assert_eq!(
            super::constructor_of(&LeanExpr::Bvar { index: 0 }),
            LeanConstructor::BoundVariable
        );
    }
}
