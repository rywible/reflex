//! Wrela Kernel Package v1 — content-addressed semantic package schema.

use reflex_canonical::{CanonicalEncode, CanonicalError, CanonicalWriter, content_id};
use reflex_types::Digest;
use serde::{Deserialize, Serialize};
use std::time::Instant;

pub const PACKAGE_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IrNode {
    ConstDyadic {
        num: i64,
        den: i64,
    },
    Var {
        name: String,
    },
    Add {
        lhs: u32,
        rhs: u32,
    },
    Sub {
        lhs: u32,
        rhs: u32,
    },
    Mul {
        lhs: u32,
        rhs: u32,
    },
    Let {
        binding: String,
        value: u32,
        body: u32,
    },
    If {
        cond: u32,
        then_branch: u32,
        else_branch: u32,
    },
    LoopBounded {
        body: u32,
        max_iters: u32,
    },
    Tuple {
        elems: Vec<u32>,
    },
    ArrayBounded {
        elems: Vec<u32>,
        max_len: u32,
    },
}

impl CanonicalEncode for IrNode {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        match self {
            Self::ConstDyadic { num, den } => {
                out.write_u8(0)?;
                out.write_i64(*num)?;
                out.write_i64(*den)
            }
            Self::Var { name } => {
                out.write_u8(1)?;
                out.write_str(name)
            }
            Self::Add { lhs, rhs } => encode_binary_node(out, 2, *lhs, *rhs),
            Self::Sub { lhs, rhs } => encode_binary_node(out, 3, *lhs, *rhs),
            Self::Mul { lhs, rhs } => encode_binary_node(out, 4, *lhs, *rhs),
            Self::Let {
                binding,
                value,
                body,
            } => {
                out.write_u8(5)?;
                out.write_str(binding)?;
                out.write_u32(*value)?;
                out.write_u32(*body)
            }
            Self::If {
                cond,
                then_branch,
                else_branch,
            } => {
                out.write_u8(6)?;
                out.write_u32(*cond)?;
                out.write_u32(*then_branch)?;
                out.write_u32(*else_branch)
            }
            Self::LoopBounded { body, max_iters } => {
                out.write_u8(7)?;
                out.write_u32(*body)?;
                out.write_u32(*max_iters)
            }
            Self::Tuple { elems } => encode_node_list(out, 8, elems, None),
            Self::ArrayBounded { elems, max_len } => {
                encode_node_list(out, 9, elems, Some(*max_len))
            }
        }
    }
}

fn encode_binary_node(
    out: &mut CanonicalWriter,
    tag: u8,
    lhs: u32,
    rhs: u32,
) -> Result<(), CanonicalError> {
    out.write_u8(tag)?;
    out.write_u32(lhs)?;
    out.write_u32(rhs)
}

fn encode_node_list(
    out: &mut CanonicalWriter,
    tag: u8,
    elems: &[u32],
    max_len: Option<u32>,
) -> Result<(), CanonicalError> {
    out.write_u8(tag)?;
    out.write_u32(elems.len() as u32)?;
    for elem in elems {
        out.write_u32(*elem)?;
    }
    if let Some(max_len) = max_len {
        out.write_u32(max_len)?;
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceSpan {
    pub file: String,
    pub line: u32,
    pub column: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageDiagnostic {
    pub span: SourceSpan,
    pub message: String,
    pub unsupported: UnsupportedConstruct,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnsupportedConstruct {
    Allocation,
    Actor,
    Concurrency,
    ArbitraryPointer,
    UnboundedLoop,
    UnsupportedEffect,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputRange {
    pub name: String,
    pub start: i64,
    pub end: i64,
    pub precision_bits: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OverflowBehavior {
    pub on_overflow: String,
    pub on_div_zero: String,
    pub result_type: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KernelPackage {
    pub version: u32,
    pub kernel_id: String,
    pub wrela_commit: String,
    pub source_digest: Digest,
    pub semantic_ir: Vec<IrNode>,
    pub root: u32,
    pub input_ranges: Vec<InputRange>,
    pub overflow: OverflowBehavior,
    pub target_cost_table: Vec<(String, u64)>,
    pub fixture_refs: Vec<Digest>,
    pub trace_refs: Vec<Digest>,
}

impl CanonicalEncode for KernelPackage {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        out.write_u32(self.version)?;
        out.write_str(&self.kernel_id)?;
        out.write_str(&self.wrela_commit)?;
        out.write_digest(&self.source_digest)?;
        out.write_u32(self.semantic_ir.len() as u32)?;
        for node in &self.semantic_ir {
            node.encode_canonical(out)?;
        }
        out.write_u32(self.root)?;
        out.write_u32(self.input_ranges.len() as u32)?;
        for r in &self.input_ranges {
            out.write_str(&r.name)?;
            out.write_i64(r.start)?;
            out.write_i64(r.end)?;
            out.write_u32(r.precision_bits)?;
        }
        out.write_str(&self.overflow.on_overflow)?;
        out.write_str(&self.overflow.on_div_zero)?;
        out.write_str(&self.overflow.result_type)?;
        out.write_u32(self.target_cost_table.len() as u32)?;
        for (target, cost) in &self.target_cost_table {
            out.write_str(target)?;
            out.write_u64(*cost)?;
        }
        out.write_u32(self.fixture_refs.len() as u32)?;
        for digest in &self.fixture_refs {
            out.write_digest(digest)?;
        }
        out.write_u32(self.trace_refs.len() as u32)?;
        for digest in &self.trace_refs {
            out.write_digest(digest)?;
        }
        Ok(())
    }
}

impl KernelPackage {
    pub fn identity(&self) -> Result<Digest, CanonicalError> {
        content_id(b"wrela.package.v1", self)
    }

    #[cfg(test)]
    pub(crate) fn synthetic_test_fixture_quadratic() -> Self {
        // P(x) = 2x² - 3x + 1.5 encoded as semantic IR
        let ir = vec![
            IrNode::Var {
                name: "x".to_string(),
            },
            IrNode::ConstDyadic { num: 2, den: 1 },
            IrNode::Mul { lhs: 1, rhs: 0 }, // 2 * x
            IrNode::Mul { lhs: 2, rhs: 2 }, // (2*x) * x  — simplified tree
            IrNode::ConstDyadic { num: 3, den: 1 },
            IrNode::Mul { lhs: 4, rhs: 0 }, // 3 * x
            IrNode::Sub { lhs: 3, rhs: 5 },
            IrNode::ConstDyadic { num: 3, den: 2 }, // 1.5 = 3/2
            IrNode::Add { lhs: 6, rhs: 7 },
        ];
        Self {
            version: PACKAGE_VERSION,
            kernel_id: "synthetic-test-quadratic-v1".to_string(),
            wrela_commit: "synthetic-test-commit".to_string(),
            source_digest: Digest::hash_blake3(b"synthetic-test-quadratic-source"),
            semantic_ir: ir,
            root: 8,
            input_ranges: vec![InputRange {
                name: "x".to_string(),
                start: 0,
                end: 10,
                precision_bits: 64,
            }],
            overflow: OverflowBehavior {
                on_overflow: "wrap".to_string(),
                on_div_zero: "error".to_string(),
                result_type: "Result".to_string(),
            },
            target_cost_table: vec![
                ("baseline_scalar".to_string(), 120_000),
                ("bernstein".to_string(), 75_000),
            ],
            fixture_refs: vec![Digest::hash_blake3(b"synthetic-test-fixture-0")],
            trace_refs: vec![],
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ValidateOutcome {
    Valid(Digest),
    Rejected(Vec<PackageDiagnostic>),
}

pub fn validate_package(pkg: &KernelPackage) -> ValidateOutcome {
    let mut diagnostics = Vec::new();

    if pkg.version != PACKAGE_VERSION {
        diagnostics.push(PackageDiagnostic {
            span: SourceSpan {
                file: "package".to_string(),
                line: 1,
                column: 1,
            },
            message: format!("unsupported package version {}", pkg.version),
            unsupported: UnsupportedConstruct::UnsupportedEffect,
        });
    }

    if pkg.kernel_id.trim().is_empty()
        || pkg.wrela_commit.trim().is_empty()
        || pkg.source_digest == Digest::ZERO
        || pkg.semantic_ir.is_empty()
        || pkg.input_ranges.is_empty()
        || pkg.root as usize >= pkg.semantic_ir.len()
    {
        diagnostics.push(PackageDiagnostic {
            span: SourceSpan {
                file: "package".to_string(),
                line: 1,
                column: 1,
            },
            message: "incomplete package identity or root/input declaration".to_string(),
            unsupported: UnsupportedConstruct::UnsupportedEffect,
        });
    }

    for range in &pkg.input_ranges {
        if range.name.trim().is_empty()
            || range.start > range.end
            || range.precision_bits == 0
            || range.precision_bits > 128
        {
            diagnostics.push(PackageDiagnostic {
                span: SourceSpan {
                    file: "input_ranges".to_string(),
                    line: 1,
                    column: 1,
                },
                message: format!("invalid input range '{}'", range.name),
                unsupported: UnsupportedConstruct::UnsupportedEffect,
            });
        }
    }

    for (idx, node) in pkg.semantic_ir.iter().enumerate() {
        if let Some(d) = reject_unsupported_node(node, idx) {
            diagnostics.push(d);
        }
        if !matches!(node, IrNode::ConstDyadic { .. })
            && let Err(msg) = validate_node_refs(node, pkg.semantic_ir.len())
        {
            diagnostics.push(PackageDiagnostic {
                span: SourceSpan {
                    file: "semantic_ir".to_string(),
                    line: idx as u32 + 1,
                    column: 1,
                },
                message: msg,
                unsupported: UnsupportedConstruct::UnsupportedEffect,
            });
        }
    }

    if !diagnostics.is_empty() {
        return ValidateOutcome::Rejected(diagnostics);
    }

    match pkg.identity() {
        Ok(d) => ValidateOutcome::Valid(d),
        Err(e) => ValidateOutcome::Rejected(vec![PackageDiagnostic {
            span: SourceSpan {
                file: "package".to_string(),
                line: 0,
                column: 0,
            },
            message: e.to_string(),
            unsupported: UnsupportedConstruct::UnsupportedEffect,
        }]),
    }
}

fn reject_unsupported_node(node: &IrNode, idx: usize) -> Option<PackageDiagnostic> {
    let span = SourceSpan {
        file: "semantic_ir".to_string(),
        line: idx as u32 + 1,
        column: 1,
    };
    match node {
        IrNode::LoopBounded { max_iters, .. } if *max_iters > 10_000 => Some(PackageDiagnostic {
            span,
            message: format!("unbounded loop max_iters={max_iters} exceeds v1 limit"),
            unsupported: UnsupportedConstruct::UnboundedLoop,
        }),
        IrNode::ArrayBounded { max_len, .. } if *max_len > 256 => Some(PackageDiagnostic {
            span,
            message: "allocation exceeds bounded array limit".to_string(),
            unsupported: UnsupportedConstruct::Allocation,
        }),
        _ => None,
    }
}

fn validate_node_refs(node: &IrNode, len: usize) -> Result<(), String> {
    let check = |i: u32| -> Result<(), String> {
        if (i as usize) >= len {
            Err(format!("node index {i} out of range (len={len})"))
        } else {
            Ok(())
        }
    };
    match node {
        IrNode::Add { lhs, rhs } | IrNode::Sub { lhs, rhs } | IrNode::Mul { lhs, rhs } => {
            check(*lhs)?;
            check(*rhs)
        }
        IrNode::Let { value, body, .. } => {
            check(*value)?;
            check(*body)
        }
        IrNode::If {
            cond,
            then_branch,
            else_branch,
        } => {
            check(*cond)?;
            check(*then_branch)?;
            check(*else_branch)
        }
        IrNode::LoopBounded { body, .. } => check(*body),
        IrNode::Tuple { elems } | IrNode::ArrayBounded { elems, .. } => {
            for e in elems {
                check(*e)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

pub fn export_package_json(pkg: &KernelPackage) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(pkg)
}

pub fn import_package_json(json: &str) -> Result<KernelPackage, serde_json::Error> {
    serde_json::from_str(json)
}

#[allow(dead_code)]
pub fn roundtrip_package(pkg: &KernelPackage) -> Result<KernelPackage, String> {
    let json = export_package_json(pkg).map_err(|e| e.to_string())?;
    import_package_json(&json).map_err(|e| e.to_string())
}

/// Validate a large package within the performance gate (100k nodes < 500ms).
#[allow(dead_code)]
pub fn validate_large_package(node_count: usize) -> std::time::Duration {
    let start = Instant::now();
    let mut ir = Vec::with_capacity(node_count);
    for i in 0..node_count {
        ir.push(IrNode::ConstDyadic {
            num: i as i64,
            den: 1,
        });
    }
    let pkg = KernelPackage {
        version: PACKAGE_VERSION,
        kernel_id: "bench-large".to_string(),
        wrela_commit: "bench".to_string(),
        source_digest: Digest::hash_blake3(b"bench"),
        semantic_ir: ir,
        root: (node_count - 1) as u32,
        input_ranges: vec![InputRange {
            name: "x".to_string(),
            start: 0,
            end: 1,
            precision_bits: 32,
        }],
        overflow: OverflowBehavior {
            on_overflow: "wrap".to_string(),
            on_div_zero: "error".to_string(),
            result_type: "Result".to_string(),
        },
        target_cost_table: vec![],
        fixture_refs: vec![],
        trace_refs: vec![],
    };
    let _ = validate_package(&pkg);
    start.elapsed()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_reference_package_validates() {
        let pkg = KernelPackage::synthetic_test_fixture_quadratic();
        match validate_package(&pkg) {
            ValidateOutcome::Valid(d) => assert_ne!(d, Digest::ZERO),
            ValidateOutcome::Rejected(d) => panic!("unexpected reject: {d:?}"),
        }
    }

    #[test]
    fn test_reject_unbounded_loop() {
        let mut pkg = KernelPackage::synthetic_test_fixture_quadratic();
        pkg.semantic_ir.push(IrNode::LoopBounded {
            body: 0,
            max_iters: 999_999,
        });
        match validate_package(&pkg) {
            ValidateOutcome::Rejected(d) => {
                assert!(
                    d.iter()
                        .any(|x| matches!(x.unsupported, UnsupportedConstruct::UnboundedLoop))
                );
            }
            ValidateOutcome::Valid(_) => panic!("expected rejection"),
        }
    }

    #[test]
    fn test_package_roundtrip() {
        let pkg = KernelPackage::synthetic_test_fixture_quadratic();
        let id1 = pkg.identity().unwrap();
        let rt = roundtrip_package(&pkg).unwrap();
        let id2 = rt.identity().unwrap();
        assert_eq!(id1, id2);
    }

    #[test]
    fn test_semantic_change_changes_identity() {
        let mut pkg = KernelPackage::synthetic_test_fixture_quadratic();
        let id1 = pkg.identity().unwrap();
        pkg.input_ranges[0].precision_bits = 32;
        let id2 = pkg.identity().unwrap();
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_cost_and_verifier_inputs_change_identity() {
        let mut pkg = KernelPackage::synthetic_test_fixture_quadratic();
        let original = pkg.identity().unwrap();
        pkg.target_cost_table[0].1 += 1;
        assert_ne!(pkg.identity().unwrap(), original);

        let mut pkg = KernelPackage::synthetic_test_fixture_quadratic();
        let original = pkg.identity().unwrap();
        pkg.fixture_refs[0] = Digest::hash_blake3(b"different-synthetic-test-fixture");
        assert_ne!(pkg.identity().unwrap(), original);
    }

    #[test]
    fn test_large_package_under_500ms() {
        let elapsed = validate_large_package(100_000);
        assert!(
            elapsed.as_millis() < 500,
            "100k node validation took {}ms",
            elapsed.as_millis()
        );
    }
}
