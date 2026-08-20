//! Deterministic Turmoil simulations for distributed fencing (P16.5).

#[cfg(turmoil)]
mod models {
    use reflex_meta::{
        ClaimRequest, FinalAttempt, MemoryMetaStore, MetaError, MetaStore, NewCell, NewExperiment,
    };
    use reflex_types::{CellId, Digest, ExperimentId, GenerationId, WorkerId};
    use std::time::Duration;
    use turmoil::net::UdpSocket;

    const COORDINATOR: &str = "coordinator:7000";
    const STALE_FINALIZE: &[u8] = b"finalize:0";
    const CURRENT_FINALIZE: &[u8] = b"finalize:1";

    fn run_finalize_order(requests: [&'static [u8]; 3], replies: [&'static [u8]; 3], seed: u64) {
        let mut builder = turmoil::Builder::new();
        builder
            .rng_seed(seed)
            .simulation_duration(Duration::from_secs(3))
            .tick_duration(Duration::from_millis(1))
            .min_message_latency(Duration::from_millis(1))
            .max_message_latency(Duration::from_millis(3))
            .enable_random_order();
        let mut sim = builder.build();

        sim.host("coordinator", || async {
            let socket = UdpSocket::bind("0.0.0.0:7000").await?;
            let meta = MemoryMetaStore::new();
            let experiment_id = ExperimentId::from_digest(Digest::hash_blake3(b"turmoil-exp"));
            let cell_id = CellId::from_digest(Digest::hash_blake3(b"turmoil-cell"));
            meta.create_experiment(NewExperiment {
                id: experiment_id,
                name: "turmoil-fencing".to_string(),
                domain: "test".to_string(),
                manifest_digest: Digest::hash_blake3(b"experiment-manifest"),
            })
            .await?;
            meta.enqueue_cells(&[NewCell {
                id: cell_id,
                experiment_id,
                generation_id: GenerationId::from_digest(Digest::hash_blake3(b"generation")),
                manifest_digest: Digest::hash_blake3(b"cell-manifest"),
                resource_class: "test".to_string(),
                priority: 0,
            }])
            .await?;
            let current_lease = meta
                .claim_cell(ClaimRequest {
                    worker_id: WorkerId::from_digest(Digest::hash_blake3(b"current-worker")),
                    resource_class: "test".to_string(),
                    lease_duration_secs: 60,
                })
                .await?
                .expect("the queued cell must be claimable");

            let mut buffer = [0_u8; 32];
            for _ in 0..3 {
                let (length, peer) = socket.recv_from(&mut buffer).await?;
                let mut attempted_lease = current_lease.clone();
                let reply = match &buffer[..length] {
                    STALE_FINALIZE => {
                        attempted_lease.fencing_token -= 1;
                        match meta
                            .finalize_attempt(
                                &attempted_lease,
                                FinalAttempt {
                                    accepted: true,
                                    completion_manifest_digest: Digest::hash_blake3(
                                        b"stale-completion",
                                    ),
                                    error_code: None,
                                },
                            )
                            .await
                        {
                            Err(MetaError::StaleFence { .. }) => b"stale-fence".as_slice(),
                            other => panic!("stale finalization did not fail closed: {other:?}"),
                        }
                    }
                    CURRENT_FINALIZE => match meta
                        .finalize_attempt(
                            &attempted_lease,
                            FinalAttempt {
                                accepted: true,
                                completion_manifest_digest: Digest::hash_blake3(
                                    b"current-completion",
                                ),
                                error_code: None,
                            },
                        )
                        .await
                    {
                        Ok(_) => b"accepted".as_slice(),
                        Err(MetaError::AlreadyFinalized(_)) => b"already-finalized".as_slice(),
                        other => {
                            panic!("current finalization returned an invalid result: {other:?}")
                        }
                    },
                    _ => b"malformed".as_slice(),
                };
                socket.send_to(reply, peer).await?;
            }
            Ok(())
        });

        sim.client("worker", async move {
            let socket = UdpSocket::bind("0.0.0.0:0").await?;
            let mut buffer = [0_u8; 32];
            for (request, expected) in requests.into_iter().zip(replies) {
                socket.send_to(request, COORDINATOR).await?;
                let (length, _) = socket.recv_from(&mut buffer).await?;
                assert_eq!(&buffer[..length], expected);
            }
            Ok(())
        });

        sim.run().unwrap();
    }

    #[test]
    fn turmoil_stale_fence_cannot_finalize_before_or_after_current_attempt() {
        run_finalize_order(
            [STALE_FINALIZE, CURRENT_FINALIZE, CURRENT_FINALIZE],
            [b"stale-fence", b"accepted", b"already-finalized"],
            0x5eed_0001,
        );
        run_finalize_order(
            [CURRENT_FINALIZE, STALE_FINALIZE, CURRENT_FINALIZE],
            [b"accepted", b"stale-fence", b"already-finalized"],
            0x5eed_0002,
        );
    }
}
