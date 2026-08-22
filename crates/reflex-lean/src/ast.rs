use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LeanName {
    Anonymous,
    Str { parent: Box<Self>, value: String },
    Num { parent: Box<Self>, value: usize },
}

impl LeanName {
    #[must_use]
    pub fn from_dotted(value: &str) -> Self {
        value.split('.').fold(Self::Anonymous, |parent, component| {
            match component.parse::<usize>() {
                Ok(value) => Self::Num {
                    parent: Box::new(parent),
                    value,
                },
                Err(_) => Self::Str {
                    parent: Box::new(parent),
                    value: component.to_owned(),
                },
            }
        })
    }

    fn write_dotted(&self, output: &mut String) {
        match self {
            Self::Anonymous => {}
            Self::Str { parent, value } => {
                parent.write_dotted(output);
                if !output.is_empty() {
                    output.push('.');
                }
                output.push_str(value);
            }
            Self::Num { parent, value } => {
                parent.write_dotted(output);
                if !output.is_empty() {
                    output.push('.');
                }
                output.push_str(&value.to_string());
            }
        }
    }
}

impl fmt::Display for LeanName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut dotted = String::new();
        self.write_dotted(&mut dotted);
        formatter.write_str(&dotted)
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LeanLevel {
    Zero,
    Succ { level: Box<Self> },
    Max { left: Box<Self>, right: Box<Self> },
    Imax { left: Box<Self>, right: Box<Self> },
    Param { name: LeanName },
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LeanBinderInfo {
    Default,
    Implicit,
    StrictImplicit,
    InstImplicit,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LeanLiteral {
    NatVal { value: String },
    StrVal { value: String },
}

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum LeanExpr {
    Bvar {
        index: usize,
    },
    Sort {
        level: LeanLevel,
    },
    Const {
        name: LeanName,
        levels: Vec<LeanLevel>,
    },
    App {
        function: Box<Self>,
        argument: Box<Self>,
    },
    Lam {
        name: LeanName,
        binder_type: Box<Self>,
        body: Box<Self>,
        binder_info: LeanBinderInfo,
    },
    ForallE {
        name: LeanName,
        binder_type: Box<Self>,
        body: Box<Self>,
        binder_info: LeanBinderInfo,
    },
    LetE {
        name: LeanName,
        r#type: Box<Self>,
        value: Box<Self>,
        body: Box<Self>,
        non_dep: bool,
    },
    Lit {
        literal: LeanLiteral,
    },
    Proj {
        type_name: LeanName,
        index: usize,
        subject: Box<Self>,
    },
}

impl LeanExpr {
    #[must_use]
    pub fn constant(name: LeanName, levels: Vec<LeanLevel>) -> Self {
        Self::Const { name, levels }
    }

    #[must_use]
    pub fn node_count(&self) -> usize {
        let mut count = 0;
        self.visit(&mut |_| count += 1);
        count
    }

    #[must_use]
    pub fn depth(&self) -> usize {
        let mut child_depth = 0;
        self.for_each_child(|child| child_depth = child_depth.max(child.depth()));
        child_depth.saturating_add(1)
    }

    pub fn visit(&self, visitor: &mut impl FnMut(&Self)) {
        visitor(self);
        self.for_each_child(|child| child.visit(visitor));
    }

    #[must_use]
    pub fn used_constants(&self) -> Vec<LeanName> {
        let mut constants = std::collections::BTreeSet::new();
        self.visit(&mut |expression| {
            if let Self::Const { name, .. } = expression {
                constants.insert(name.clone());
            }
        });
        constants.into_iter().collect()
    }

    pub fn for_each_child<'a>(&'a self, mut visitor: impl FnMut(&'a Self)) {
        match self {
            Self::App { function, argument } => {
                visitor(function);
                visitor(argument);
            }
            Self::Lam {
                binder_type, body, ..
            }
            | Self::ForallE {
                binder_type, body, ..
            } => {
                visitor(binder_type);
                visitor(body);
            }
            Self::LetE {
                r#type,
                value,
                body,
                ..
            } => {
                visitor(r#type);
                visitor(value);
                visitor(body);
            }
            Self::Proj { subject, .. } => visitor(subject),
            Self::Bvar { .. } | Self::Sort { .. } | Self::Const { .. } | Self::Lit { .. } => {}
        }
    }

    #[must_use]
    pub fn expression_at(&self, target: usize) -> Option<&Self> {
        fn find<'a>(
            expression: &'a LeanExpr,
            target: usize,
            current: &mut usize,
        ) -> Option<&'a LeanExpr> {
            if *current == target {
                return Some(expression);
            }
            *current += 1;
            match expression {
                LeanExpr::App { function, argument } => {
                    find(function, target, current).or_else(|| find(argument, target, current))
                }
                LeanExpr::Lam {
                    binder_type, body, ..
                }
                | LeanExpr::ForallE {
                    binder_type, body, ..
                } => find(binder_type, target, current).or_else(|| find(body, target, current)),
                LeanExpr::LetE {
                    r#type,
                    value,
                    body,
                    ..
                } => find(r#type, target, current)
                    .or_else(|| find(value, target, current))
                    .or_else(|| find(body, target, current)),
                LeanExpr::Proj { subject, .. } => find(subject, target, current),
                LeanExpr::Bvar { .. }
                | LeanExpr::Sort { .. }
                | LeanExpr::Const { .. }
                | LeanExpr::Lit { .. } => None,
            }
        }

        find(self, target, &mut 0)
    }

    #[must_use]
    pub fn replacing(&self, target: usize, replacement: &Self) -> Option<Self> {
        fn rewrite(
            expression: &LeanExpr,
            target: usize,
            current: &mut usize,
            replacement: &LeanExpr,
        ) -> Option<LeanExpr> {
            let here = *current;
            *current += 1;
            if here == target {
                return Some(replacement.clone());
            }
            let rebuilt = match expression {
                LeanExpr::App { function, argument } => LeanExpr::App {
                    function: Box::new(rewrite(function, target, current, replacement)?),
                    argument: Box::new(rewrite(argument, target, current, replacement)?),
                },
                LeanExpr::Lam {
                    name,
                    binder_type,
                    body,
                    binder_info,
                } => LeanExpr::Lam {
                    name: name.clone(),
                    binder_type: Box::new(rewrite(binder_type, target, current, replacement)?),
                    body: Box::new(rewrite(body, target, current, replacement)?),
                    binder_info: *binder_info,
                },
                LeanExpr::ForallE {
                    name,
                    binder_type,
                    body,
                    binder_info,
                } => LeanExpr::ForallE {
                    name: name.clone(),
                    binder_type: Box::new(rewrite(binder_type, target, current, replacement)?),
                    body: Box::new(rewrite(body, target, current, replacement)?),
                    binder_info: *binder_info,
                },
                LeanExpr::LetE {
                    name,
                    r#type,
                    value,
                    body,
                    non_dep,
                } => LeanExpr::LetE {
                    name: name.clone(),
                    r#type: Box::new(rewrite(r#type, target, current, replacement)?),
                    value: Box::new(rewrite(value, target, current, replacement)?),
                    body: Box::new(rewrite(body, target, current, replacement)?),
                    non_dep: *non_dep,
                },
                LeanExpr::Proj {
                    type_name,
                    index,
                    subject,
                } => LeanExpr::Proj {
                    type_name: type_name.clone(),
                    index: *index,
                    subject: Box::new(rewrite(subject, target, current, replacement)?),
                },
                leaf => leaf.clone(),
            };
            Some(rebuilt)
        }

        rewrite(self, target, &mut 0, replacement)
    }

    #[must_use]
    pub fn exact_occurrences(&self, needle: &Self) -> usize {
        let mut occurrences = 0;
        self.visit(&mut |expression| {
            if expression == needle {
                occurrences += 1;
            }
        });
        occurrences
    }

    #[must_use]
    pub fn is_closed(&self) -> bool {
        fn closed(expression: &LeanExpr, depth: usize) -> bool {
            match expression {
                LeanExpr::Bvar { index } => *index < depth,
                LeanExpr::Lam {
                    binder_type, body, ..
                }
                | LeanExpr::ForallE {
                    binder_type, body, ..
                } => closed(binder_type, depth) && closed(body, depth.saturating_add(1)),
                LeanExpr::LetE {
                    r#type,
                    value,
                    body,
                    ..
                } => {
                    closed(r#type, depth)
                        && closed(value, depth)
                        && closed(body, depth.saturating_add(1))
                }
                _ => {
                    let mut result = true;
                    expression.for_each_child(|child| result &= closed(child, depth));
                    result
                }
            }
        }
        closed(self, 0)
    }

    #[must_use]
    pub fn factor_closed(&self, needle: &Self) -> Option<Self> {
        fn replace(expression: &LeanExpr, needle: &LeanExpr, depth: usize) -> LeanExpr {
            if expression == needle {
                return LeanExpr::Bvar { index: depth };
            }
            match expression {
                LeanExpr::App { function, argument } => LeanExpr::App {
                    function: Box::new(replace(function, needle, depth)),
                    argument: Box::new(replace(argument, needle, depth)),
                },
                LeanExpr::Lam {
                    name,
                    binder_type,
                    body,
                    binder_info,
                } => LeanExpr::Lam {
                    name: name.clone(),
                    binder_type: Box::new(replace(binder_type, needle, depth)),
                    body: Box::new(replace(body, needle, depth.saturating_add(1))),
                    binder_info: *binder_info,
                },
                LeanExpr::ForallE {
                    name,
                    binder_type,
                    body,
                    binder_info,
                } => LeanExpr::ForallE {
                    name: name.clone(),
                    binder_type: Box::new(replace(binder_type, needle, depth)),
                    body: Box::new(replace(body, needle, depth.saturating_add(1))),
                    binder_info: *binder_info,
                },
                LeanExpr::LetE {
                    name,
                    r#type,
                    value,
                    body,
                    non_dep,
                } => LeanExpr::LetE {
                    name: name.clone(),
                    r#type: Box::new(replace(r#type, needle, depth)),
                    value: Box::new(replace(value, needle, depth)),
                    body: Box::new(replace(body, needle, depth.saturating_add(1))),
                    non_dep: *non_dep,
                },
                LeanExpr::Proj {
                    type_name,
                    index,
                    subject,
                } => LeanExpr::Proj {
                    type_name: type_name.clone(),
                    index: *index,
                    subject: Box::new(replace(subject, needle, depth)),
                },
                leaf => leaf.clone(),
            }
        }
        if !needle.is_closed() || self.exact_occurrences(needle) < 2 {
            return None;
        }
        Some(replace(self, needle, 0))
    }

    #[must_use]
    pub fn beta_or_zeta_contract(&self) -> Option<Self> {
        match self {
            Self::App { function, argument } => {
                let Self::Lam { body, .. } = function.as_ref() else {
                    return None;
                };
                instantiate(body, argument)
            }
            Self::LetE { value, body, .. } => instantiate(body, value),
            _ => None,
        }
    }

    #[must_use]
    pub fn eta_contract(&self) -> Option<Self> {
        let Self::Lam { body, .. } = self else {
            return None;
        };
        let Self::App { function, argument } = body.as_ref() else {
            return None;
        };
        if argument.as_ref() != &(Self::Bvar { index: 0 }) || contains_removed_bvar(function, 0) {
            return None;
        }
        shift(function, -1, 0)
    }
}

fn contains_removed_bvar(expression: &LeanExpr, depth: usize) -> bool {
    match expression {
        LeanExpr::Bvar { index } => *index == depth,
        LeanExpr::Lam {
            binder_type, body, ..
        }
        | LeanExpr::ForallE {
            binder_type, body, ..
        } => {
            contains_removed_bvar(binder_type, depth)
                || contains_removed_bvar(body, depth.saturating_add(1))
        }
        LeanExpr::LetE {
            r#type,
            value,
            body,
            ..
        } => {
            contains_removed_bvar(r#type, depth)
                || contains_removed_bvar(value, depth)
                || contains_removed_bvar(body, depth.saturating_add(1))
        }
        _ => {
            let mut found = false;
            expression.for_each_child(|child| found |= contains_removed_bvar(child, depth));
            found
        }
    }
}

fn shift(expression: &LeanExpr, amount: isize, cutoff: usize) -> Option<LeanExpr> {
    let shifted_index = |index: usize| {
        if index < cutoff {
            Some(index)
        } else if amount >= 0 {
            index.checked_add(amount.unsigned_abs())
        } else {
            index.checked_sub(amount.unsigned_abs())
        }
    };
    Some(match expression {
        LeanExpr::Bvar { index } => LeanExpr::Bvar {
            index: shifted_index(*index)?,
        },
        LeanExpr::App { function, argument } => LeanExpr::App {
            function: Box::new(shift(function, amount, cutoff)?),
            argument: Box::new(shift(argument, amount, cutoff)?),
        },
        LeanExpr::Lam {
            name,
            binder_type,
            body,
            binder_info,
        } => LeanExpr::Lam {
            name: name.clone(),
            binder_type: Box::new(shift(binder_type, amount, cutoff)?),
            body: Box::new(shift(body, amount, cutoff.saturating_add(1))?),
            binder_info: *binder_info,
        },
        LeanExpr::ForallE {
            name,
            binder_type,
            body,
            binder_info,
        } => LeanExpr::ForallE {
            name: name.clone(),
            binder_type: Box::new(shift(binder_type, amount, cutoff)?),
            body: Box::new(shift(body, amount, cutoff.saturating_add(1))?),
            binder_info: *binder_info,
        },
        LeanExpr::LetE {
            name,
            r#type,
            value,
            body,
            non_dep,
        } => LeanExpr::LetE {
            name: name.clone(),
            r#type: Box::new(shift(r#type, amount, cutoff)?),
            value: Box::new(shift(value, amount, cutoff)?),
            body: Box::new(shift(body, amount, cutoff.saturating_add(1))?),
            non_dep: *non_dep,
        },
        LeanExpr::Proj {
            type_name,
            index,
            subject,
        } => LeanExpr::Proj {
            type_name: type_name.clone(),
            index: *index,
            subject: Box::new(shift(subject, amount, cutoff)?),
        },
        leaf => leaf.clone(),
    })
}

fn instantiate(body: &LeanExpr, argument: &LeanExpr) -> Option<LeanExpr> {
    fn visit(expression: &LeanExpr, argument: &LeanExpr, depth: usize) -> Option<LeanExpr> {
        Some(match expression {
            LeanExpr::Bvar { index } if *index == depth => {
                shift(argument, isize::try_from(depth).ok()?, 0)?
            }
            LeanExpr::Bvar { index } if *index > depth => LeanExpr::Bvar { index: index - 1 },
            LeanExpr::App {
                function,
                argument: right,
            } => LeanExpr::App {
                function: Box::new(visit(function, argument, depth)?),
                argument: Box::new(visit(right, argument, depth)?),
            },
            LeanExpr::Lam {
                name,
                binder_type,
                body,
                binder_info,
            } => LeanExpr::Lam {
                name: name.clone(),
                binder_type: Box::new(visit(binder_type, argument, depth)?),
                body: Box::new(visit(body, argument, depth.saturating_add(1))?),
                binder_info: *binder_info,
            },
            LeanExpr::ForallE {
                name,
                binder_type,
                body,
                binder_info,
            } => LeanExpr::ForallE {
                name: name.clone(),
                binder_type: Box::new(visit(binder_type, argument, depth)?),
                body: Box::new(visit(body, argument, depth.saturating_add(1))?),
                binder_info: *binder_info,
            },
            LeanExpr::LetE {
                name,
                r#type,
                value,
                body,
                non_dep,
            } => LeanExpr::LetE {
                name: name.clone(),
                r#type: Box::new(visit(r#type, argument, depth)?),
                value: Box::new(visit(value, argument, depth)?),
                body: Box::new(visit(body, argument, depth.saturating_add(1))?),
                non_dep: *non_dep,
            },
            LeanExpr::Proj {
                type_name,
                index,
                subject,
            } => LeanExpr::Proj {
                type_name: type_name.clone(),
                index: *index,
                subject: Box::new(visit(subject, argument, depth)?),
            },
            leaf => leaf.clone(),
        })
    }
    visit(body, argument, 0)
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LeanDeclarationIdentity {
    pub name: LeanName,
    pub level_params: Vec<LeanName>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LeanArtifact {
    pub environment: LeanEnvironmentIdentity,
    pub declaration: LeanDeclarationIdentity,
    pub proposition: LeanExpr,
    pub proof_term: LeanExpr,
    pub dependencies: Vec<LeanName>,
    pub allowed_axioms: Vec<LeanName>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LeanEnvironmentIdentity {
    pub mathlib_commit: String,
    pub lean_toolchain: String,
    pub lean_commit: String,
    pub artifact_format: u32,
    pub kernel_contract: u32,
    pub worker_source_sha256: String,
}
