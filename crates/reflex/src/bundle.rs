use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub enum BundlePlan {
    /// Starts with no prior Domain Bundle.
    Fresh { target: PathBuf },
    /// Continues the exact retained search state from a prior Domain Bundle.
    Resume { source: PathBuf, target: PathBuf },
    /// Starts new search from a completed Bundle's verified and learned state.
    ///
    /// The prior search tail and spent Resource Envelope are not inherited.
    Fork { source: PathBuf, target: PathBuf },
}

impl BundlePlan {
    pub(crate) fn target(&self) -> &Path {
        match self {
            Self::Fresh { target } | Self::Resume { target, .. } | Self::Fork { target, .. } => {
                target
            }
        }
    }

    pub(crate) fn source(&self) -> Option<&Path> {
        match self {
            Self::Fresh { .. } => None,
            Self::Resume { source, .. } | Self::Fork { source, .. } => Some(source),
        }
    }

    pub(crate) fn forks(&self) -> bool {
        matches!(self, Self::Fork { .. })
    }
}

#[derive(Clone, Debug)]
pub struct DomainBundle {
    path: PathBuf,
}

impl DomainBundle {
    pub(crate) fn published(path: PathBuf) -> Self {
        Self { path }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}
