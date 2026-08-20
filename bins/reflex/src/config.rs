use reflex_canonical::{CanonicalEncode, CanonicalError, CanonicalWriter};
use reflex_domain::Domain;
use reflex_domain_bitvec::{BITVEC_FEATURE_DIM, BitvecDomain, frozen_eval, frozen_train};
use reflex_scheduler::{
    ExperimentManifest, ExperimentMode, ImmutableInputs, ManifestLabels, ResourceRequirements,
    SearchConfig,
};
use reflex_types::{Digest, EvaluatorId, VerifierId};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LocalExperimentConfig {
    experiment: ExperimentSection,
    search: SearchSection,
    training: TrainingSection,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExperimentSection {
    name: String,
    domain: String,
    generations: u32,
    #[serde(default)]
    horizon_difficulty: Option<String>,
    #[serde(default)]
    mode: Option<ExperimentMode>,
    seeds: Vec<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchSection {
    algorithm: String,
    action_budget: u32,
    node_budget: u32,
    exploration_uniform: f32,
    #[serde(default = "default_cpu_seconds")]
    cpu_seconds: f64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TrainingSection {
    model_type: String,
    input_dim: usize,
    hidden_dim: usize,
    epochs: usize,
    learning_rate: f32,
    weight_decay: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LocalRunConfig {
    pub manifest: ExperimentManifest,
    pub generations: u32,
    pub hidden_dim: usize,
    pub epochs: usize,
    pub learning_rate: f32,
    pub weight_decay: f32,
}

fn default_cpu_seconds() -> f64 {
    30.0
}

pub fn load_experiment_manifest(
    path: impl AsRef<Path>,
) -> Result<ExperimentManifest, Box<dyn std::error::Error>> {
    Ok(load_local_run_config(path)?.manifest)
}

pub fn load_local_run_config(
    path: impl AsRef<Path>,
) -> Result<LocalRunConfig, Box<dyn std::error::Error>> {
    let bytes = std::fs::read(path.as_ref()).map_err(|error| {
        format!(
            "failed to read experiment config `{}`: {error}",
            path.as_ref().display()
        )
    })?;
    let text = std::str::from_utf8(&bytes).map_err(|error| {
        format!(
            "experiment config `{}` is not UTF-8: {error}",
            path.as_ref().display()
        )
    })?;
    run_config_from_toml(text)
}

#[cfg(test)]
fn manifest_from_toml(text: &str) -> Result<ExperimentManifest, Box<dyn std::error::Error>> {
    Ok(run_config_from_toml(text)?.manifest)
}

fn run_config_from_toml(text: &str) -> Result<LocalRunConfig, Box<dyn std::error::Error>> {
    let config: LocalExperimentConfig =
        toml::from_str(text).map_err(|error| format!("invalid experiment config: {error}"))?;
    validate_config(&config)?;

    let domain = BitvecDomain::new();
    let capabilities = domain.capabilities();
    let corpus = digest_tasks(&domain, frozen_train())?;
    let eval = digest_tasks(&domain, frozen_eval())?;
    let split = Digest::hash_blake3(
        [corpus.as_bytes().as_slice(), eval.as_bytes().as_slice()]
            .concat()
            .as_slice(),
    );
    let toolchain = Digest::hash_blake3(include_bytes!("../../../rust-toolchain.toml"));
    let analysis_plan = reflex_canonical::content_id(
        b"reflex.local-bitvec-analysis.v1",
        &AnalysisPlanIdentity(&config),
    )?;
    let mode = config.experiment.mode.unwrap_or(ExperimentMode::Registered);
    let seeds = config.experiment.seeds;

    let manifest = ExperimentManifest {
        schema: "reflex.experiment.v1".to_string(),
        labels: ManifestLabels {
            name: config.experiment.name,
            comment: config.experiment.horizon_difficulty,
        },
        mode,
        inputs: ImmutableInputs {
            code: Digest::hash_blake3(crate::version::VERSION_LINE.as_bytes()),
            image: None,
            domain: capabilities.domain_digest,
            toolchain,
            corpus,
            split,
            feature_schema: capabilities.feature_schema,
            action_schema: capabilities.action_schema,
            policy: Digest::hash_blake3(b"bitvec-uniform-policy-v1"),
            model_checkpoint: None,
            knowledge_edition: None,
            cache_snapshot: None,
            search: SearchConfig {
                algorithm: config.search.algorithm,
                action_budget: config.search.action_budget,
                node_budget: config.search.node_budget,
                cpu_seconds: config.search.cpu_seconds,
                exploration_uniform: config.search.exploration_uniform,
            },
            resource: ResourceRequirements {
                class: "reference-4vcpu-8gb".to_string(),
                cpu_permits: 4,
                memory_bytes: 8 * 1024 * 1024 * 1024,
                scratch_bytes: 1024 * 1024 * 1024,
            },
            verifier: VerifierId::from_digest(Digest::hash_blake3(
                b"bitvec-exhaustive-u8-verifier-v1",
            )),
            utility_evaluators: vec![EvaluatorId::from_digest(Digest::hash_blake3(
                b"bitvec-operations-saved-evaluator-v1",
            ))],
            analysis_plan,
        },
        seeds,
    };
    manifest.validate()?;
    Ok(LocalRunConfig {
        manifest,
        generations: config.experiment.generations,
        hidden_dim: config.training.hidden_dim,
        epochs: config.training.epochs,
        learning_rate: config.training.learning_rate,
        weight_decay: config.training.weight_decay,
    })
}

/// Identity-bearing local analysis/training inputs not already encoded by
/// `ExperimentManifest`. Human labels and TOML whitespace are deliberately
/// absent, so semantically identical plans have one experiment identity.
struct AnalysisPlanIdentity<'a>(&'a LocalExperimentConfig);

impl CanonicalEncode for AnalysisPlanIdentity<'_> {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        let config = self.0;
        out.write_u32(config.experiment.generations)?;
        out.write_str(&config.training.model_type)?;
        out.write_u64(u64::try_from(config.training.input_dim).map_err(|_| {
            CanonicalError::LengthOverflow {
                length: config.training.input_dim,
                limit: 64,
            }
        })?)?;
        out.write_u64(u64::try_from(config.training.hidden_dim).map_err(|_| {
            CanonicalError::LengthOverflow {
                length: config.training.hidden_dim,
                limit: 64,
            }
        })?)?;
        out.write_u64(u64::try_from(config.training.epochs).map_err(|_| {
            CanonicalError::LengthOverflow {
                length: config.training.epochs,
                limit: 64,
            }
        })?)?;
        out.write_f32(config.training.learning_rate)?;
        out.write_f32(config.training.weight_decay)
    }
}

fn validate_config(config: &LocalExperimentConfig) -> Result<(), String> {
    if config.experiment.name.trim().is_empty() {
        return Err("experiment.name must not be empty".into());
    }
    if config.experiment.name.len() > 128 {
        return Err("experiment.name must be at most 128 bytes".into());
    }
    if config
        .experiment
        .horizon_difficulty
        .as_ref()
        .is_some_and(|value| value.len() > 1_024)
    {
        return Err("experiment.horizon_difficulty must be at most 1024 bytes".into());
    }
    if config.experiment.domain != "bitvec-v1" {
        return Err(format!(
            "experiment.domain must be `bitvec-v1`, got `{}`",
            config.experiment.domain
        ));
    }
    if !(1..=10_000).contains(&config.experiment.generations) {
        return Err("experiment.generations must be in 1..=10000".into());
    }
    if config.experiment.seeds.is_empty() {
        return Err("experiment.seeds must be non-empty".into());
    }
    if config.experiment.seeds.len() > 1_024 {
        return Err("experiment.seeds must contain at most 1024 entries".into());
    }
    if config
        .experiment
        .seeds
        .iter()
        .collect::<std::collections::HashSet<_>>()
        .len()
        != config.experiment.seeds.len()
    {
        return Err("experiment.seeds must be unique".into());
    }
    if config.search.algorithm != "best-first-and-or-v1" {
        return Err("search.algorithm must be `best-first-and-or-v1`".into());
    }
    if config.search.action_budget == 0 {
        return Err("search.action_budget must be non-zero".into());
    }
    if config.search.node_budget == 0 {
        return Err("search.node_budget must be non-zero".into());
    }
    if config.search.action_budget > config.search.node_budget {
        return Err("search.action_budget must not exceed search.node_budget".into());
    }
    if !config.search.cpu_seconds.is_finite() || config.search.cpu_seconds <= 0.0 {
        return Err("search.cpu_seconds must be finite and positive".into());
    }
    if !config.search.exploration_uniform.is_finite()
        || !(0.0..=1.0).contains(&config.search.exploration_uniform)
    {
        return Err("search.exploration_uniform must be finite and in [0,1]".into());
    }
    if config.training.model_type != "micro-mlp" {
        return Err("training.model_type must be `micro-mlp`".into());
    }
    if config.training.input_dim != BITVEC_FEATURE_DIM {
        return Err(format!(
            "training.input_dim must match bitvec feature dimension {BITVEC_FEATURE_DIM}"
        ));
    }
    if !(1..=4_096).contains(&config.training.hidden_dim) {
        return Err("training.hidden_dim must be in 1..=4096".into());
    }
    if !(1..=1_000_000).contains(&config.training.epochs) {
        return Err("training.epochs must be in 1..=1000000".into());
    }
    if !config.training.learning_rate.is_finite() || config.training.learning_rate <= 0.0 {
        return Err("training.learning_rate must be finite and positive".into());
    }
    if !config.training.weight_decay.is_finite() || config.training.weight_decay < 0.0 {
        return Err("training.weight_decay must be finite and non-negative".into());
    }
    Ok(())
}

fn digest_tasks(
    domain: &BitvecDomain,
    tasks: &[reflex_domain_bitvec::BitvecTask],
) -> Result<Digest, Box<dyn std::error::Error>> {
    let mut bytes = Vec::with_capacity(tasks.len() * 32);
    for task in tasks {
        bytes.extend_from_slice(domain.task_id(task)?.digest().as_bytes());
    }
    Ok(Digest::hash_blake3(&bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = r#"
[experiment]
name = "fixture"
domain = "bitvec-v1"
generations = 2
seeds = [7, 9]

[search]
algorithm = "best-first-and-or-v1"
action_budget = 100
node_budget = 1000
exploration_uniform = 0.1

[training]
model_type = "micro-mlp"
input_dim = 24
hidden_dim = 16
epochs = 30
learning_rate = 0.05
weight_decay = 0.001
"#;

    #[test]
    fn valid_config_produces_stable_resolved_manifest() {
        let first = manifest_from_toml(VALID).unwrap();
        let second = manifest_from_toml(VALID).unwrap();
        assert_eq!(
            first.manifest_digest().unwrap(),
            second.manifest_digest().unwrap()
        );
        assert_eq!(first.seeds, vec![7, 9]);
        assert_ne!(first.inputs.corpus, Digest::ZERO);
    }

    #[test]
    fn unknown_and_invalid_fields_fail_at_the_exact_config_boundary() {
        let unknown = VALID.replace("generations = 2", "generations = 2\nmagic = true");
        assert!(manifest_from_toml(&unknown).is_err());
        let wrong_dim = VALID.replace("input_dim = 24", "input_dim = 23");
        let error = manifest_from_toml(&wrong_dim).unwrap_err();
        assert!(error.to_string().contains("training.input_dim"));
    }

    #[test]
    fn search_and_generation_mutations_fail_at_exact_fields() {
        let mutations = [
            (
                "generations = 2",
                "generations = 0",
                "experiment.generations",
            ),
            ("seeds = [7, 9]", "seeds = [7, 7]", "experiment.seeds"),
            ("seeds = [7, 9]", "seeds = []", "experiment.seeds"),
            (
                "algorithm = \"best-first-and-or-v1\"",
                "algorithm = \"best-first\"",
                "search.algorithm",
            ),
            (
                "action_budget = 100",
                "action_budget = 0",
                "search.action_budget",
            ),
            (
                "node_budget = 1000",
                "node_budget = 0",
                "search.node_budget",
            ),
            (
                "exploration_uniform = 0.1",
                "exploration_uniform = 1.1",
                "search.exploration_uniform",
            ),
        ];
        for (before, after, field) in mutations {
            let mutated = VALID.replace(before, after);
            let error = manifest_from_toml(&mutated).unwrap_err();
            assert!(error.to_string().contains(field), "{field}: {error}");
        }
        let cpu = VALID.replace(
            "exploration_uniform = 0.1",
            "exploration_uniform = 0.1\ncpu_seconds = 0.0",
        );
        let error = manifest_from_toml(&cpu).unwrap_err();
        assert!(error.to_string().contains("search.cpu_seconds"));
    }

    #[test]
    fn experiment_identity_ignores_labels_and_toml_layout_but_covers_training() {
        let base = manifest_from_toml(VALID).unwrap();
        let relabeled = VALID
            .replace("name = \"fixture\"", "name = \"human-only rename\"")
            .replace("domain = \"bitvec-v1\"", "domain   =   \"bitvec-v1\"");
        let relabeled = manifest_from_toml(&relabeled).unwrap();
        assert_eq!(
            base.experiment_id().unwrap(),
            relabeled.experiment_id().unwrap()
        );

        let changed = VALID.replace("epochs = 30", "epochs = 31");
        let changed = manifest_from_toml(&changed).unwrap();
        assert_ne!(
            base.experiment_id().unwrap(),
            changed.experiment_id().unwrap()
        );
    }
}
