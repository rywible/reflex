use std::num::NonZeroU64;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use reflex::domain::{ReplayVerdictWriter, StructuralSchema, SymbolId, VerificationReplayRequest};
use reflex::{
    ApplicationWriter, CandidateWriter, ConstructorDescriptor, DomainDefinition,
    ExternalVerificationUsage, Incomparable, KernelRevision, MeasurementDescriptor,
    MeasurementEnvironment, MeasurementSpace, MeasurementWriter, MetricOrdering, OperatorAlgebra,
    OperatorDescriptor, OperatorEnumerationBatch, ProposalFeatures, ProposalProvenance, Seed,
    SeedPage, SeedSource, SeedWriter, SemanticIdentity, StructuralProtocol, StructuralView,
    Verdict, VerdictWriter, VerificationBatch, VerificationBatchOutcome, VerificationBatchReport,
    VerificationKernel, VerificationRecord, VerificationReplayBatch,
    VerificationWorkerRequirements, VerifiedBatch,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::ast::{
    LeanArtifact, LeanBinderInfo, LeanDeclarationIdentity, LeanEnvironmentIdentity, LeanExpr,
    LeanLevel, LeanLiteral, LeanName,
};
use crate::retrieval::{DonorRetrievalIndex, DonorRetrievalScratch, RetrievalTier, RetrievedDonor};
use crate::worker::{
    IndexPage, IndexedTheorem, LeanWorker, LeanWorkerConfig, VerificationItem, WorkerError,
    WorkerUsage,
};
use crate::{
    ARTIFACT_FORMAT_VERSION, KERNEL_CONTRACT_VERSION, LEAN_COMMIT, LEAN_TOOLCHAIN, MATHLIB_COMMIT,
};

const KERNEL_REVISION: KernelRevision = KernelRevision(2);

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
}

#[derive(Clone, Debug, Default)]
pub struct LeanCorpus {
    entries: Vec<LeanCorpusEntry>,
    operator_library: Vec<LeanArtifact>,
}

impl LeanCorpus {
    #[must_use]
    pub fn entries(&self) -> &[LeanCorpusEntry] {
        &self.entries
    }

    #[must_use]
    pub fn operator_library(&self) -> &[LeanArtifact] {
        &self.operator_library
    }

    pub fn verified_page(worker: &LeanWorker, page: IndexPage) -> Result<Self, LeanError> {
        Self::verified_theorems(worker, page.artifacts)
    }

    pub fn verified_theorems(
        worker: &LeanWorker,
        theorems: Vec<IndexedTheorem>,
    ) -> Result<Self, LeanError> {
        let mut corpus = Self::verified_entries(worker, theorems)?;
        corpus.operator_library = corpus
            .entries
            .iter()
            .map(|entry| entry.artifact.clone())
            .collect();
        Ok(corpus)
    }

    pub fn verified_seeds_with_library(
        worker: &LeanWorker,
        mut seeds: Vec<IndexedTheorem>,
        library: Vec<IndexedTheorem>,
    ) -> Result<Self, LeanError> {
        let seed_count = seeds.len();
        seeds.extend(library);
        let mut verified = Self::verified_entries(worker, seeds)?;
        let library_entries = verified.entries.split_off(seed_count);
        verified.operator_library = library_entries
            .into_iter()
            .map(|entry| entry.artifact)
            .collect();
        Ok(verified)
    }

    fn verified_entries(
        worker: &LeanWorker,
        theorems: Vec<IndexedTheorem>,
    ) -> Result<Self, LeanError> {
        let environment = worker.environment().clone();
        let items = theorems
            .iter()
            .map(|theorem| VerificationItem {
                level_params: theorem.level_params.clone(),
                claim_proposition: theorem.proposition.clone(),
                candidate_proposition: theorem.proposition.clone(),
                proof_term: theorem.proof_term.clone(),
                allowed_axioms: theorem.axioms.clone(),
            })
            .collect::<Vec<_>>();
        let (results, _) = worker.verify(&items)?;
        let mut entries = Vec::with_capacity(theorems.len());
        for (theorem, result) in theorems.into_iter().zip(results) {
            if !result.accepted {
                return Err(LeanError::InvalidStructure(format!(
                    "indexed theorem {} did not replay: {}",
                    theorem.name, result.diagnostic
                )));
            }
            let artifact = artifact_from_index(&environment, &theorem);
            let dependencies = normalized_names(result.dependencies);
            if artifact.dependencies != dependencies {
                return Err(LeanError::InvalidStructure(format!(
                    "indexed theorem {} reported different dependencies on replay",
                    theorem.name
                )));
            }
            let evidence = LeanEvidence {
                artifact_digest: artifact_digest(&artifact)?,
                dependencies,
                axioms: result.axioms,
                environment: environment.clone(),
            };
            entries.push(LeanCorpusEntry {
                name: theorem.name,
                artifact,
                evidence,
            });
        }
        Ok(Self {
            entries,
            operator_library: Vec::new(),
        })
    }

    pub fn append(&mut self, mut other: Self) {
        self.entries.append(&mut other.entries);
        self.operator_library.append(&mut other.operator_library);
    }
}

fn artifact_from_index(
    environment: &LeanEnvironmentIdentity,
    theorem: &IndexedTheorem,
) -> LeanArtifact {
    LeanArtifact {
        environment: environment.clone(),
        declaration: LeanDeclarationIdentity {
            name: theorem.name.clone(),
            level_params: theorem.level_params.clone(),
        },
        proposition: theorem.proposition.clone(),
        proof_term: theorem.proof_term.clone(),
        dependencies: normalized_names(theorem.dependencies.clone()),
        allowed_axioms: theorem.axioms.clone(),
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LeanMetric {
    ProofNodes,
    ProofDepth,
    EncodedBytes,
    AllowedAxiomCount,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LeanClaim {
    environment: LeanEnvironmentIdentity,
    declaration: LeanDeclarationIdentity,
    proposition: LeanExpr,
    allowed_axioms: Vec<LeanName>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LeanEvidence {
    artifact_digest: [u8; 32],
    dependencies: Vec<LeanName>,
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
        let environment = config.environment_identity()?;
        if corpus
            .entries()
            .iter()
            .any(|entry| entry.artifact.environment != environment)
        {
            return Err(LeanError::IncompatibleEnvironment);
        }
        let substitutions = corpus.operator_library;
        Ok(Self {
            environment: environment.clone(),
            structure: LeanStructure::new(),
            seeds: LeanSeeds {
                entries: Arc::new(corpus.entries),
            },
            operators: LeanOperators::new(substitutions)?,
            kernel: LeanKernel {
                config,
                environment: environment.clone(),
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
            "reflex-lean-v4:mathlib={}:toolchain={}:lean={}:artifact={}:kernel={}:worker={}",
            self.environment.mathlib_commit,
            self.environment.lean_toolchain,
            self.environment.lean_commit,
            self.environment.artifact_format,
            self.environment.kernel_contract,
            self.environment.worker_source_sha256,
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
    declaration: LeanDeclarationIdentity,
    proposition: LeanExpr,
    allowed_axioms: Vec<LeanName>,
    auxiliary: NodeAuxiliary,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
enum NodeAuxiliary {
    BoundVariable {
        index: usize,
    },
    Sort {
        level: LeanLevel,
    },
    Constant {
        name: LeanName,
        levels: Vec<LeanLevel>,
    },
    Application,
    Lambda {
        name: LeanName,
        binder_info: LeanBinderInfo,
    },
    Forall {
        name: LeanName,
        binder_info: LeanBinderInfo,
    },
    Let {
        name: LeanName,
        non_dep: bool,
    },
    Literal {
        literal: LeanLiteral,
    },
    Projection {
        type_name: LeanName,
        index: usize,
    },
}

pub struct LeanStructureView<'a> {
    artifact: &'a LeanArtifact,
    nodes: Vec<&'a LeanExpr>,
    children: Vec<Vec<usize>>,
    preorder_indexes: Vec<usize>,
}

impl<'a> LeanStructureView<'a> {
    fn new(artifact: &'a LeanArtifact) -> Self {
        fn push<'a>(
            expression: &'a LeanExpr,
            nodes: &mut Vec<&'a LeanExpr>,
            children: &mut Vec<Vec<usize>>,
            preorder_indexes: &mut Vec<usize>,
            next_preorder: &mut usize,
        ) -> usize {
            let preorder = *next_preorder;
            *next_preorder = next_preorder.saturating_add(1);
            let mut child_indexes = Vec::new();
            expression.for_each_child(|child| {
                child_indexes.push(push(
                    child,
                    nodes,
                    children,
                    preorder_indexes,
                    next_preorder,
                ));
            });
            let index = nodes.len();
            nodes.push(expression);
            children.push(child_indexes);
            preorder_indexes.push(preorder);
            index
        }
        let mut nodes = Vec::new();
        let mut children = Vec::new();
        let mut preorder_indexes = Vec::new();
        let mut next_preorder = 0;
        push(
            &artifact.proof_term,
            &mut nodes,
            &mut children,
            &mut preorder_indexes,
            &mut next_preorder,
        );
        Self {
            artifact,
            nodes,
            children,
            preorder_indexes,
        }
    }

    fn preorder_index(&self, node: usize) -> Option<usize> {
        self.preorder_indexes.get(node).copied()
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
        self.nodes
            .get(node)
            .map(|expression| constructor_of(expression))
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
            declaration: self.artifact.declaration.clone(),
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
        serde_json::to_vec(self.artifact).map_or(u64::MAX, |bytes| bytes.len() as u64)
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
        let dependencies = artifact_dependencies(&immediate.proposition, &proof_term);
        Ok(LeanArtifact {
            environment: immediate.environment,
            declaration: immediate.declaration,
            proposition: immediate.proposition,
            dependencies,
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
        let view = LeanStructureView::new(artifact);
        let proof_term = (**view
            .nodes
            .get(node)
            .ok_or_else(|| LeanError::InvalidStructure("node is out of range".into()))?)
        .clone();
        Ok(LeanArtifact {
            environment: artifact.environment.clone(),
            declaration: artifact.declaration.clone(),
            proposition: artifact.proposition.clone(),
            dependencies: artifact_dependencies(&artifact.proposition, &proof_term),
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
        let preorder = LeanStructureView::new(artifact)
            .preorder_index(node)
            .ok_or_else(|| LeanError::InvalidStructure("node is out of range".into()))?;
        let proof_term = artifact
            .proof_term
            .replacing(preorder, &replacement.proof_term)
            .ok_or_else(|| LeanError::InvalidStructure("node is out of range".into()))?;
        Ok(LeanArtifact {
            environment: artifact.environment.clone(),
            declaration: artifact.declaration.clone(),
            proposition: artifact.proposition.clone(),
            dependencies: artifact_dependencies(&artifact.proposition, &proof_term),
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
    let invalid =
        || LeanError::InvalidStructure("constructor payload or child arity differs".into());
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
                function: Arc::new(function.clone()),
                argument: Arc::new(argument.clone()),
            })
        }
        (
            LeanConstructor::Lambda,
            NodeAuxiliary::Lambda { name, binder_info },
            [binder_type, body],
        ) => Ok(LeanExpr::Lam {
            name: name.clone(),
            binder_type: Arc::new(binder_type.clone()),
            body: Arc::new(body.clone()),
            binder_info: *binder_info,
        }),
        (
            LeanConstructor::Forall,
            NodeAuxiliary::Forall { name, binder_info },
            [binder_type, body],
        ) => Ok(LeanExpr::ForallE {
            name: name.clone(),
            binder_type: Arc::new(binder_type.clone()),
            body: Arc::new(body.clone()),
            binder_info: *binder_info,
        }),
        (LeanConstructor::Let, NodeAuxiliary::Let { name, non_dep }, [r#type, value, body]) => {
            Ok(LeanExpr::LetE {
                name: name.clone(),
                r#type: Arc::new(r#type.clone()),
                value: Arc::new(value.clone()),
                body: Arc::new(body.clone()),
                non_dep: *non_dep,
            })
        }
        (LeanConstructor::Literal, NodeAuxiliary::Literal { literal }, []) => Ok(LeanExpr::Lit {
            literal: literal.clone(),
        }),
        (
            LeanConstructor::Projection,
            NodeAuxiliary::Projection { type_name, index },
            [subject],
        ) => Ok(LeanExpr::Proj {
            type_name: type_name.clone(),
            index: *index,
            subject: Arc::new(subject.clone()),
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
            return Err(LeanError::InvalidStructure(
                "seed scope exceeds corpus".into(),
            ));
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
                declaration: entry.artifact.declaration.clone(),
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
    retrieval: DonorRetrievalIndex,
    all_proofs: Vec<LeanArtifact>,
    donor_keys: Vec<ProposalProvenance>,
    resident_bytes: u64,
}

// Canonical JSON is the stable measurable payload; this matches the Runtime's
// conservative decoded-bundle multiplier for tree nodes and allocation control data.
const ARTIFACT_RESIDENT_MULTIPLIER: u64 = 12;

#[derive(Default)]
pub struct LeanOperatorScratch {
    retrieval: DonorRetrievalScratch,
}

impl LeanOperators {
    fn new(substitutions: Vec<LeanArtifact>) -> Result<Self, LeanError> {
        let mut donor_keys = Vec::with_capacity(substitutions.len());
        let mut artifact_bytes = 0_u64;
        for artifact in &substitutions {
            let encoded = serde_json::to_vec(artifact)?;
            artifact_bytes = artifact_bytes.saturating_add(encoded.len() as u64);
            donor_keys.push(ProposalProvenance::new(Sha256::digest(encoded).into()));
        }
        let retrieval = DonorRetrievalIndex::build(&substitutions);
        let descriptor =
            |operator, symbol| OperatorDescriptor::new(operator, SymbolId::new(symbol));
        let catalog = vec![
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
        ];
        let resident_bytes = (catalog.capacity() as u64)
            .saturating_mul(std::mem::size_of::<OperatorDescriptor<LeanOperator>>() as u64)
            .saturating_add(catalog.iter().fold(0_u64, |bytes, descriptor| {
                bytes.saturating_add(descriptor.symbol().as_str().len() as u64)
            }))
            .saturating_add(
                (substitutions.capacity() as u64)
                    .saturating_mul(std::mem::size_of::<LeanArtifact>() as u64),
            )
            .saturating_add(artifact_bytes.saturating_mul(ARTIFACT_RESIDENT_MULTIPLIER))
            .saturating_add(
                (donor_keys.capacity() as u64)
                    .saturating_mul(std::mem::size_of::<ProposalProvenance>() as u64),
            )
            .saturating_add(retrieval.resident_bytes());
        Ok(Self {
            catalog,
            retrieval,
            all_proofs: substitutions,
            donor_keys,
            resident_bytes,
        })
    }

    fn substitutions(
        &self,
        source_index: usize,
        source: &LeanArtifact,
        output: &mut ApplicationWriter<'_, LeanApplication>,
        scratch: &mut LeanOperatorScratch,
    ) {
        let limit = output.remaining_capacity();
        let ranked = self.retrieval.rank_prefix(
            &source.proposition,
            &source.proof_term,
            limit.saturating_add(1),
            &mut scratch.retrieval,
        );
        for (proposal_rank, donor) in ranked.iter().take(limit).enumerate() {
            if output.is_full() {
                return;
            }
            let substitution = &self.all_proofs[donor.index];
            let relevance = donor.relevance();
            let margin = retrieval_margin(ranked, proposal_rank);
            output.push(LeanApplication::supported(
                source_index,
                candidate_with_proof(source, substitution.proof_term.clone()),
                source,
                substitution,
                SupportRelationship::retrieved(proposal_rank, donor.tier, relevance, margin),
                self.donor_keys[donor.index],
            ));
        }
    }

    fn applications(
        &self,
        source_index: usize,
        source: &LeanArtifact,
        reverse: bool,
        output: &mut ApplicationWriter<'_, LeanApplication>,
    ) {
        for (index, other) in self.all_proofs.iter().enumerate() {
            if output.is_full() {
                return;
            }
            let (function, argument) = if reverse {
                (other.proof_term.clone(), source.proof_term.clone())
            } else {
                (source.proof_term.clone(), other.proof_term.clone())
            };
            output.push(LeanApplication::supported(
                source_index,
                candidate_with_proof(
                    source,
                    LeanExpr::App {
                        function: Arc::new(function),
                        argument: Arc::new(argument),
                    },
                ),
                source,
                other,
                SupportRelationship::corpus(index),
                self.donor_keys[index],
            ));
        }
    }

    fn rewrites(
        &self,
        source_index: usize,
        node: usize,
        source: &LeanArtifact,
        output: &mut ApplicationWriter<'_, LeanApplication>,
    ) {
        let Some(node) = structural_preorder_index(source, node) else {
            return;
        };
        for (index, other) in self.all_proofs.iter().enumerate() {
            if output.is_full() {
                return;
            }
            if let Some(proof_term) = source.proof_term.replacing(node, &other.proof_term) {
                output.push(LeanApplication::supported(
                    source_index,
                    candidate_with_proof(source, proof_term),
                    source,
                    other,
                    SupportRelationship::corpus(index),
                    self.donor_keys[index],
                ));
            }
        }
    }

    fn factorings(
        &self,
        source_index: usize,
        source: &LeanArtifact,
        output: &mut ApplicationWriter<'_, LeanApplication>,
    ) {
        for (index, shared) in self.all_proofs.iter().enumerate() {
            if output.is_full() {
                return;
            }
            if shared.declaration.level_params != source.declaration.level_params
                || shared.proof_term.node_count() < 2
            {
                continue;
            }
            let Some(body) = source.proof_term.factor_closed(&shared.proof_term) else {
                continue;
            };
            let proof_term = LeanExpr::LetE {
                name: LeanName::from_dotted("_reflex_shared"),
                r#type: Arc::new(shared.proposition.clone()),
                value: Arc::new(shared.proof_term.clone()),
                body: Arc::new(body),
                non_dep: false,
            };
            output.push(LeanApplication::supported(
                source_index,
                candidate_with_proof(source, proof_term),
                source,
                shared,
                SupportRelationship::corpus(index),
                self.donor_keys[index],
            ));
        }
    }

    fn anti_unifications(
        &self,
        source_index: usize,
        node: usize,
        source: &LeanArtifact,
        output: &mut ApplicationWriter<'_, LeanApplication>,
    ) {
        let Some(node) = structural_preorder_index(source, node) else {
            return;
        };
        let Some(target) = source.proof_term.expression_at(node) else {
            return;
        };
        for (index, analogous) in self.all_proofs.iter().enumerate() {
            if output.is_full() {
                return;
            }
            let Some((relative, replacement)) =
                analogous_replacement(target, &analogous.proof_term)
            else {
                continue;
            };
            let Some(rewritten_target) = target.replacing(relative, &replacement) else {
                continue;
            };
            let Some(proof_term) = source.proof_term.replacing(node, &rewritten_target) else {
                continue;
            };
            output.push(LeanApplication::supported(
                source_index,
                candidate_with_proof(source, proof_term),
                source,
                analogous,
                SupportRelationship::corpus(index),
                self.donor_keys[index],
            ));
        }
    }

    fn contraction(
        source_index: usize,
        node: usize,
        source: &LeanArtifact,
        contract: fn(&LeanExpr) -> Option<LeanExpr>,
        output: &mut ApplicationWriter<'_, LeanApplication>,
    ) {
        let Some(node) = structural_preorder_index(source, node) else {
            return;
        };
        if let Some(proof_term) = contracted_at(&source.proof_term, node, contract) {
            output.push(LeanApplication::unassisted(
                source_index,
                candidate_with_proof(source, proof_term),
            ));
        }
    }

    fn generalizations(
        &self,
        source_index: usize,
        source: &LeanArtifact,
        output: &mut ApplicationWriter<'_, LeanApplication>,
    ) {
        for (index, general) in self.all_proofs.iter().enumerate() {
            if output.is_full() {
                return;
            }
            if general.declaration.level_params != source.declaration.level_params {
                continue;
            }
            if let Some(proof_term) = generalized_application(
                &general.proposition,
                &general.proof_term,
                &source.proposition,
            ) {
                output.push(LeanApplication::supported(
                    source_index,
                    candidate_with_proof(source, proof_term),
                    source,
                    general,
                    SupportRelationship::corpus(index),
                    self.donor_keys[index],
                ));
            }
        }
    }
}

pub struct LeanApplication {
    source_index: usize,
    candidate: LeanArtifact,
    proposal_features: ProposalFeatures,
    proposal_provenance: Option<ProposalProvenance>,
}

#[derive(Clone, Copy)]
struct SupportRelationship {
    ordinal: usize,
    tier: Option<RetrievalTier>,
    relevance: f32,
    margin: f32,
}

impl SupportRelationship {
    const fn corpus(ordinal: usize) -> Self {
        Self {
            ordinal,
            tier: None,
            relevance: 0.0,
            margin: 0.0,
        }
    }

    const fn retrieved(ordinal: usize, tier: RetrievalTier, relevance: f32, margin: f32) -> Self {
        Self {
            ordinal,
            tier: Some(tier),
            relevance,
            margin,
        }
    }
}

impl LeanApplication {
    fn supported(
        source_index: usize,
        candidate: LeanArtifact,
        source: &LeanArtifact,
        support: &LeanArtifact,
        relationship: SupportRelationship,
        proposal_provenance: ProposalProvenance,
    ) -> Self {
        Self {
            source_index,
            candidate,
            proposal_features: proposal_features(source, support, relationship),
            proposal_provenance: Some(proposal_provenance),
        }
    }

    fn unassisted(source_index: usize, candidate: LeanArtifact) -> Self {
        Self {
            source_index,
            candidate,
            proposal_features: ProposalFeatures::default(),
            proposal_provenance: None,
        }
    }
}

fn proposal_features(
    source: &LeanArtifact,
    support: &LeanArtifact,
    relationship: SupportRelationship,
) -> ProposalFeatures {
    let rank = u16::try_from(relationship.ordinal.saturating_add(1)).unwrap_or(u16::MAX);
    let source_proposition_nodes = bounded_nodes(source.proposition.node_count());
    let support_proposition_nodes = bounded_nodes(support.proposition.node_count());
    let source_proof_nodes = bounded_nodes(source.proof_term.node_count());
    let support_proof_nodes = bounded_nodes(support.proof_term.node_count());
    ProposalFeatures::new([
        f32::from(relationship.tier == Some(RetrievalTier::Exact)),
        f32::from(relationship.tier == Some(RetrievalTier::Structural)),
        relationship.relevance,
        1.0 / f32::from(rank),
        signed_reduction(source_proposition_nodes, support_proposition_nodes),
        signed_reduction(source_proof_nodes, support_proof_nodes),
        f32::from(source.allowed_axioms == support.allowed_axioms),
        relationship.margin,
    ])
}

fn retrieval_margin(ranked: &[RetrievedDonor], index: usize) -> f32 {
    (ranked[index].relevance() - ranked.get(index + 1).map_or(0.0, |next| next.relevance()))
        .max(0.0)
}

fn bounded_nodes(nodes: usize) -> f32 {
    f32::from(u16::try_from(nodes).unwrap_or(u16::MAX))
}

fn signed_reduction(source: f32, support: f32) -> f32 {
    ((source - support) / source.max(1.0)).clamp(-1.0, 1.0)
}

impl OperatorAlgebra<LeanDomain> for LeanOperators {
    type Operator = LeanOperator;
    type Application = LeanApplication;
    type Scratch = LeanOperatorScratch;

    fn catalog(&self) -> &[OperatorDescriptor<Self::Operator>] {
        &self.catalog
    }

    fn resident_bytes(&self) -> u64 {
        self.resident_bytes
    }

    fn scratch_resident_bytes(&self, output_capacity: usize) -> u64 {
        self.retrieval
            .scratch_resident_bytes(output_capacity.saturating_add(1))
    }

    fn enumerate_legal(
        &self,
        requests: OperatorEnumerationBatch<'_, LeanDomain, Self::Operator>,
        output: &mut ApplicationWriter<'_, Self::Application>,
        scratch: &mut Self::Scratch,
    ) -> Result<(), LeanError> {
        for location in requests.locations() {
            let source = requests
                .artifacts()
                .get(location.artifact_index())
                .ok_or_else(|| {
                    LeanError::InvalidStructure("artifact index is out of range".into())
                })?;
            for operator in requests.operators() {
                match operator {
                    LeanOperator::ProofSubstitution
                        if is_root_location(source, location.node_index()) =>
                    {
                        self.substitutions(location.artifact_index(), source, output, scratch);
                    }
                    LeanOperator::Application
                        if is_root_location(source, location.node_index()) =>
                    {
                        self.applications(location.artifact_index(), source, false, output);
                    }
                    LeanOperator::Composition
                        if is_root_location(source, location.node_index()) =>
                    {
                        self.applications(location.artifact_index(), source, true, output);
                    }
                    LeanOperator::Rewriting => self.rewrites(
                        location.artifact_index(),
                        location.node_index(),
                        source,
                        output,
                    ),
                    LeanOperator::Factoring if is_root_location(source, location.node_index()) => {
                        self.factorings(location.artifact_index(), source, output);
                    }
                    LeanOperator::AntiUnification => self.anti_unifications(
                        location.artifact_index(),
                        location.node_index(),
                        source,
                        output,
                    ),
                    LeanOperator::Abstraction => Self::contraction(
                        location.artifact_index(),
                        location.node_index(),
                        source,
                        LeanExpr::eta_contract,
                        output,
                    ),
                    LeanOperator::Normalization => Self::contraction(
                        location.artifact_index(),
                        location.node_index(),
                        source,
                        LeanExpr::beta_or_zeta_contract,
                        output,
                    ),
                    LeanOperator::VerifiedGeneralization
                        if is_root_location(source, location.node_index()) =>
                    {
                        self.generalizations(location.artifact_index(), source, output);
                    }
                    LeanOperator::Factoring
                    | LeanOperator::Application
                    | LeanOperator::Composition
                    | LeanOperator::VerifiedGeneralization
                    | LeanOperator::ProofSubstitution => {}
                }
                if output.is_full() {
                    return Ok(());
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
            output.push_with_provenance(
                application.source_index,
                application.candidate.clone(),
                application.proposal_features,
                application.proposal_provenance,
            );
        }
        Ok(())
    }
}

pub struct LeanKernel {
    config: LeanWorkerConfig,
    environment: LeanEnvironmentIdentity,
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
            || seed.environment != self.environment
            || seed.declaration != candidate.declaration
        {
            return Err(LeanError::IncompatibleEnvironment);
        }
        Ok(LeanClaim {
            environment: seed.environment.clone(),
            declaration: seed.declaration.clone(),
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
            .map(|request| verification_item(request.claim, request.candidate))
            .collect::<Vec<_>>();
        match self.verify_items(&items, requests.allowance().elapsed_time()) {
            Ok((results, usage)) => {
                for (request, result) in requests.requests().iter().zip(results) {
                    let dependencies = normalized_names(result.dependencies.clone());
                    if result.accepted
                        && request.claim.environment == request.candidate.environment
                        && request.claim.declaration == request.candidate.declaration
                        && request.claim.allowed_axioms == request.candidate.allowed_axioms
                        && request.candidate.dependencies == dependencies
                    {
                        let evidence = artifact_digest(request.candidate).map(|artifact_digest| {
                            LeanEvidence {
                                artifact_digest,
                                dependencies,
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
            .map(|record| verification_item(record.claim, record.artifact))
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
    let dependencies = normalized_names(result.dependencies.clone());
    result.accepted
        && record.kernel_revision == KERNEL_REVISION
        && record.claim.environment == record.artifact.environment
        && record.claim.declaration == record.artifact.declaration
        && record.claim.allowed_axioms == record.artifact.allowed_axioms
        && record.evidence.environment == record.artifact.environment
        && record.evidence.dependencies.as_slice() == dependencies.as_slice()
        && record.artifact.dependencies.as_slice() == dependencies.as_slice()
        && record.evidence.axioms == result.axioms
        && artifact_digest(record.artifact)
            .is_ok_and(|digest| digest == record.evidence.artifact_digest)
}

fn verification_item(claim: &LeanClaim, artifact: &LeanArtifact) -> VerificationItem {
    VerificationItem {
        level_params: artifact.declaration.level_params.clone(),
        claim_proposition: claim.proposition.clone(),
        candidate_proposition: artifact.proposition.clone(),
        proof_term: artifact.proof_term.clone(),
        allowed_axioms: claim.allowed_axioms.clone(),
    }
}

fn artifact_dependencies(proposition: &LeanExpr, proof_term: &LeanExpr) -> Vec<LeanName> {
    let mut dependencies = proposition.used_constants();
    dependencies.extend(proof_term.used_constants());
    normalized_names(dependencies)
}

fn candidate_with_proof(source: &LeanArtifact, proof_term: LeanExpr) -> LeanArtifact {
    LeanArtifact {
        environment: source.environment.clone(),
        declaration: source.declaration.clone(),
        proposition: source.proposition.clone(),
        dependencies: artifact_dependencies(&source.proposition, &proof_term),
        proof_term,
        allowed_axioms: source.allowed_axioms.clone(),
    }
}

fn is_root_location(source: &LeanArtifact, node: usize) -> bool {
    node.checked_add(1) == Some(source.proof_term.node_count())
}

fn structural_preorder_index(source: &LeanArtifact, node: usize) -> Option<usize> {
    LeanStructureView::new(source).preorder_index(node)
}

fn contracted_at(
    expression: &LeanExpr,
    node: usize,
    contract: fn(&LeanExpr) -> Option<LeanExpr>,
) -> Option<LeanExpr> {
    let contracted = contract(expression.expression_at(node)?)?;
    expression.replacing(node, &contracted)
}

fn analogous_replacement(left: &LeanExpr, right: &LeanExpr) -> Option<(usize, LeanExpr)> {
    fn same_header(left: &LeanExpr, right: &LeanExpr) -> bool {
        match (left, right) {
            (LeanExpr::Bvar { index: left }, LeanExpr::Bvar { index: right }) => left == right,
            (LeanExpr::Sort { level: left }, LeanExpr::Sort { level: right }) => left == right,
            (
                LeanExpr::Const {
                    name: left_name,
                    levels: left_levels,
                },
                LeanExpr::Const {
                    name: right_name,
                    levels: right_levels,
                },
            ) => left_name == right_name && left_levels == right_levels,
            (LeanExpr::App { .. }, LeanExpr::App { .. }) => true,
            (
                LeanExpr::Lam {
                    name: left_name,
                    binder_info: left_info,
                    ..
                },
                LeanExpr::Lam {
                    name: right_name,
                    binder_info: right_info,
                    ..
                },
            )
            | (
                LeanExpr::ForallE {
                    name: left_name,
                    binder_info: left_info,
                    ..
                },
                LeanExpr::ForallE {
                    name: right_name,
                    binder_info: right_info,
                    ..
                },
            ) => left_name == right_name && left_info == right_info,
            (
                LeanExpr::LetE {
                    name: left_name,
                    non_dep: left_non_dep,
                    ..
                },
                LeanExpr::LetE {
                    name: right_name,
                    non_dep: right_non_dep,
                    ..
                },
            ) => left_name == right_name && left_non_dep == right_non_dep,
            (LeanExpr::Lit { literal: left }, LeanExpr::Lit { literal: right }) => left == right,
            (
                LeanExpr::Proj {
                    type_name: left_name,
                    index: left_index,
                    ..
                },
                LeanExpr::Proj {
                    type_name: right_name,
                    index: right_index,
                    ..
                },
            ) => left_name == right_name && left_index == right_index,
            _ => false,
        }
    }

    let mut left_nodes = Vec::new();
    left.visit(&mut |expression| left_nodes.push(expression.clone()));
    let mut right_nodes = Vec::new();
    right.visit(&mut |expression| right_nodes.push(expression.clone()));
    left_nodes
        .into_iter()
        .zip(right_nodes)
        .enumerate()
        .find_map(|(index, (left, right))| (!same_header(&left, &right)).then_some((index, right)))
}

fn generalized_application(
    proposition: &LeanExpr,
    proof_term: &LeanExpr,
    target: &LeanExpr,
) -> Option<LeanExpr> {
    let mut body = proposition;
    let mut binder_count = 0usize;
    while let LeanExpr::ForallE { body: next, .. } = body {
        binder_count = binder_count.checked_add(1)?;
        body = next;
    }
    if binder_count == 0 {
        return None;
    }
    let mut bindings = vec![None; binder_count];
    if !match_generalized(body, target, binder_count, 0, &mut bindings) {
        return None;
    }
    let mut result = proof_term.clone();
    for argument in bindings.into_iter().rev() {
        result = LeanExpr::App {
            function: Arc::new(result),
            argument: Arc::new(argument?),
        };
    }
    Some(result)
}

fn match_generalized(
    pattern: &LeanExpr,
    target: &LeanExpr,
    binder_count: usize,
    depth: usize,
    bindings: &mut [Option<LeanExpr>],
) -> bool {
    if let LeanExpr::Bvar { index } = pattern
        && *index >= depth
        && *index < depth.saturating_add(binder_count)
        && target.is_closed()
    {
        let binding = &mut bindings[*index - depth];
        return binding.as_ref().is_none_or(|bound| bound == target) && {
            *binding = Some(target.clone());
            true
        };
    }
    match (pattern, target) {
        (LeanExpr::Bvar { index: left }, LeanExpr::Bvar { index: right }) => left == right,
        (LeanExpr::Sort { level: left }, LeanExpr::Sort { level: right }) => left == right,
        (
            LeanExpr::Const {
                name: left_name,
                levels: left_levels,
            },
            LeanExpr::Const {
                name: right_name,
                levels: right_levels,
            },
        ) => left_name == right_name && left_levels == right_levels,
        (
            LeanExpr::App {
                function: left_function,
                argument: left_argument,
            },
            LeanExpr::App {
                function: right_function,
                argument: right_argument,
            },
        ) => {
            match_generalized(left_function, right_function, binder_count, depth, bindings)
                && match_generalized(left_argument, right_argument, binder_count, depth, bindings)
        }
        (LeanExpr::Lam { .. }, LeanExpr::Lam { .. })
        | (LeanExpr::ForallE { .. }, LeanExpr::ForallE { .. }) => {
            match_generalized_binder(pattern, target, binder_count, depth, bindings)
        }
        (LeanExpr::LetE { .. }, LeanExpr::LetE { .. }) => {
            match_generalized_let(pattern, target, binder_count, depth, bindings)
        }
        (LeanExpr::Lit { literal: left }, LeanExpr::Lit { literal: right }) => left == right,
        (
            LeanExpr::Proj {
                type_name: left_name,
                index: left_index,
                subject: left_subject,
            },
            LeanExpr::Proj {
                type_name: right_name,
                index: right_index,
                subject: right_subject,
            },
        ) => {
            left_name == right_name
                && left_index == right_index
                && match_generalized(left_subject, right_subject, binder_count, depth, bindings)
        }
        _ => false,
    }
}

fn match_generalized_binder(
    pattern: &LeanExpr,
    target: &LeanExpr,
    binder_count: usize,
    depth: usize,
    bindings: &mut [Option<LeanExpr>],
) -> bool {
    let (
        LeanExpr::Lam {
            name: left_name,
            binder_type: left_type,
            body: left_body,
            binder_info: left_info,
        }
        | LeanExpr::ForallE {
            name: left_name,
            binder_type: left_type,
            body: left_body,
            binder_info: left_info,
        },
    ) = (pattern,)
    else {
        return false;
    };
    let (
        LeanExpr::Lam {
            name: right_name,
            binder_type: right_type,
            body: right_body,
            binder_info: right_info,
        }
        | LeanExpr::ForallE {
            name: right_name,
            binder_type: right_type,
            body: right_body,
            binder_info: right_info,
        },
    ) = (target,)
    else {
        return false;
    };
    left_name == right_name
        && left_info == right_info
        && match_generalized(left_type, right_type, binder_count, depth, bindings)
        && match_generalized(
            left_body,
            right_body,
            binder_count,
            depth.saturating_add(1),
            bindings,
        )
}

fn match_generalized_let(
    pattern: &LeanExpr,
    target: &LeanExpr,
    binder_count: usize,
    depth: usize,
    bindings: &mut [Option<LeanExpr>],
) -> bool {
    let LeanExpr::LetE {
        name: left_name,
        r#type: left_type,
        value: left_value,
        body: left_body,
        non_dep: left_non_dep,
    } = pattern
    else {
        return false;
    };
    let LeanExpr::LetE {
        name: right_name,
        r#type: right_type,
        value: right_value,
        body: right_body,
        non_dep: right_non_dep,
    } = target
    else {
        return false;
    };
    left_name == right_name
        && left_non_dep == right_non_dep
        && match_generalized(left_type, right_type, binder_count, depth, bindings)
        && match_generalized(left_value, right_value, binder_count, depth, bindings)
        && match_generalized(
            left_body,
            right_body,
            binder_count,
            depth.saturating_add(1),
            bindings,
        )
}

fn artifact_digest(artifact: &LeanArtifact) -> Result<[u8; 32], LeanError> {
    let encoded = serde_json::to_vec(artifact)?;
    Ok(Sha256::digest(encoded).into())
}

fn normalized_names(mut names: Vec<LeanName>) -> Vec<LeanName> {
    names.sort_unstable();
    names.dedup();
    names
}

fn sha256_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    Sha256::digest(bytes)
        .iter()
        .fold(String::with_capacity(64), |mut output, byte| {
            let _ = write!(output, "{byte:02x}");
            output
        })
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
                MeasurementDescriptor::new(
                    LeanMetric::AllowedAxiomCount,
                    SymbolId::new("allowed-axiom-count"),
                ),
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
            output.push(
                index,
                LeanMetric::ProofNodes,
                artifact.proof_term.node_count() as u64,
            );
            output.push(
                index,
                LeanMetric::ProofDepth,
                artifact.proof_term.depth() as u64,
            );
            output.push(index, LeanMetric::EncodedBytes, scratch.len() as u64);
            output.push(
                index,
                LeanMetric::AllowedAxiomCount,
                artifact.allowed_axioms.len() as u64,
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
        artifact_format: ARTIFACT_FORMAT_VERSION,
        kernel_contract: KERNEL_CONTRACT_VERSION,
        worker_source_sha256: sha256_hex(include_bytes!("../worker/ReflexLeanWorker.lean")),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use cpu_time::ProcessTime;
    use reflex::{OperatorAlgebra, StructuralProtocol, StructuralView};

    use super::{
        LeanArtifact, LeanConstructor, LeanExpr, LeanOperatorScratch, LeanOperators, LeanStructure,
        RetrievalTier, pinned_environment_identity,
    };
    use crate::ast::{LeanBinderInfo, LeanDeclarationIdentity, LeanName};
    use crate::worker::{LeanWorker, LeanWorkerConfig};

    #[test]
    fn cloned_proof_terms_share_immutable_subtrees() {
        let proof = LeanExpr::App {
            function: Arc::new(LeanExpr::Bvar { index: 0 }),
            argument: Arc::new(LeanExpr::Bvar { index: 1 }),
        };
        let cloned = proof.clone();
        let (
            LeanExpr::App {
                function: original, ..
            },
            LeanExpr::App {
                function: copied, ..
            },
        ) = (&proof, &cloned)
        else {
            panic!("the proof shape changed during cloning");
        };

        assert!(Arc::ptr_eq(original, copied));
    }

    fn artifact() -> LeanArtifact {
        let nat = LeanExpr::constant(LeanName::from_dotted("Nat"), vec![]);
        LeanArtifact {
            environment: pinned_environment_identity(),
            declaration: LeanDeclarationIdentity {
                name: LeanName::from_dotted("Reflex.test"),
                level_params: vec![],
            },
            proposition: LeanExpr::ForallE {
                name: LeanName::from_dotted("n"),
                binder_type: Arc::new(nat.clone()),
                body: Arc::new(nat.clone()),
                binder_info: LeanBinderInfo::Default,
            },
            proof_term: LeanExpr::Lam {
                name: LeanName::from_dotted("n"),
                binder_type: Arc::new(nat),
                body: Arc::new(LeanExpr::Bvar { index: 0 }),
                binder_info: LeanBinderInfo::Default,
            },
            dependencies: vec![LeanName::from_dotted("Nat")],
            allowed_axioms: vec![],
        }
    }

    #[test]
    fn proposal_features_distinguish_exact_and_unrelated_library_support() {
        let source = artifact();
        let exact = LeanArtifact {
            proof_term: LeanExpr::constant(LeanName::from_dotted("exact"), vec![]),
            ..source.clone()
        };
        let unrelated = LeanArtifact {
            proposition: LeanExpr::constant(LeanName::from_dotted("False"), vec![]),
            proof_term: LeanExpr::constant(LeanName::from_dotted("unrelated"), vec![]),
            ..source.clone()
        };

        let exact_features = super::proposal_features(
            &source,
            &exact,
            super::SupportRelationship::retrieved(0, RetrievalTier::Exact, 1.0, 1.0),
        );
        let unrelated_features = super::proposal_features(
            &source,
            &unrelated,
            super::SupportRelationship::retrieved(1, RetrievalTier::Fallback, 0.0, 0.0),
        );

        let exact_values = exact_features.as_array();
        let unrelated_values = unrelated_features.as_array();
        assert_eq!(
            [exact_values[0].to_bits(), exact_values[1].to_bits()],
            [1.0_f32.to_bits(), 0.0_f32.to_bits()]
        );
        assert_eq!(
            [unrelated_values[0].to_bits(), unrelated_values[1].to_bits()],
            [0.0_f32.to_bits(), 0.0_f32.to_bits()]
        );
        assert_ne!(exact_features, unrelated_features);
    }

    #[test]
    fn donor_retrieval_prioritizes_exact_then_structurally_related_propositions() {
        let source = artifact();
        let unrelated = LeanArtifact {
            proposition: LeanExpr::constant(LeanName::from_dotted("False"), vec![]),
            proof_term: LeanExpr::constant(LeanName::from_dotted("unrelated"), vec![]),
            ..source.clone()
        };
        let boolean = LeanExpr::constant(LeanName::from_dotted("Bool"), vec![]);
        let structurally_related = LeanArtifact {
            proposition: LeanExpr::ForallE {
                name: LeanName::from_dotted("b"),
                binder_type: Arc::new(boolean.clone()),
                body: Arc::new(boolean),
                binder_info: LeanBinderInfo::Default,
            },
            proof_term: LeanExpr::constant(LeanName::from_dotted("related"), vec![]),
            ..source.clone()
        };
        let exact = LeanArtifact {
            proof_term: LeanExpr::constant(LeanName::from_dotted("exact"), vec![]),
            ..source.clone()
        };
        let operators =
            LeanOperators::new(vec![unrelated, structurally_related, exact.clone()]).unwrap();
        let mut scratch = LeanOperatorScratch::default();

        assert_eq!(
            operators
                .retrieval
                .rank_prefix(
                    &source.proposition,
                    &source.proof_term,
                    3,
                    &mut scratch.retrieval,
                )
                .iter()
                .map(|donor| donor.index)
                .collect::<Vec<_>>(),
            [2, 1, 0],
            "bounded generation must see exact and structurally relevant donors before corpus-order noise"
        );

        let mut applications = Vec::new();
        operators.substitutions(
            0,
            &source,
            &mut reflex::ApplicationWriter::new(&mut applications),
            &mut scratch,
        );
        assert_eq!(
            applications
                .iter()
                .map(|application| application.candidate.proof_term.clone())
                .collect::<Vec<_>>(),
            [
                LeanExpr::constant(LeanName::from_dotted("exact"), vec![]),
                LeanExpr::constant(LeanName::from_dotted("related"), vec![]),
                LeanExpr::constant(LeanName::from_dotted("unrelated"), vec![]),
            ]
        );
        assert_eq!(
            applications
                .iter()
                .map(|application| application.proposal_features.as_array()[3].to_bits())
                .collect::<Vec<_>>(),
            [
                1.0_f32.to_bits(),
                0.5_f32.to_bits(),
                (1.0_f32 / 3.0).to_bits()
            ],
            "proposal rank must describe emitted retrieval order rather than original corpus position"
        );
        assert!(
            applications
                .iter()
                .all(|application| application.proposal_provenance.is_some()),
            "every supported proposal retains its verified donor identity"
        );

        let second_exact = LeanArtifact {
            proof_term: LeanExpr::constant(LeanName::from_dotted("second-exact"), vec![]),
            ..source.clone()
        };
        let tied = LeanOperators::new(vec![exact, second_exact]).unwrap();
        let tied_ranked = tied.retrieval.rank_prefix(
            &source.proposition,
            &source.proof_term,
            2,
            &mut scratch.retrieval,
        );
        assert_eq!(
            super::retrieval_margin(tied_ranked, 0).to_bits(),
            0.0_f32.to_bits(),
            "the boundary margin must compare against the first omitted donor"
        );
    }

    #[test]
    #[ignore = "requires the pinned Lean/mathlib installation; Development diagnostic"]
    #[expect(
        clippy::too_many_lines,
        reason = "the complete frozen Development corpus is intentionally visible in one diagnostic"
    )]
    fn premise_retrieval_recalls_known_shorter_proofs_in_the_v11_corpus() {
        const TRAINING: &[&str] = &[
            "Nat.bitwise.eq_1",
            "Filter.EventuallyLE.refl",
            "exists_eq_ciSup_of_not_isSuccPrelimit'",
            "LinearMap.map_coprod_prod",
            "Nat.card_divisors",
            "MeasureTheory.MeasurePreserving.setLIntegral_comp_emb",
            "Init.Data.Int.Lemmas._auxLemma.1",
            "MeasureTheory.Supermartingale.setIntegral_le",
            "Left.mul_lt_one_of_le_of_lt",
            "Mathlib.Data.Fin.Basic._auxLemma.9",
            "Relation.EqvGen.is_equivalence",
            "dist_triangle",
            "contMDiffAt_finset_prod'",
            "Lean.Omega.Fin.not_lt",
            "lt_of_eq_of_lt",
            "mul_lt_mul_left'",
        ];
        const TRAINING_LIBRARY: &[&str] = &[
            "exists_eq_ciSup_of_not_isSuccLimit'",
            "mul_lt_one_of_le_of_lt",
            "smoothAt_finset_prod'",
            "ArithmeticFunction.card_divisors",
            "Eq.trans_lt",
            "EqvGen.is_equivalence",
            "Fin.not_lt",
            "Int.ofNat_add_ofNat",
            "Int.ofNat_add_out",
            "LinearMap.coprod_map_prod",
            "OrderedCommGroup.mul_lt_mul_left'",
            "PseudoMetricSpace.dist_triangle",
            "Filter.EventuallyLE.rfl",
            "MeasureTheory.MeasurePreserving.set_lintegral_comp_emb",
            "MeasureTheory.Supermartingale.set_integral_le",
            "Nat.bitwise.eq_def",
            "Batteries.Classes.Order._auxLemma.7",
            "Mathlib.AlgebraicTopology.ExtraDegeneracy._auxLemma.5",
            "Mathlib.AlgebraicTopology.SimplexCategory._auxLemma.13",
            "Mathlib.CategoryTheory.ComposableArrows._auxLemma.3",
            "Mathlib.Order.JordanHolder._auxLemma.8",
            "Init.Data.Fin.Lemmas._auxLemma.15",
            "Init.Data.Int.DivModLemmas._auxLemma.13",
            "Mathlib.Algebra.BigOperators.Fin._auxLemma.3",
            "Mathlib.AlgebraicTopology.DoldKan.Faces._auxLemma.5",
            "Mathlib.LinearAlgebra.Matrix.ZPow._auxLemma.4",
            "Mathlib.Data.Int.Cast.Basic._auxLemma.6",
        ];
        const COLLAPSES: &[(&str, &str)] = &[
            (
                "CompleteLattice.isCompactlyGenerated_of_wellFoundedGT",
                "CompleteLattice.isCompactlyGenerated_of_wellFounded",
            ),
            (
                "Matrix.det_updateCol_eq_zero",
                "Matrix.det_updateColumn_eq_zero",
            ),
            ("hfdifferential_apply", "apply_hfdifferential"),
            (
                "Matrix.fromCols_mul_fromRows",
                "Matrix.fromColumns_mul_fromRows",
            ),
        ];
        let lake = std::env::var_os("REFLEX_LEAN_LAKE").expect("REFLEX_LEAN_LAKE is required");
        let mathlib =
            std::env::var_os("REFLEX_LEAN_MATHLIB").expect("REFLEX_LEAN_MATHLIB is required");
        let worker = LeanWorker::start(&LeanWorkerConfig::pinned(lake, mathlib)).unwrap();
        let seed_names = COLLAPSES
            .iter()
            .map(|(seed, _)| LeanName::from_dotted(seed))
            .collect::<Vec<_>>();
        let library_names = TRAINING
            .iter()
            .chain(TRAINING_LIBRARY)
            .copied()
            .chain(COLLAPSES.iter().map(|(_, donor)| *donor))
            .map(LeanName::from_dotted)
            .collect::<Vec<_>>();
        let seeds = worker.fetch(&seed_names).unwrap();
        let library = worker.fetch(&library_names).unwrap();
        let corpus = super::LeanCorpus::verified_seeds_with_library(&worker, seeds, library)
            .expect("the retained Development artifacts must still pass the pinned kernel");
        let donor_indexes = corpus
            .operator_library()
            .iter()
            .enumerate()
            .map(|(index, artifact)| (artifact.declaration.name.to_string(), index))
            .collect::<HashMap<_, _>>();
        let build_cpu_started = ProcessTime::now();
        let operators = LeanOperators::new(corpus.operator_library().to_vec()).unwrap();
        let build_cpu = build_cpu_started.elapsed();
        let mut scratch = LeanOperatorScratch::default();
        let query_cpu_started = ProcessTime::now();
        let ranks = corpus
            .entries()
            .iter()
            .zip(COLLAPSES)
            .map(|(entry, (_, donor))| {
                let expected = donor_indexes[*donor];
                operators
                    .retrieval
                    .rank_prefix(
                        &entry.artifact.proposition,
                        &entry.artifact.proof_term,
                        16,
                        &mut scratch.retrieval,
                    )
                    .iter()
                    .position(|candidate| candidate.index == expected)
                    .map(|rank| rank + 1)
            })
            .collect::<Vec<_>>();
        let query_cpu = query_cpu_started.elapsed();
        let recall = [1, 4, 8, 16].map(|cutoff| {
            ranks
                .iter()
                .filter(|rank| rank.is_some_and(|rank| rank <= cutoff))
                .count()
        });

        eprintln!(
            "premise retrieval ranks={ranks:?} recall@1/4/8/16={recall:?}/{} build_cpu={build_cpu:?} query_cpu={query_cpu:?} index_and_library_bytes={} scratch_bytes={}",
            COLLAPSES.len(),
            operators.resident_bytes(),
            operators.scratch_resident_bytes(16),
        );
        assert_eq!(
            recall[3],
            COLLAPSES.len(),
            "every known shorter proof must enter the bounded top-16 generation window"
        );
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
        let root = original.proof_term.node_count() - 1;
        assert_eq!(rebuild(&structure, &original, root), original);
        let view = structure.view(&original);
        for parent in 0..view.node_count() {
            let mut children = Vec::new();
            assert!(view.write_children(parent, &mut children));
            assert!(children.into_iter().all(|child| child < parent));
        }
        assert!(structure.schema().constructors.iter().all(|descriptor| {
            descriptor.immediate_arity() == reflex::ImmediateArity::Variable
        }));
    }

    #[test]
    fn extract_and_replace_preserve_the_seed_relative_claim() {
        let structure = LeanStructure::new();
        let original = artifact();
        let extracted = structure.extract(&original, 1, &mut vec![]).unwrap();
        assert_eq!(extracted.proof_term, LeanExpr::Bvar { index: 0 });
        let replacement = LeanArtifact {
            proof_term: LeanExpr::constant(LeanName::from_dotted("Nat.zero"), vec![]),
            ..extracted
        };
        let replaced = structure
            .replace(&original, 1, &replacement, &mut vec![])
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

    #[test]
    fn normalization_and_abstraction_preserve_de_bruijn_structure() {
        let nat = LeanExpr::constant(LeanName::from_dotted("Nat"), vec![]);
        let zero = LeanExpr::constant(LeanName::from_dotted("Nat.zero"), vec![]);
        let beta = LeanExpr::App {
            function: Arc::new(LeanExpr::Lam {
                name: LeanName::from_dotted("x"),
                binder_type: Arc::new(nat.clone()),
                body: Arc::new(LeanExpr::Bvar { index: 0 }),
                binder_info: LeanBinderInfo::Default,
            }),
            argument: Arc::new(zero.clone()),
        };
        assert_eq!(beta.beta_or_zeta_contract(), Some(zero));

        let function = LeanExpr::constant(LeanName::from_dotted("f"), vec![]);
        let eta = LeanExpr::Lam {
            name: LeanName::from_dotted("x"),
            binder_type: Arc::new(nat),
            body: Arc::new(LeanExpr::App {
                function: Arc::new(function.clone()),
                argument: Arc::new(LeanExpr::Bvar { index: 0 }),
            }),
            binder_info: LeanBinderInfo::Default,
        };
        assert_eq!(eta.eta_contract(), Some(function));
    }

    #[test]
    fn factoring_round_trips_through_kernel_zeta_semantics() {
        let shared = LeanExpr::App {
            function: Arc::new(LeanExpr::constant(LeanName::from_dotted("f"), vec![])),
            argument: Arc::new(LeanExpr::constant(LeanName::from_dotted("x"), vec![])),
        };
        let original = LeanExpr::App {
            function: Arc::new(shared.clone()),
            argument: Arc::new(shared.clone()),
        };
        let body = original.factor_closed(&shared).unwrap();
        let factored = LeanExpr::LetE {
            name: LeanName::from_dotted("shared"),
            r#type: Arc::new(LeanExpr::constant(LeanName::from_dotted("T"), vec![])),
            value: Arc::new(shared),
            body: Arc::new(body),
            non_dep: false,
        };
        assert_eq!(factored.beta_or_zeta_contract(), Some(original));
    }

    #[test]
    fn verified_generalization_instantiates_all_syntactic_parameters() {
        let binder = LeanExpr::constant(LeanName::from_dotted("T"), vec![]);
        let proposition = LeanExpr::ForallE {
            name: LeanName::from_dotted("x"),
            binder_type: Arc::new(binder),
            body: Arc::new(LeanExpr::Bvar { index: 0 }),
            binder_info: LeanBinderInfo::Default,
        };
        let proof = LeanExpr::constant(LeanName::from_dotted("general"), vec![]);
        let target = LeanExpr::constant(LeanName::from_dotted("specific"), vec![]);
        assert_eq!(
            super::generalized_application(&proposition, &proof, &target),
            Some(LeanExpr::App {
                function: Arc::new(proof),
                argument: Arc::new(target),
            })
        );
    }
}
