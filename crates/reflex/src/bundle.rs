use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub enum BundlePlan {
    Fresh { target: PathBuf },
    Resume { source: PathBuf, target: PathBuf },
}

impl BundlePlan {
    pub(crate) fn target(&self) -> &Path {
        match self {
            Self::Fresh { target } | Self::Resume { target, .. } => target,
        }
    }

    pub(crate) fn source(&self) -> Option<&Path> {
        match self {
            Self::Fresh { .. } => None,
            Self::Resume { source, .. } => Some(source),
        }
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
