use crate::config::LocalRunConfig;
use reflex_cas::{
    ArtifactArena, ArtifactArenaLimits, EvidenceArtifact, LocalEvidenceBundleStore, RetentionClass,
};
use reflex_domain_bitvec::{
    BITVEC_FEATURE_DIM, BitvecVerificationReceipt, CollectionReport, run_evaluation_collection,
    run_first_collection,
};
use reflex_engine::{
    AttemptOutcome, CellRegistration, EngineLimits, ExperimentRegistration, GenerationPhase,
    GenerationRegistration, LocalRunState,
};
use reflex_eval::{EvaluationReport, evaluate_model_offline};
use reflex_ml_micro::{MicroCheckpointBundle, MicroMlp, MicroTrainer};
use reflex_scheduler::PromotionPolicy;
use reflex_training::{TrainingConfig, TrainingMetrics, train_micro_model};
use reflex_types::{CellId, Digest, ExperimentId, GenerationId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Instant;

const EVIDENCE_SCHEMA: &str = "reflex.local-evidence.v1";
const SNAPSHOT_SCHEMA: &str = "reflex.local-snapshot.v1";
const GIB: u64 = 1024 * 1024 * 1024;
const ARENA_BYTES: u64 = 48 * GIB;
const MAX_BUNDLE_BYTES: u64 = 16 * GIB;
const MAX_ARTIFACT_BYTES: u64 = 8 * GIB;
const MAX_BUNDLE_ARTIFACTS: usize = 65_536;

#[derive(Clone, Debug, Serialize)]
struct LocalCollectionArtifact {
    schema: &'static str,
    split: &'static str,
    seed: u64,
    report: CollectionReport,
}

#[derive(Clone, Debug, Serialize)]
struct LocalAcceptedEvidenceArtifact {
    schema: &'static str,
    plan_artifact: Digest,
    collection_artifact: Digest,
    verification_receipts: Vec<BitvecVerificationReceipt>,
    groups: Vec<reflex_dataset::DecisionGroup>,
    features: BTreeMap<reflex_types::StateId, Vec<f32>>,
    attempts: Vec<LocalAttemptReceipt>,
}

#[derive(Clone, Debug, Serialize)]
struct LocalAttemptReceipt {
    cell_id: CellId,
    lane: String,
    attempt_no: u32,
    epoch: u64,
    manifest: Digest,
    evidence_bundle: Digest,
    archive_generation: u64,
}

#[derive(Clone, Debug, Serialize)]
struct LocalDatasetArtifact {
    schema: &'static str,
    source_collection: Digest,
    groups: Vec<reflex_dataset::DecisionGroup>,
    features: BTreeMap<reflex_types::StateId, Vec<f32>>,
}

#[derive(Clone, Debug, Serialize)]
struct LocalCheckpointArtifact {
    schema: &'static str,
    source_dataset: Digest,
    checkpoint: MicroCheckpointBundle,
    training: TrainingMetrics,
}

#[derive(Clone, Debug, Serialize)]
struct LocalEvaluationArtifact {
    schema: &'static str,
    split: &'static str,
    seed: u64,
    held_out_collection: LocalCollectionArtifact,
    groups: Vec<reflex_dataset::DecisionGroup>,
    features: BTreeMap<reflex_types::StateId, Vec<f32>>,
    verification_receipts: Vec<BitvecVerificationReceipt>,
    checkpoint_artifact: Digest,
    untrained: EvaluationReport,
    trained: EvaluationReport,
    promotion_eligible: bool,
    promotion_blocker: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LocalGenerationOutcome {
    pub ordinal: u32,
    pub generation_id: GenerationId,
    pub state: String,
    pub plan_artifact: Digest,
    pub collection_artifact: Digest,
    pub accepted_evidence_artifact: Digest,
    pub dataset_artifact: Digest,
    pub checkpoint_artifact: Digest,
    pub evaluation_artifact: Digest,
    pub report_artifact: Digest,
    pub train_wall_ns: u64,
    pub evaluation_wall_ns: u64,
    pub promotion_eligible: bool,
    pub promotion_blocker: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LocalRunOutcome {
    pub schema: String,
    pub experiment_id: ExperimentId,
    pub resolved_manifest: Digest,
    pub generations: Vec<LocalGenerationOutcome>,
    pub evidence_bundle: Digest,
    pub archive_generation: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct LocalEvidenceSnapshot {
    schema: String,
    experiment_id: ExperimentId,
    resolved_manifest: Digest,
    config: LocalRunConfig,
    stage: String,
    active_ordinal: Option<u32>,
    completed: Vec<LocalGenerationOutcome>,
}

#[derive(Clone, Debug, Serialize)]
pub struct LocalExperimentStatus {
    pub schema: &'static str,
    pub experiment_id: ExperimentId,
    pub resolved_manifest: Digest,
    pub stage: String,
    pub active_ordinal: Option<u32>,
    pub completed_generations: usize,
    pub configured_generations: u32,
    pub generations: Vec<LocalGenerationOutcome>,
    pub next_cursor: Option<String>,
    pub evidence_bundle: Digest,
    pub archive_generation: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct LocalReconstructionOutcome {
    pub schema: &'static str,
    pub action: String,
    pub outcome: LocalRunOutcome,
}

#[derive(Clone, Debug, Serialize)]
pub struct LocalEvidenceBundleSummary {
    pub schema: &'static str,
    pub experiment_id: ExperimentId,
    pub bundle_digest: Digest,
    pub archive_generation: u64,
    pub total_bytes: u64,
    pub artifacts: Vec<reflex_cas::EvidenceBundleEntry>,
}

pub async fn run(
    root: &Path,
    config: LocalRunConfig,
) -> Result<LocalRunOutcome, Box<dyn std::error::Error>> {
    let experiment_id = config.manifest.experiment_id()?;
    let resolved_manifest = config.manifest.manifest_digest()?;
    let evidence = evidence_store(root, experiment_id)?;
    let (prior, prior_bundle) = match evidence.read_current(EVIDENCE_SCHEMA) {
        Ok((bundle, _)) => {
            let snapshot = snapshot_from_bundle(&bundle)?;
            validate_snapshot(&snapshot, experiment_id, resolved_manifest)?;
            validate_snapshot_roots(&snapshot, &bundle)?;
            (snapshot.completed, Some(bundle))
        }
        Err(ref error) if is_missing_current(error) => (Vec::new(), None),
        Err(error) => return Err(error.into()),
    };
    execute(root, config, prior, prior_bundle.as_ref()).await
}

pub async fn resume(
    root: &Path,
    experiment_id: ExperimentId,
) -> Result<LocalReconstructionOutcome, Box<dyn std::error::Error>> {
    let evidence = evidence_store(root, experiment_id)?;
    let (bundle, _) = evidence.read_current(EVIDENCE_SCHEMA)?;
    let snapshot = snapshot_from_bundle(&bundle)?;
    let resolved_manifest = snapshot.config.manifest.manifest_digest()?;
    validate_snapshot(&snapshot, experiment_id, resolved_manifest)?;
    validate_snapshot_roots(&snapshot, &bundle)?;
    let already_complete =
        snapshot.completed.len() == usize::try_from(snapshot.config.generations)?;
    let completed = snapshot.completed;
    let outcome = execute(root, snapshot.config, completed, Some(&bundle)).await?;
    Ok(LocalReconstructionOutcome {
        schema: "reflex.cli.experiment-reconstructed.v1",
        action: if already_complete {
            "verified the completed run from the current atomic evidence bundle".into()
        } else {
            "reconstructed the last committed barrier and safely re-executed volatile work".into()
        },
        outcome,
    })
}

async fn execute(
    root: &Path,
    config: LocalRunConfig,
    mut completed: Vec<LocalGenerationOutcome>,
    prior_bundle: Option<&reflex_cas::EvidenceBundle>,
) -> Result<LocalRunOutcome, Box<dyn std::error::Error>> {
    let resolved_manifest = config.manifest.manifest_digest()?;
    let experiment_id = config.manifest.experiment_id()?;
    if completed.len() > usize::try_from(config.generations)? {
        return Err("evidence snapshot contains more generations than the immutable plan".into());
    }

    let arena = ArtifactArena::new(arena_limits())?;
    if let Some(bundle) = prior_bundle {
        hydrate_arena(&arena, bundle).await?;
    }
    let evidence = evidence_store(root, experiment_id)?;
    let mut engine = LocalRunState::new(EngineLimits::new(
        1,
        usize::try_from(config.generations)?,
        usize::try_from(config.generations)?
            .saturating_mul(3)
            .max(1),
    )?)?;
    engine.register_experiment(ExperimentRegistration {
        id: experiment_id,
        manifest: resolved_manifest,
    })?;

    let start = u32::try_from(completed.len())?
        .checked_add(1)
        .ok_or("generation ordinal overflow")?;
    let mut last_bundle = None;
    for ordinal in start..=config.generations {
        let generation =
            run_generation(&mut engine, &arena, &evidence, &config, &completed, ordinal).await?;
        completed.push(generation);
        let mut snapshot = snapshot(&config, completed.clone(), "generation-complete", None)?;
        let (bundle, _) =
            commit_barrier(&evidence, &arena, &mut snapshot, "generation-complete", &[]).await?;
        last_bundle = Some(bundle);
    }

    let (bundle, _) = match last_bundle {
        Some(bundle) => {
            let current = evidence.read_current(EVIDENCE_SCHEMA)?;
            debug_assert_eq!(current.0.manifest.digest, bundle);
            current
        }
        None => evidence.read_current(EVIDENCE_SCHEMA)?,
    };
    Ok(LocalRunOutcome {
        schema: "reflex.cli.local-bitvec-run.v2".into(),
        experiment_id,
        resolved_manifest,
        generations: completed,
        evidence_bundle: bundle.manifest.digest,
        archive_generation: bundle.manifest.archive_generation.get(),
    })
}

async fn run_generation(
    engine: &mut LocalRunState,
    arena: &ArtifactArena,
    evidence: &LocalEvidenceBundleStore,
    config: &LocalRunConfig,
    completed: &[LocalGenerationOutcome],
    ordinal: u32,
) -> Result<LocalGenerationOutcome, Box<dyn std::error::Error>> {
    let resolved_manifest = config.manifest.manifest_digest()?;
    let experiment_id = config.manifest.experiment_id()?;
    let generation_id = tagged_generation_id(experiment_id, ordinal);
    engine.register_generation(GenerationRegistration {
        id: generation_id,
        experiment_id,
        ordinal,
    })?;

    let seed = config.manifest.seeds[usize::try_from(ordinal - 1)? % config.manifest.seeds.len()];
    let plan = serde_json::json!({
        "schema": "reflex.local.collection-plan.v2",
        "resolved_manifest": resolved_manifest,
        "ordinal": ordinal,
        "seed": seed,
        "lanes": ["stable", "uniform", "heuristic"]
    });
    let plan_digest = publish_json(arena, &plan, RetentionClass::EvidencePermanent).await?;
    let lanes = ["stable", "uniform", "heuristic"];
    for lane in lanes {
        let manifest = lane_manifest(resolved_manifest, generation_id, lane);
        engine.register_cell(CellRegistration {
            id: CellId::from_digest(tagged_digest(b"reflex.local.lane-cell.v2", manifest)),
            experiment_id,
            generation_id,
            manifest,
            priority: 0,
        })?;
    }
    let mut stage = snapshot(config, completed.to_vec(), "collecting", Some(ordinal))?;
    let (_, committed) = commit_barrier(
        evidence,
        arena,
        &mut stage,
        "collecting",
        &[(format!("generation-{ordinal}-plan.json"), plan_digest)],
    )
    .await?;
    engine.generation_mut(generation_id)?.advance(
        GenerationPhase::Bootstrap,
        GenerationPhase::Collecting,
        &committed,
    )?;

    let collection = run_first_collection(seed)?;
    let collection_payload = LocalCollectionArtifact {
        schema: "reflex.local.bitvec-collection.v2",
        split: "frozen_train",
        seed,
        report: collection.report.clone(),
    };
    let collection_artifact = publish_json(
        arena,
        &collection_payload,
        RetentionClass::EvidencePermanent,
    )
    .await?;
    let mut collection_roots = Vec::with_capacity(collection.report.lanes.len() + 1);
    collection_roots.push((
        format!("generation-{ordinal}-collection.json"),
        collection_artifact,
    ));
    for lane in &collection.report.lanes {
        let digest = publish_json(
            arena,
            &serde_json::json!({
                "schema": "reflex.local.bitvec-lane-result.v2",
                "collection": collection_artifact,
                "lane": lane,
            }),
            RetentionClass::EvidencePermanent,
        )
        .await?;
        collection_roots.push((
            format!("generation-{ordinal}-lane-{}.json", lane.lane),
            digest,
        ));
    }
    stage.stage = "verifying".into();
    let (_, committed) =
        commit_barrier(evidence, arena, &mut stage, "verifying", &collection_roots).await?;
    let mut attempts = Vec::with_capacity(lanes.len());
    while let Some(job) = engine.claim_next()? {
        let lane = lanes
            .iter()
            .find(|lane| {
                lane_manifest(resolved_manifest, generation_id, lane) == job.ticket.manifest()
            })
            .ok_or("local engine claimed an unrecognized lane manifest")?;
        engine.finalize(job.ticket, AttemptOutcome::Accepted, &committed)?;
        attempts.push(LocalAttemptReceipt {
            cell_id: job.ticket.cell(),
            lane: (*lane).into(),
            attempt_no: job.ticket.attempt_no().get(),
            epoch: job.ticket.epoch().get(),
            manifest: job.ticket.manifest(),
            evidence_bundle: committed.digest(),
            archive_generation: committed.archive_generation().get(),
        });
    }
    if attempts.len() != lanes.len() {
        return Err("not every configured collection lane reached accepted evidence".into());
    }
    engine.generation_mut(generation_id)?.advance(
        GenerationPhase::Collecting,
        GenerationPhase::Verifying,
        &committed,
    )?;

    let features: BTreeMap<_, _> = collection.features.into_iter().collect();
    let accepted_payload = LocalAcceptedEvidenceArtifact {
        schema: "reflex.local.bitvec-accepted-evidence.v2",
        plan_artifact: plan_digest,
        collection_artifact,
        verification_receipts: collection.verification_receipts,
        groups: collection.groups.clone(),
        features: features.clone(),
        attempts,
    };
    let accepted_artifact =
        publish_json(arena, &accepted_payload, RetentionClass::EvidencePermanent).await?;
    stage.stage = "compiling-dataset".into();
    let (_, committed) = commit_barrier(
        evidence,
        arena,
        &mut stage,
        "compiling-dataset",
        &[(
            format!("generation-{ordinal}-accepted-evidence.json"),
            accepted_artifact,
        )],
    )
    .await?;
    engine.generation_mut(generation_id)?.advance(
        GenerationPhase::Verifying,
        GenerationPhase::CompilingDataset,
        &committed,
    )?;

    let dataset_payload = LocalDatasetArtifact {
        schema: "reflex.local.bitvec-dataset.v2",
        source_collection: accepted_artifact,
        groups: collection.groups,
        features,
    };
    let dataset_artifact =
        publish_json(arena, &dataset_payload, RetentionClass::EvidencePermanent).await?;
    stage.stage = "training".into();
    let (_, committed) = commit_barrier(
        evidence,
        arena,
        &mut stage,
        "training",
        &[(
            format!("generation-{ordinal}-dataset.json"),
            dataset_artifact,
        )],
    )
    .await?;
    engine.generation_mut(generation_id)?.advance(
        GenerationPhase::CompilingDataset,
        GenerationPhase::Training,
        &committed,
    )?;

    let initial = MicroMlp::random(BITVEC_FEATURE_DIM, config.hidden_dim, seed);
    let untrained = initial.clone();
    let train_start = Instant::now();
    let feature_map = dataset_payload
        .features
        .iter()
        .map(|(key, value)| (*key, value.clone()))
        .collect();
    let (trained, training) = train_micro_model(
        MicroTrainer::new(initial, config.learning_rate, config.weight_decay),
        &dataset_payload.groups,
        &feature_map,
        &TrainingConfig {
            epochs: config.epochs,
            learning_rate: config.learning_rate,
            weight_decay: config.weight_decay,
            temperature: 1.0,
            seed,
        },
    )?;
    let train_wall_ns = elapsed_ns(train_start);
    let checkpoint_payload = LocalCheckpointArtifact {
        schema: "reflex.local.micro-checkpoint.v2",
        source_dataset: dataset_artifact,
        checkpoint: trained.to_manifest(),
        training,
    };
    let checkpoint_artifact =
        publish_json(arena, &checkpoint_payload, RetentionClass::Active).await?;
    stage.stage = "evaluating".into();
    let (_, committed) = commit_barrier(
        evidence,
        arena,
        &mut stage,
        "evaluating",
        &[(
            format!("generation-{ordinal}-checkpoint.json"),
            checkpoint_artifact,
        )],
    )
    .await?;
    engine.generation_mut(generation_id)?.advance(
        GenerationPhase::Training,
        GenerationPhase::Evaluating,
        &committed,
    )?;

    let evaluation_start = Instant::now();
    let held_out = run_evaluation_collection(seed ^ 0x4556_414c)?;
    let schema = config.manifest.inputs.feature_schema;
    let untrained_eval = evaluate_model_offline(
        &untrained,
        &held_out.groups,
        &held_out.features,
        BITVEC_FEATURE_DIM,
        schema,
    )?;
    let trained_eval = evaluate_model_offline(
        &trained,
        &held_out.groups,
        &held_out.features,
        BITVEC_FEATURE_DIM,
        schema,
    )?;
    let evaluation_wall_ns = elapsed_ns(evaluation_start);
    let promotion_policy = PromotionPolicy::bootstrap_tutorial();
    let (promotion_eligible, promotion_blocker) = match promotion_policy
        .evaluate_promotion(&trained_eval, &[])
    {
        Ok(false) => (false, "held-out offline thresholds were not met".into()),
        Err(error) => (false, error.to_string()),
        Ok(true) => {
            return Err("promotion policy accepted without registered benchmark evidence".into());
        }
    };
    let evaluation_payload = LocalEvaluationArtifact {
        schema: "reflex.local.bitvec-held-out-evaluation.v2",
        split: "frozen_eval",
        seed: seed ^ 0x4556_414c,
        held_out_collection: LocalCollectionArtifact {
            schema: "reflex.local.bitvec-collection.v2",
            split: "frozen_eval",
            seed: seed ^ 0x4556_414c,
            report: held_out.report,
        },
        groups: held_out.groups,
        features: held_out.features.into_iter().collect(),
        verification_receipts: held_out.verification_receipts,
        checkpoint_artifact,
        untrained: untrained_eval,
        trained: trained_eval,
        promotion_eligible,
        promotion_blocker: promotion_blocker.clone(),
    };
    let evaluation_artifact = publish_json(
        arena,
        &evaluation_payload,
        RetentionClass::EvidencePermanent,
    )
    .await?;
    stage.stage = "rejected".into();
    let (_, committed) = commit_barrier(
        evidence,
        arena,
        &mut stage,
        "rejected",
        &[(
            format!("generation-{ordinal}-evaluation.json"),
            evaluation_artifact,
        )],
    )
    .await?;
    engine.generation_mut(generation_id)?.advance(
        GenerationPhase::Evaluating,
        GenerationPhase::Rejected,
        &committed,
    )?;

    let report_artifact = publish_json(
        arena,
        &serde_json::json!({
            "schema": "reflex.local.generation-report.v2",
            "ordinal": ordinal,
            "generation_id": generation_id,
            "state": "rejected",
            "resolved_manifest": resolved_manifest,
            "evaluation_artifact": evaluation_artifact,
            "promotion_eligible": promotion_eligible,
            "promotion_blocker": &promotion_blocker,
        }),
        RetentionClass::EvidencePermanent,
    )
    .await?;
    Ok(LocalGenerationOutcome {
        ordinal,
        generation_id,
        state: "rejected".into(),
        plan_artifact: plan_digest,
        collection_artifact,
        accepted_evidence_artifact: accepted_artifact,
        dataset_artifact,
        checkpoint_artifact,
        evaluation_artifact,
        report_artifact,
        train_wall_ns,
        evaluation_wall_ns,
        promotion_eligible,
        promotion_blocker,
    })
}

pub fn status(
    root: &Path,
    experiment_id: ExperimentId,
    after: Option<&str>,
    limit: usize,
) -> Result<LocalExperimentStatus, Box<dyn std::error::Error>> {
    let evidence = evidence_store(root, experiment_id)?;
    let (bundle, _) = evidence.read_current(EVIDENCE_SCHEMA)?;
    let snapshot = snapshot_from_bundle(&bundle)?;
    validate_snapshot(
        &snapshot,
        experiment_id,
        snapshot.config.manifest.manifest_digest()?,
    )?;
    validate_snapshot_roots(&snapshot, &bundle)?;
    let after = after.map(u32::from_str).transpose()?;
    let mut generations: Vec<_> = snapshot
        .completed
        .iter()
        .filter(|generation| after.is_none_or(|ordinal| generation.ordinal > ordinal))
        .take(limit.saturating_add(1))
        .cloned()
        .collect();
    let has_more = generations.len() > limit;
    generations.truncate(limit);
    let next_cursor = has_more
        .then(|| {
            generations
                .last()
                .map(|generation| generation.ordinal.to_string())
        })
        .flatten();
    Ok(LocalExperimentStatus {
        schema: "reflex.cli.experiment-status.v2",
        experiment_id,
        resolved_manifest: snapshot.resolved_manifest,
        stage: snapshot.stage,
        active_ordinal: snapshot.active_ordinal,
        completed_generations: snapshot.completed.len(),
        configured_generations: snapshot.config.generations,
        generations,
        next_cursor,
        evidence_bundle: bundle.manifest.digest,
        archive_generation: bundle.manifest.archive_generation.get(),
    })
}

pub fn inspect_bundle(
    root: &Path,
    digest: Digest,
) -> Result<LocalEvidenceBundleSummary, Box<dyn std::error::Error>> {
    let evidence_root = root.join("evidence");
    for entry in std::fs::read_dir(&evidence_root)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let Ok(experiment_id) = ExperimentId::from_str(&entry.file_name().to_string_lossy()) else {
            continue;
        };
        let store = evidence_store(root, experiment_id)?;
        if let Ok(bundle) = store.read(digest, EVIDENCE_SCHEMA) {
            let snapshot = snapshot_from_bundle(&bundle)?;
            validate_snapshot(&snapshot, experiment_id, snapshot.resolved_manifest)?;
            validate_snapshot_roots(&snapshot, &bundle)?;
            return Ok(LocalEvidenceBundleSummary {
                schema: "reflex.cli.evidence-bundle.v1",
                experiment_id,
                bundle_digest: digest,
                archive_generation: bundle.manifest.archive_generation.get(),
                total_bytes: bundle.manifest.total_bytes,
                artifacts: bundle.manifest.artifacts,
            });
        }
    }
    Err(format!("evidence bundle {} was not found", digest.to_hex()).into())
}

fn snapshot(
    config: &LocalRunConfig,
    completed: Vec<LocalGenerationOutcome>,
    stage: &str,
    active_ordinal: Option<u32>,
) -> Result<LocalEvidenceSnapshot, Box<dyn std::error::Error>> {
    Ok(LocalEvidenceSnapshot {
        schema: SNAPSHOT_SCHEMA.into(),
        experiment_id: config.manifest.experiment_id()?,
        resolved_manifest: config.manifest.manifest_digest()?,
        config: config.clone(),
        stage: stage.into(),
        active_ordinal,
        completed,
    })
}

async fn commit_barrier(
    evidence: &LocalEvidenceBundleStore,
    arena: &ArtifactArena,
    snapshot: &mut LocalEvidenceSnapshot,
    stage: &str,
    artifacts: &[(String, Digest)],
) -> Result<(Digest, reflex_cas::CommittedEvidence), Box<dyn std::error::Error>> {
    snapshot.stage = stage.into();
    let mut roots = BTreeMap::<String, Digest>::new();
    for generation in &snapshot.completed {
        for (name, digest) in generation_roots(generation) {
            roots.insert(name, digest);
        }
    }
    for (name, digest) in artifacts {
        if roots.insert(name.clone(), *digest).is_some() {
            return Err(format!("duplicate evidence root name {name}").into());
        }
    }
    let mut bundle_artifacts = Vec::with_capacity(roots.len() + 1);
    bundle_artifacts.push(EvidenceArtifact {
        name: "snapshot.json".into(),
        bytes: Arc::from(serde_json::to_vec(snapshot)?),
    });
    for (name, digest) in roots {
        bundle_artifacts.push(EvidenceArtifact {
            name,
            bytes: arena.get_arc(digest).await?,
        });
    }
    let (bundle, committed) = evidence.commit(EVIDENCE_SCHEMA, bundle_artifacts)?;
    Ok((bundle.manifest.digest, committed))
}

async fn publish_json<T: Serialize>(
    arena: &ArtifactArena,
    value: &T,
    retention: RetentionClass,
) -> Result<Digest, Box<dyn std::error::Error>> {
    let bytes: Arc<[u8]> = serde_json::to_vec(value)?.into();
    Ok(arena.put_arc(None, bytes, retention).await?.digest)
}

fn snapshot_from_bundle(
    bundle: &reflex_cas::EvidenceBundle,
) -> Result<LocalEvidenceSnapshot, Box<dyn std::error::Error>> {
    let bytes = bundle
        .artifacts
        .get("snapshot.json")
        .ok_or("evidence bundle is missing snapshot.json")?;
    Ok(serde_json::from_slice(bytes)?)
}

fn validate_snapshot(
    snapshot: &LocalEvidenceSnapshot,
    experiment_id: ExperimentId,
    resolved_manifest: Digest,
) -> Result<(), Box<dyn std::error::Error>> {
    if snapshot.schema != SNAPSHOT_SCHEMA
        || snapshot.experiment_id != experiment_id
        || snapshot.resolved_manifest != resolved_manifest
        || snapshot.config.manifest.experiment_id()? != experiment_id
        || snapshot.config.manifest.manifest_digest()? != resolved_manifest
    {
        return Err("atomic evidence snapshot does not match the requested experiment".into());
    }
    for (index, generation) in snapshot.completed.iter().enumerate() {
        if generation.ordinal != u32::try_from(index + 1)?
            || generation.generation_id != tagged_generation_id(experiment_id, generation.ordinal)
            || generation.state != "rejected"
            || generation.promotion_eligible
        {
            return Err("atomic evidence snapshot contains an invalid generation history".into());
        }
    }
    Ok(())
}

fn validate_snapshot_roots(
    snapshot: &LocalEvidenceSnapshot,
    bundle: &reflex_cas::EvidenceBundle,
) -> Result<(), Box<dyn std::error::Error>> {
    for generation in &snapshot.completed {
        for (name, digest) in generation_roots(generation) {
            let bytes = bundle
                .artifacts
                .get(&name)
                .ok_or_else(|| format!("evidence snapshot root {name} is absent"))?;
            if Digest::hash_blake3(bytes) != digest {
                return Err(format!("evidence snapshot root {name} has the wrong digest").into());
            }
        }
        let ordinal = generation.ordinal;
        let plan = bundle_json(bundle, &format!("generation-{ordinal}-plan.json"))?;
        require_digest(&plan, "resolved_manifest", snapshot.resolved_manifest)?;
        if plan.get("ordinal").and_then(serde_json::Value::as_u64) != Some(u64::from(ordinal)) {
            return Err("collection plan ordinal does not match its generation".into());
        }
        let accepted = bundle_json(
            bundle,
            &format!("generation-{ordinal}-accepted-evidence.json"),
        )?;
        require_digest(&accepted, "plan_artifact", generation.plan_artifact)?;
        require_digest(
            &accepted,
            "collection_artifact",
            generation.collection_artifact,
        )?;
        let attempts = accepted
            .get("attempts")
            .and_then(serde_json::Value::as_array)
            .ok_or("accepted evidence is missing attempt receipts")?;
        if attempts.len() != 3
            || attempts.iter().any(|attempt| {
                attempt
                    .get("attempt_no")
                    .and_then(serde_json::Value::as_u64)
                    != Some(1)
                    || attempt.get("epoch").and_then(serde_json::Value::as_u64) != Some(1)
                    || attempt
                        .get("archive_generation")
                        .and_then(serde_json::Value::as_u64)
                        .is_none_or(|value| value == 0)
            })
        {
            return Err(
                "accepted evidence does not contain three first-epoch lane receipts".into(),
            );
        }
        let dataset = bundle_json(bundle, &format!("generation-{ordinal}-dataset.json"))?;
        require_digest(
            &dataset,
            "source_collection",
            generation.accepted_evidence_artifact,
        )?;
        let checkpoint = bundle_json(bundle, &format!("generation-{ordinal}-checkpoint.json"))?;
        require_digest(&checkpoint, "source_dataset", generation.dataset_artifact)?;
        let evaluation = bundle_json(bundle, &format!("generation-{ordinal}-evaluation.json"))?;
        require_digest(
            &evaluation,
            "checkpoint_artifact",
            generation.checkpoint_artifact,
        )?;
        if evaluation
            .get("promotion_eligible")
            .and_then(serde_json::Value::as_bool)
            != Some(false)
            || evaluation
                .get("promotion_blocker")
                .and_then(serde_json::Value::as_str)
                != Some(generation.promotion_blocker.as_str())
        {
            return Err("held-out evaluation does not support the recorded rejection".into());
        }
        let report = bundle_json(bundle, &format!("generation-{ordinal}-report.json"))?;
        require_digest(
            &report,
            "evaluation_artifact",
            generation.evaluation_artifact,
        )?;
        if report
            .get("generation_id")
            .cloned()
            .map(serde_json::from_value::<GenerationId>)
            .transpose()?
            != Some(generation.generation_id)
        {
            return Err("generation report identity does not match its snapshot".into());
        }
    }
    Ok(())
}

fn bundle_json(
    bundle: &reflex_cas::EvidenceBundle,
    name: &str,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let bytes = bundle
        .artifacts
        .get(name)
        .ok_or_else(|| format!("evidence bundle is missing {name}"))?;
    Ok(serde_json::from_slice(bytes)?)
}

fn require_digest(
    value: &serde_json::Value,
    field: &str,
    expected: Digest,
) -> Result<(), Box<dyn std::error::Error>> {
    let actual = value
        .get(field)
        .cloned()
        .map(serde_json::from_value::<Digest>)
        .transpose()?;
    if actual != Some(expected) {
        return Err(format!("evidence field {field} has the wrong digest").into());
    }
    Ok(())
}

fn generation_roots(generation: &LocalGenerationOutcome) -> [(String, Digest); 7] {
    let ordinal = generation.ordinal;
    [
        (
            format!("generation-{ordinal}-plan.json"),
            generation.plan_artifact,
        ),
        (
            format!("generation-{ordinal}-collection.json"),
            generation.collection_artifact,
        ),
        (
            format!("generation-{ordinal}-accepted-evidence.json"),
            generation.accepted_evidence_artifact,
        ),
        (
            format!("generation-{ordinal}-dataset.json"),
            generation.dataset_artifact,
        ),
        (
            format!("generation-{ordinal}-checkpoint.json"),
            generation.checkpoint_artifact,
        ),
        (
            format!("generation-{ordinal}-evaluation.json"),
            generation.evaluation_artifact,
        ),
        (
            format!("generation-{ordinal}-report.json"),
            generation.report_artifact,
        ),
    ]
}

async fn hydrate_arena(
    arena: &ArtifactArena,
    bundle: &reflex_cas::EvidenceBundle,
) -> Result<(), Box<dyn std::error::Error>> {
    for entry in &bundle.manifest.artifacts {
        if entry.name == "snapshot.json" {
            continue;
        }
        let bytes = bundle
            .artifacts
            .get(&entry.name)
            .ok_or("validated evidence bundle is missing an artifact payload")?;
        arena
            .put_arc(
                Some(entry.digest),
                Arc::clone(bytes),
                RetentionClass::EvidencePermanent,
            )
            .await?;
    }
    Ok(())
}

fn evidence_store(
    root: &Path,
    experiment_id: ExperimentId,
) -> Result<LocalEvidenceBundleStore, reflex_cas::StoreError> {
    LocalEvidenceBundleStore::new(
        evidence_path(root, experiment_id),
        MAX_BUNDLE_BYTES,
        MAX_ARTIFACT_BYTES,
        MAX_BUNDLE_ARTIFACTS,
    )
}

fn evidence_path(root: &Path, experiment_id: ExperimentId) -> PathBuf {
    root.join("evidence").join(experiment_id.to_string())
}

fn is_missing_current(error: &reflex_cas::StoreError) -> bool {
    matches!(error, reflex_cas::StoreError::Io(message) if message.contains("No such file or directory"))
}

fn arena_limits() -> ArtifactArenaLimits {
    ArtifactArenaLimits {
        total_bytes: ARENA_BYTES,
        per_object_bytes: 8 * GIB,
        evidence_permanent_bytes: 32 * GIB,
        release_bytes: 16 * GIB,
        active_bytes: 16 * GIB,
        cache_bytes: 16 * GIB,
        ephemeral_bytes: 16 * GIB,
    }
}

fn tagged_generation_id(experiment: ExperimentId, ordinal: u32) -> GenerationId {
    let mut bytes = b"reflex.local.generation.v2\0".to_vec();
    bytes.extend_from_slice(experiment.digest().as_bytes());
    bytes.extend_from_slice(&ordinal.to_le_bytes());
    GenerationId::from_digest(Digest::hash_blake3(&bytes))
}

fn lane_manifest(resolved_manifest: Digest, generation_id: GenerationId, lane: &str) -> Digest {
    let mut bytes = b"reflex.local.lane-manifest.v2\0".to_vec();
    bytes.extend_from_slice(resolved_manifest.as_bytes());
    bytes.extend_from_slice(generation_id.digest().as_bytes());
    bytes.extend_from_slice(lane.as_bytes());
    Digest::hash_blake3(&bytes)
}

fn tagged_digest(domain: &[u8], payload: Digest) -> Digest {
    let mut bytes = Vec::with_capacity(domain.len() + payload.as_bytes().len());
    bytes.extend_from_slice(domain);
    bytes.extend_from_slice(payload.as_bytes());
    Digest::hash_blake3(&bytes)
}

fn elapsed_ns(start: Instant) -> u64 {
    u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn local_run_completes_all_generations_and_reconstructs_from_current() {
        let temp = tempfile::tempdir().unwrap();
        let mut config = crate::config::load_local_run_config("../../config/bitvec-tutorial.toml")
            .or_else(|_| crate::config::load_local_run_config("config/bitvec-tutorial.toml"))
            .unwrap();
        config.generations = 2;
        let outcome = run(temp.path(), config).await.unwrap();
        assert_eq!(outcome.generations.len(), 2);
        assert!(outcome.generations.iter().all(|generation| {
            generation.state == "rejected" && !generation.promotion_eligible
        }));

        let first = status(temp.path(), outcome.experiment_id, None, 1).unwrap();
        assert_eq!(first.completed_generations, 2);
        assert_eq!(first.generations.len(), 1);
        let cursor = first.next_cursor.as_deref().unwrap();
        let next = status(temp.path(), outcome.experiment_id, Some(cursor), 1).unwrap();
        assert_eq!(next.generations[0].ordinal, 2);

        let resumed = resume(temp.path(), outcome.experiment_id).await.unwrap();
        assert_eq!(resumed.outcome.generations.len(), 2);
        assert!(resumed.action.contains("completed run"));
    }

    #[test]
    fn arena_is_bounded_below_host_memory() {
        let limits = arena_limits();
        assert_eq!(limits.total_bytes, 48 * GIB);
        limits.validate().unwrap();
    }
}
