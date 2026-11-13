use super::*;
use std::{
    io::{self, Cursor},
    os::{
        fd::AsRawFd,
        unix::{fs::PermissionsExt, net::UnixListener},
    },
    sync::{mpsc, Arc},
    thread,
    time::Duration,
};

fn binding() -> Binding {
    Binding {
        boot: nonce().unwrap(),
        publisher: nonce().unwrap(),
        channel: 1,
        key_id: "fixture-key-only".into(),
        library: "synthetic-library".into(),
        principal: "a".repeat(64),
        session: "fixture-session".into(),
        grant_epoch: 3,
        target: "synthetic-item".into(),
        scope: "edit".into(),
        ceiling: "private".into(),
    }
}
fn key() -> [u8; 32] {
    let mut key = [0; 32];
    getrandom::getrandom(&mut key).unwrap();
    key
}
fn live(now: u64) -> Arc<Fence> {
    let f = Arc::new(Fence::new());
    let c = f.challenge(now).unwrap();
    f.renew(|| Ok(now), &c, LEASE_MS).unwrap();
    f.bind(now, "job").unwrap();
    f
}

#[test]
fn authenticated_frames_reject_tamper_reflection_replay_and_every_identity_drift() {
    let key = key();
    let b = binding();
    let mut tx = Channel::new(key, b.clone(), Direction::Publisher);
    let original = tx
        .encode(Message::Renew {
            challenge: "single-use".into(),
            remaining_ms: 1234,
        })
        .unwrap();
    let mut rx = Channel::new(key, b.clone(), Direction::Publisher);
    assert!(matches!(
        rx.receive(&mut Cursor::new(&original)).unwrap(),
        Message::Renew {
            remaining_ms: 1234,
            ..
        }
    ));
    assert!(rx.receive(&mut Cursor::new(&original)).is_err());
    for offset in [0, 4, original.len() / 2, original.len() - 1] {
        let mut bad = original.clone();
        bad[offset] ^= 1;
        assert!(Channel::new(key, b.clone(), Direction::Publisher)
            .receive(&mut Cursor::new(bad))
            .is_err());
    }
    assert!(Channel::new(key, b.clone(), Direction::Daemon)
        .receive(&mut Cursor::new(&original))
        .is_err());
    assert!(Channel::new([0; 32], b.clone(), Direction::Publisher)
        .receive(&mut Cursor::new(&original))
        .is_err());
    let value = serde_json::to_value(&b).unwrap();
    for field in value.as_object().unwrap().keys() {
        let mut changed = value.clone();
        changed[field] = if changed[field].is_number() {
            serde_json::json!(99)
        } else {
            serde_json::json!("different")
        };
        let other: Binding = serde_json::from_value(changed).unwrap();
        assert!(
            Channel::new(key, other, Direction::Publisher)
                .receive(&mut Cursor::new(&original))
                .is_err(),
            "{field}"
        );
    }
    for length in [0u32, 65501, u32::MAX] {
        assert!(Channel::new(key, b.clone(), Direction::Publisher)
            .receive(&mut Cursor::new(length.to_be_bytes()))
            .is_err());
    }
    // Valid MAC is insufficient for an unknown or duplicate schema field.
    let binding_json = serde_json::to_string(&b).unwrap();
    for payload in [
        format!(
            r#"{{"binding":{binding_json},"sequence":1,"message":{{"op":"revoke"}},"allow":true}}"#
        )
        .into_bytes(),
        format!(
            r#"{{"binding":{binding_json},"sequence":1,"sequence":2,"message":{{"op":"revoke"}}}}"#
        )
        .into_bytes(),
    ] {
        let mut bytes = (payload.len() as u32).to_be_bytes().to_vec();
        bytes.extend(&payload);
        bytes.extend(tx.mac(&payload).finalize().into_bytes());
        assert!(Channel::new(key, b.clone(), Direction::Publisher)
            .receive(&mut Cursor::new(bytes))
            .is_err());
    }
}

#[test]
fn observed_forward_expiry_or_revocation_cannot_revive_on_wall_rollback() {
    let d = GrantDeadline::new(200, 10000, 12000).unwrap();
    assert!(d.remaining(300, 12000, true, true, true).is_err());
    assert!(d.remaining(301, 9000, true, true, true).is_err());
    let d = GrantDeadline::new(200, 10000, 12000).unwrap();
    assert!(d.remaining(300, 10100, false, true, true).is_err());
    assert!(d.remaining(301, 10101, true, true, true).is_err());
}

#[test]
fn renewal_waiting_for_fence_cannot_use_time_sampled_before_lock() {
    use std::sync::atomic::{AtomicU64, Ordering};
    let f = live(0);
    let challenge = f.challenge(1000).unwrap();
    let key = key();
    let b = binding();
    let bytes = Channel::new(key, b.clone(), Direction::Publisher)
        .encode(Message::Renew {
            challenge,
            remaining_ms: 5000,
        })
        .unwrap();
    let clock = Arc::new(AtomicU64::new(4999));
    let worker_clock = clock.clone();
    let worker_fence = f.clone();
    let guard = f.state.lock().unwrap();
    let (sample_tx, sample_rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        Channel::new(key, b, Direction::Publisher).update(
            &mut Cursor::new(bytes),
            &worker_fence,
            || {
                let now = worker_clock.load(Ordering::SeqCst);
                sample_tx.send(()).unwrap();
                Ok(now)
            },
        )
    });
    // Old implementation reports a sample here; corrected code cannot sample yet.
    let _ = sample_rx.recv_timeout(Duration::from_millis(50));
    clock.store(5500, Ordering::SeqCst);
    drop(guard);
    assert!(worker.join().unwrap().is_err());
    assert!(f.bind(5500, "job").is_err());
}

#[test]
fn lease_uses_challenge_time_single_pending_and_original_grant_deadline() {
    let f = Fence::new();
    let c = f.challenge(100).unwrap();
    assert!(f.challenge(101).is_err());
    assert!(f.renew(|| Ok(101), "wrong", 5000).is_err());
    f.renew(|| Ok(4999), &c, 5000).unwrap();
    f.bind(5099, "job").unwrap();
    assert!(f.bind(5100, "job").is_err()); // not arrival + 5000
    assert!(f.renew(|| Ok(5000), &c, 5000).is_err());
    assert!(f.challenge(5100).is_err());
    let f = Fence::new();
    let c = f.challenge(100).unwrap();
    assert!(f.renew(|| Ok(1100), &c, 1000).is_err());
    assert!(f.renew(|| Ok(1100), &c, 5001).is_err());
    let f = live(0);
    let c = f.challenge(RENEW_MS).unwrap();
    f.renew(|| Ok(1001), &c, 5000).unwrap();
    f.bind(5999, "job").unwrap();
    assert!(f.bind(6000, "job").is_err());
    f.revoke();
    assert!(f.challenge(6001).is_err());
    assert!(f.renew(|| Ok(6001), &c, 5000).is_err());
    assert!(Fence::new().bind(0, "old-job").is_err());
    let d = GrantDeadline::new(200, 10000, 12000).unwrap();
    assert_eq!(d.remaining(1200, 9000, true, true, true).unwrap(), 1000); // wall rollback
    assert!(d.remaining(2200, 9000, true, true, true).is_err());
    assert!(d.remaining(201, 12000, true, true, true).is_err()); // observed wall expiry
    for flags in [
        (false, true, true),
        (true, false, true),
        (true, true, false),
    ] {
        assert!(d.remaining(201, 10001, flags.0, flags.1, flags.2).is_err());
    }
}

#[test]
fn actual_continuous_clock_progress_and_expiry_use_native_adapter() {
    let start = continuous_ms().unwrap();
    let f = Fence::new();
    let c = f.challenge(start).unwrap();
    f.renew(|| Ok(start), &c, 20).unwrap();
    f.bind(start, "job").unwrap();
    thread::sleep(Duration::from_millis(35));
    let now = continuous_ms().unwrap();
    assert!(now >= start + 20);
    assert!(f
        .commit("job", continuous_ms, || Ok(()), || Ok(()))
        .is_err());
    // This is ordinary thread sleep, NOT a machine suspend/resume proof.
}

#[test]
fn owner_uds_mutual_mac_and_disconnect_revoke_live_fence() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = dir.path().join("fixture.sock");
    let listener = UnixListener::bind(&path).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let b = binding();
    let key = key();
    let f = Arc::new(Fence::new());
    let server_f = f.clone();
    let server_b = b.clone();
    let (ready_tx, ready_rx) = mpsc::channel();
    let (close_tx, close_rx) = mpsc::channel();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let (mut uid, mut gid) = (0, 0);
        assert_eq!(
            unsafe { libc::getpeereid(socket.as_raw_fd(), &mut uid, &mut gid) },
            0
        );
        assert_eq!(uid, unsafe { libc::geteuid() }); // only one layer; MAC still mandatory
        let t0 = continuous_ms().unwrap();
        let c = server_f.challenge(t0).unwrap();
        socket
            .write_all(
                &Channel::new(key, server_b.clone(), Direction::Daemon)
                    .encode(Message::Challenge { challenge: c })
                    .unwrap(),
            )
            .unwrap();
        let mut rx = Channel::new(key, server_b, Direction::Publisher);
        rx.update(&mut socket, &server_f, continuous_ms).unwrap();
        server_f.bind(continuous_ms().unwrap(), "job").unwrap();
        ready_tx.send(()).unwrap();
        // EOF or malformed frame ends this publisher; no reconnect renewal.
        assert!(rx.update(&mut socket, &server_f, continuous_ms).is_err());
        close_tx.send(()).unwrap();
    });
    let mut client = UnixStream::connect(path).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let challenge = match Channel::new(key, b.clone(), Direction::Daemon)
        .receive(&mut client)
        .unwrap()
    {
        Message::Challenge { challenge } => challenge,
        _ => panic!("wrong message"),
    };
    client
        .write_all(
            &Channel::new(key, b, Direction::Publisher)
                .encode(Message::Renew {
                    challenge,
                    remaining_ms: 5000,
                })
                .unwrap(),
        )
        .unwrap();
    ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    drop(client);
    close_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    server.join().unwrap();
    assert!(f
        .commit::<()>("job", continuous_ms, || Ok(()), || panic!("revoked commit"))
        .is_err());
    assert!(f.challenge(continuous_ms().unwrap()).is_err());
}

#[test]
fn real_nonblocking_handoff_is_once_and_wouldblock_rechecks_after_revocation() {
    let f = live(0);
    let (mut tx, mut rx) = UnixStream::pair().unwrap();
    tx.set_nonblocking(true).unwrap();
    rx.set_nonblocking(true).unwrap();
    assert_eq!(
        f.send("job", 1, || Ok(1), || Ok(()), &mut tx, b"request")
            .unwrap(),
        7
    );
    assert!(f
        .send("job", 1, || Ok(1), || Ok(()), &mut tx, b"duplicate")
        .is_err());
    let mut bytes = [0; 7];
    rx.read_exact(&mut bytes).unwrap();
    assert_eq!(&bytes, b"request");
    assert!(f
        .send(
            "job",
            2,
            || Ok(1),
            || anyhow::bail!("stale_settings"),
            &mut tx,
            b"denied"
        )
        .is_err());
    assert_eq!(
        rx.read(&mut bytes).unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    let filler = vec![b'x'; 65536];
    loop {
        match tx.write(&filler) {
            Ok(n) => assert!(n > 0),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
            Err(e) => panic!("{e}"),
        }
    }
    let error = f
        .send("job", 2, || Ok(1), || Ok(()), &mut tx, b"forbidden")
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<io::Error>().unwrap().kind(),
        io::ErrorKind::WouldBlock
    );
    f.revoke();
    loop {
        match rx.read(&mut [0; 65536]) {
            Ok(n) => assert!(n > 0),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
            Err(e) => panic!("{e}"),
        }
    }
    assert!(f
        .send("job", 2, || Ok(2), || Ok(()), &mut tx, b"forbidden")
        .is_err());
    assert_eq!(
        rx.read(&mut bytes).unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    let f = live(0);
    let (mut tx, mut rx) = UnixStream::pair().unwrap();
    tx.set_nonblocking(true).unwrap();
    rx.set_nonblocking(true).unwrap();
    let large = vec![b'p'; 8 * 1024 * 1024];
    let n = f
        .send("job", 1, || Ok(1), || Ok(()), &mut tx, &large)
        .unwrap();
    assert!(n > 0 && n < large.len());
    assert!(f
        .send("job", 1, || Ok(2), || Ok(()), &mut tx, &large)
        .is_err());
    let mut got = vec![0; n];
    rx.read_exact(&mut got).unwrap();
    assert_eq!(got, &large[..n]);
}

#[test]
fn actual_threads_linearize_revocation_against_syscall_and_commit() {
    for send in [false, true] {
        let f = live(0);
        let worker_f = f.clone();
        let revoker_f = f.clone();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (attempt_tx, attempt_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let (mut tx, mut rx) = UnixStream::pair().unwrap();
        tx.set_nonblocking(true).unwrap();
        let worker = thread::spawn(move || {
            let current = || {
                entered_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(2)).unwrap();
                Ok(())
            };
            if send {
                worker_f
                    .send("job", 1, || Ok(1), current, &mut tx, b"x")
                    .map(|_| ())
            } else {
                worker_f.commit("job", || Ok(1), current, || Ok(()))
            }
        });
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let revoker = thread::spawn(move || {
            attempt_tx.send(()).unwrap();
            revoker_f.revoke();
            done_tx.send(()).unwrap();
        });
        attempt_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(done_rx.try_recv().is_err());
        release_tx.send(()).unwrap();
        worker.join().unwrap().unwrap();
        done_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        revoker.join().unwrap();
        if send {
            let mut b = [0];
            rx.read_exact(&mut b).unwrap();
            assert_eq!(b, [b'x']);
        }
        assert!(f
            .commit::<()>("job", || Ok(2), || Ok(()), || panic!("after revoke"))
            .is_err());
        let f = live(0);
        f.revoke();
        assert!(f
            .commit::<()>("job", || Ok(2), || Ok(()), || panic!("revoke won"))
            .is_err());
    }
}

#[path = "../../../../examples/fixtures/reader_book.rs"]
mod book;

#[test]
fn process_exit_at_revision_gate_never_replays_an_unpublished_intent() {
    use memex_core::Sensitivity;
    use memex_store::reader::repair;
    const ROOT: &str = "MEMEX_SYNTHETIC_GATE_CRASH_ROOT";
    if let Some(root) = std::env::var_os(ROOT) {
        let root = std::path::PathBuf::from(root);
        let plan: repair::Plan =
            serde_json::from_slice(&std::fs::read(root.join("plan.json")).unwrap()).unwrap();
        let after = std::env::var("MEMEX_SYNTHETIC_GATE_AFTER").unwrap() == "1";
        let f = live(0);
        repair::apply_with_gate(
            &root.join("Library"),
            plan,
            Sensitivity::Public,
            |publish| {
                f.commit(
                    "job",
                    || Ok(1),
                    || Ok(()),
                    || {
                        if after {
                            publish()?;
                        }
                        // No destructors, no post-publication sync: actual process loss.
                        unsafe { libc::_exit(73) }
                    },
                )
            },
        )
        .unwrap();
        panic!("fixture did not reach publication gate");
    }
    for after in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let library = dir.path().join("Library");
        let doc = book::seed(&library).unwrap();
        let mut plan = repair::plan(
            &library,
            &doc.item_id,
            &doc.revision_id,
            "appendix",
            Sensitivity::Public,
        )
        .unwrap();
        let end = plan.identity.original.bytes;
        plan.original_edits.push(repair::Edit {
            range: [end - 1, end],
            replacement: "\n\n".into(),
        });
        plan.pairs
            .iter_mut()
            .rev()
            .find_map(|p| p.o.as_mut())
            .unwrap()[1] += 1;
        std::fs::write(
            dir.path().join("plan.json"),
            serde_json::to_vec(&plan).unwrap(),
        )
        .unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact","queue::reader_authority::control::tests::process_exit_at_revision_gate_never_replays_an_unpublished_intent","--nocapture"])
            .env(ROOT,dir.path()).env("MEMEX_SYNTHETIC_GATE_AFTER",if after{"1"}else{"0"})
            .stdout(std::process::Stdio::null()).status().unwrap();
        assert_eq!(status.code(), Some(73));
        let before_recovery = memex_store::read_item(&library, &doc.item_id)
            .unwrap()
            .1
            .revision_id;
        assert_eq!(before_recovery != doc.revision_id, after);
        memex_store::recover_pending_ingests(&library).unwrap();
        assert_eq!(
            memex_store::read_item(&library, &doc.item_id)
                .unwrap()
                .1
                .revision_id,
            before_recovery
        );
        assert_eq!(
            repair::committed(&library, &plan, Sensitivity::Public)
                .unwrap()
                .is_some(),
            after
        );
        if after {
            repair::apply_with_gate(&library, plan, Sensitivity::Public, |_| {
                panic!("receipt must not publish again")
            })
            .unwrap();
        }
    }
}

#[test]
fn real_store_final_gate_orders_publication_against_revocation() {
    use memex_core::Sensitivity;
    use memex_store::reader::{
        chapter_translation as tr, chapter_translation_map as map, paragraphs,
    };
    for publication_first in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let library = dir.path().join("Library");
        let doc = book::seed(&library).unwrap();
        let s = tr::source(
            &library,
            &doc.item_id,
            &doc.revision_id,
            "appendix",
            Sensitivity::Public,
        )
        .unwrap();
        let route = tr::Route {
            provider_id: "fixture".into(),
            model_id: "synthetic".into(),
            reasoning_effort: None,
        };
        let version = tr::publish(
            &library,
            s.identity,
            tr::Provenance {
                route: route.clone(),
                prompt_version: "fixture".into(),
                prompt_sha256: "a".repeat(64),
                plan_sha256: "b".repeat(64),
            },
            "# 合成附录\n\n合成正文。\n".into(),
            Sensitivity::Public,
        )
        .unwrap()
        .version;
        let s = map::source(
            &library,
            &doc.item_id,
            &doc.revision_id,
            "appendix",
            &version,
            Sensitivity::Public,
        )
        .unwrap();
        let identity = s.identity;
        let pairs: Vec<_> = paragraphs::blocks(&s.source_markdown)
            .into_iter()
            .zip(paragraphs::blocks(&s.target_markdown))
            .map(|((o, kind), (t, _))| paragraphs::Pair {
                o: Some(o),
                t: Some(t),
                kind,
                relation: paragraphs::Relation::Matched,
            })
            .collect();
        let provenance = map::Provenance {
            route,
            input_profile_sha256: "c".repeat(64),
            prompt_version: "fixture".into(),
            prompt_sha256: "d".repeat(64),
            algorithm: "fixture-map".into(),
        };
        let fence = live(0);
        let worker_fence = fence.clone();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let worker_library = library.clone();
        let worker_id = identity.clone();
        let worker_pairs = pairs.clone();
        let worker_provenance = provenance.clone();
        let writer = thread::spawn(move || {
            map::publish_with_gate(
                &worker_library,
                worker_id,
                worker_provenance,
                worker_pairs,
                "job",
                Sensitivity::Public,
                |publish| {
                    if publication_first {
                        worker_fence.commit(
                            "job",
                            || Ok(1),
                            || {
                                entered_tx.send(()).unwrap();
                                release_rx.recv_timeout(Duration::from_secs(3)).unwrap();
                                Ok(())
                            },
                            publish,
                        )
                    } else {
                        entered_tx.send(()).unwrap();
                        release_rx.recv_timeout(Duration::from_secs(3)).unwrap();
                        worker_fence.commit("job", || Ok(1), || Ok(()), publish)
                    }
                },
            )
        });
        entered_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        if publication_first {
            let revoker_fence = fence.clone();
            let (attempt_tx, attempt_rx) = mpsc::channel();
            let revoker = thread::spawn(move || {
                attempt_tx.send(()).unwrap();
                revoker_fence.revoke();
            });
            attempt_rx.recv_timeout(Duration::from_secs(3)).unwrap();
            release_tx.send(()).unwrap();
            writer.join().unwrap().unwrap();
            revoker.join().unwrap();
        } else {
            fence.revoke();
            release_tx.send(()).unwrap();
            assert!(writer.join().unwrap().is_err());
        }
        memex_store::recover_pending_ingests(&library).unwrap();
        assert_eq!(
            map::receipt(&library, &identity, "job", Sensitivity::Public)
                .unwrap()
                .is_some(),
            publication_first
        );
        assert_eq!(
            tr::metadata(
                &library,
                &doc.item_id,
                &doc.revision_id,
                "appendix",
                Sensitivity::Public
            )
            .unwrap()
            .unwrap()
            .version,
            version
        );
        if publication_first {
            map::publish_with_gate(
                &library,
                identity,
                provenance,
                pairs,
                "job",
                Sensitivity::Public,
                |_| panic!("receipt replay is not a new commit"),
            )
            .unwrap();
        }
    }
}
