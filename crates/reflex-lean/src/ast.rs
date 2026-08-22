use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LeanName {
    Anonymous,
    Str {
        parent: Box<Self>,
        value: String,
    },
    Num {
        parent: Box<Self>,
        value: usize,
    },
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
    Succ {
        level: Box<Self>,
    },
    Max {
        left: Box<Self>,
        right: Box<Self>,
    },
    Imax {
        left: Box<Self>,
        right: Box<Self>,
    },
    Param {
        name: LeanName,
    },
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
        match self {
            Self::Bvar { .. } | Self::Sort { .. } | Self::Const { .. } | Self::Lit { .. } => 1,
            Self::App { function, argument } => 1 + function.depth().max(argument.depth()),
            Self::Lam {
                binder_type, body, ..
            }
            | Self::ForallE {
                binder_type, body, ..
            } => 1 + binder_type.depth().max(body.depth()),
            Self::LetE {
                r#type,
                value,
                body,
                ..
            } => 1 + r#type.depth().max(value.depth()).max(body.depth()),
            Self::Proj { subject, .. } => 1 + subject.depth(),
        }
    }

    pub fn visit(&self, visitor: &mut impl FnMut(&Self)) {
        visitor(self);
        self.for_each_child(|child| child.visit(visitor));
    }

    pub fn for_each_child(&self, mut visitor: impl FnMut(&Self)) {
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
                LeanExpr::App { function, argument } => find(function, target, current)
                    .or_else(|| find(argument, target, current)),
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
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LeanArtifact {
    pub environment: LeanEnvironmentIdentity,
    pub proposition: LeanExpr,
    pub proof_term: LeanExpr,
    pub allowed_axioms: Vec<LeanName>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LeanEnvironmentIdentity {
    pub mathlib_commit: String,
    pub lean_toolchain: String,
    pub lean_commit: String,
}
