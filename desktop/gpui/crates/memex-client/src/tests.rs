use super::*;
use crate::fixture::{FixtureBackend, FixtureControls};
use std::{
    io::{Read, Write},
    os::unix::{fs::PermissionsExt, net::UnixListener},
    path::Path,
    thread,
};

/// One-shot agent stand-in: reads one frame, answers with `reply` (or never answers).
fn serve_once(path: &Path, mode: u32, reply: Option<&'static str>) -> thread::JoinHandle<Vec<u8>> {
    let listener = UnixListener::bind(path).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else { return vec![] };
        let mut header = [0u8; 4];
        stream.read_exact(&mut header).unwrap();
        let mut body = vec![0u8; u32::from_be_bytes(header) as usize];
        stream.read_exact(&mut body).unwrap();
        match reply {
            Some(r) => stream.write_all(&Framing::encode(r.as_bytes())).unwrap(),
            None => thread::sleep(Duration::from_millis(600)),
        }
        body
    })
}

fn agent(dir: &tempfile::TempDir) -> (AgentBackend, std::path::PathBuf) {
    let path = dir.path().join("agent.sock");
    (AgentBackend::new(path.clone()).with_timeout(Duration::from_millis(300)), path)
}

#[test]
fn sends_exact_command_shape_and_decodes_page() {
    let dir = tempfile::tempdir().unwrap();
    let (backend, path) = agent(&dir);
    let server = serve_once(&path, 0o600, Some(r#"{"ok":true,"job":null,"jobs":null,"next_cursor":"item_b","reason_code":null,"library_items":[{"item_id":"item_a","revision_id":"rev_1","title":"中文 🌹","item_type":"论文","captured_at":null,"site_name":null}]}"#));
    let page = Client::new(backend).library_page(None).unwrap();
    let sent: serde_json::Value = serde_json::from_slice(&server.join().unwrap()).unwrap();
    assert_eq!(sent, serde_json::json!({"command":"library_list","cursor":null}));
    assert_eq!(page.next_cursor.as_deref(), Some("item_b"));
    assert_eq!(page.items[0].title, "中文 🌹");
    assert_eq!(page.items[0].source(), "其他");
}

#[test]
fn refuses_group_readable_socket() {
    let dir = tempfile::tempdir().unwrap();
    let (backend, path) = agent(&dir);
    let _server = serve_once(&path, 0o660, Some(r#"{"ok":true}"#));
    assert_eq!(Client::new(backend).jobs(), Err(ClientError::UnsafeSocket));
}

#[test]
fn index_loading_is_retried_but_write_loading_is_not() {
    let dir = tempfile::tempdir().unwrap();
    let (backend, path) = agent(&dir);
    let listener = UnixListener::bind(&path).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let server = thread::spawn(move || {
        for reply in [
            r#"{"ok":false,"reason_code":"library_loading"}"#,
            r#"{"ok":true,"library_query":{"schema_version":1,"view":"v1","generation":"g1","offset":0,"total":0,"next_offset":null,"items":[],"sources":[],"item_types":[]}}"#,
        ] {
            let (mut stream, _) = listener.accept().unwrap();
            let mut header = [0; 4];
            stream.read_exact(&mut header).unwrap();
            let mut body = vec![0; u32::from_be_bytes(header) as usize];
            stream.read_exact(&mut body).unwrap();
            stream.write_all(&Framing::encode(reply.as_bytes())).unwrap();
        }
    });
    let result = Client::new(backend).library_query(&QuerySpec::default(), Sensitivity::Public, 0, 10, None).unwrap();
    assert_eq!(result.view, "v1");
    assert_eq!(result.total, 0);
    server.join().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (backend, path) = agent(&dir);
    let server = serve_once(&path, 0o600, Some(r#"{"ok":false,"reason_code":"library_loading"}"#));
    assert_eq!(Client::new(backend).update_metadata("item_a", "rev_1", "edited", &[]), Err(ClientError::Daemon("library_loading".into())));
    server.join().unwrap();
}

#[test]
fn missing_socket_is_unavailable_and_silence_times_out() {
    let dir = tempfile::tempdir().unwrap();
    let (backend, path) = agent(&dir);
    assert_eq!(Client::new(AgentBackend::new(path.clone())).jobs(), Err(ClientError::Unavailable));
    let _server = serve_once(&path, 0o600, None);
    assert_eq!(Client::new(backend).jobs(), Err(ClientError::Timeout));
}

#[test]
fn old_agent_rejecting_context_read_is_unsupported_not_daemon_error() {
    let dir = tempfile::tempdir().unwrap();
    let (backend, path) = agent(&dir);
    let _server = serve_once(&path, 0o600, Some(r#"{"ok":false,"reason_code":"invalid_request"}"#));
    let request = ContextRequest { item_id: "item_a".into(), expected_revision_id: "rev_1".into(), start_byte: 0, end_byte: 4, max_sensitivity: Sensitivity::Private };
    assert_eq!(Client::new(backend).context_read(request), Err(ClientError::Unsupported));
}

#[test]
fn old_agent_internal_error_for_library_query_is_unsupported_but_not_for_old_reads() {
    // Observed: the archived 916 agent answers unknown commands with internal_error.
    let dir = tempfile::tempdir().unwrap();
    let (backend, path) = agent(&dir);
    let _server = serve_once(&path, 0o600, Some(r#"{"ok":false,"reason_code":"internal_error"}"#));
    let client = Client::new(backend);
    assert_eq!(client.library_query(&QuerySpec::default(), Sensitivity::Public, 0, 10, None), Err(ClientError::Unsupported));
    let dir2 = tempfile::tempdir().unwrap();
    let (backend2, path2) = agent(&dir2);
    let _server2 = serve_once(&path2, 0o600, Some(r#"{"ok":false,"reason_code":"internal_error"}"#));
    assert_eq!(Client::new(backend2).item_read("item_a"), Err(ClientError::Daemon("internal_error".into())));
}

#[test]
fn invalid_request_for_an_old_command_stays_a_daemon_error() {
    let dir = tempfile::tempdir().unwrap();
    let (backend, path) = agent(&dir);
    let _server = serve_once(&path, 0o600, Some(r#"{"ok":false,"reason_code":"invalid_request"}"#));
    assert_eq!(Client::new(backend).item_read("item_a"), Err(ClientError::Daemon("invalid_request".into())));
}

#[test]
fn context_slice_for_another_revision_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let (backend, path) = agent(&dir);
    let _server = serve_once(&path, 0o600, Some(r#"{"ok":true,"context":{"schema_version":1,"input":{"item_id":"item_a","revision_id":"rev_2","revision_sha256":"x","evidence_ids":[]},"origin":"machine_projection","coordinate_space":"revision_markdown_utf8_v1","content_sha256":"x","total_bytes":9,"start_byte":0,"end_byte":4,"content_markdown":"abcd","source_url":"https://example.invalid","canonical_url":null,"sensitivity":"private"}}"#));
    let request = ContextRequest { item_id: "item_a".into(), expected_revision_id: "rev_1".into(), start_byte: 0, end_byte: 4, max_sensitivity: Sensitivity::Private };
    assert_eq!(Client::new(backend).context_read(request), Err(ClientError::InvalidFrame));
}

#[test]
fn fixture_rejects_stale_metadata_and_mid_codepoint_ranges() {
    let fixture = FixtureBackend::new(30);
    let client = Client::new(fixture.clone());
    let detail = client.item_detail("item_fixture_000004").unwrap();
    fixture.simulate_external_edit("item_fixture_000004");
    let stale = client.update_metadata("item_fixture_000004", &detail.canonical.revision_id, "新标题", &[]);
    assert!(stale.unwrap_err().is_stale_revision());

    let md = client.item_read("item_fixture_000004").unwrap();
    let emoji = md.markdown.find('🌹').unwrap();
    let mid = ContextRequest { item_id: md.item_id.clone(), expected_revision_id: md.revision_id.clone(), start_byte: emoji + 1, end_byte: emoji + 8, max_sensitivity: Sensitivity::Private };
    assert_eq!(client.context_read(mid), Err(ClientError::Daemon("invalid_context_range".into())));
    let whole = ContextRequest { item_id: md.item_id.clone(), expected_revision_id: md.revision_id.clone(), start_byte: emoji, end_byte: emoji + '🌹'.len_utf8(), max_sensitivity: Sensitivity::Private };
    assert_eq!(client.context_read(whole).unwrap().content_markdown, "🌹");

    fixture.set_controls(FixtureControls { legacy_agent: true, ..Default::default() });
    let legacy = ContextRequest { item_id: md.item_id, expected_revision_id: md.revision_id, start_byte: 0, end_byte: 4, max_sensitivity: Sensitivity::Private };
    assert_eq!(client.context_read(legacy), Err(ClientError::Unsupported));
}

#[test]
fn generation_drops_late_tickets() {
    let generation = Generation::default();
    let first = generation.begin();
    let second = generation.begin();
    assert!(!generation.is_current(first));
    assert!(generation.is_current(second));
    generation.invalidate();
    assert!(!generation.is_current(second));
}

#[test]
fn fixture_query_windows_facets_and_stale_views() {
    let fixture = FixtureBackend::new(120);
    let client = Client::new(fixture.clone());
    let spec = QuerySpec { text: "城市".into(), ..Default::default() };
    let first = client.library_query(&spec, Sensitivity::Restricted, 0, 7, None).unwrap();
    assert!(first.total > 7);
    assert_eq!(first.items.len(), 7);
    // Facets count the whole filtered result, not the page.
    assert_eq!(first.item_types.iter().map(|f| f.count).sum::<usize>(), first.total);
    // Random offset inside the same view.
    let later = client.library_query(&spec, Sensitivity::Restricted, first.total - 1, 7, Some(first.view.clone())).unwrap();
    assert_eq!(later.items.len(), 1);
    assert_eq!(later.next_offset, None);
    // A change elsewhere invalidates the view.
    fixture.simulate_external_edit(&first.items[0].item_id);
    let stale = client.library_query(&spec, Sensitivity::Restricted, 7, 7, Some(first.view)).unwrap_err();
    assert!(stale.is_stale_view());
    // An offset without a view is refused.
    assert_eq!(client.library_query(&spec, Sensitivity::Restricted, 7, 7, None), Err(ClientError::Daemon("invalid_query_window".into())));
}

#[test]
fn model_settings_effort_is_optional_verbatim_and_unknown_shapes_fail_closed() {
    let read = |selections: &str| serde_json::from_str::<ModelConfiguration>(&format!(r#"{{"providers":[],"models":[],"selections":{selections}}}"#));
    // An older agent omits effort: it reads as unspecified and stays omitted on save.
    let old = read(r#"[{"purpose":"chat","model":"a"}]"#).unwrap();
    assert_eq!(old.selections[0].reasoning_effort, None);
    assert_eq!(serde_json::to_string(&old.selections[0]).unwrap(), r#"{"purpose":"chat","model":"a"}"#);
    // A value this version cannot label is kept verbatim, so a save does not drop it.
    let newer = read(r#"[{"purpose":"classification","model":"a","reasoning_effort":"ultra"}]"#).unwrap();
    assert_eq!(newer.selections[0].purpose, Purpose::Classification);
    assert!(serde_json::to_string(&newer).unwrap().contains(r#""reasoning_effort":"ultra""#));
    // Unknown fields or purposes would be lost on the next whole-configuration save: refuse to read.
    assert!(read(r#"[{"purpose":"chat","model":"a","temperature":0}]"#).is_err());
    assert!(read(r#"[{"purpose":"planning","model":"a"}]"#).is_err());
}

#[test]
fn fixture_subtree_counts_and_filters_are_a_deduplicated_union() {
    let fixture = FixtureBackend::new(128);
    let client = Client::new(fixture);
    let groups = client.groups().unwrap();
    let node = |id: &str| groups.iter().find(|g| g.id == id).unwrap().clone();
    let (bias, anchor, confirm) = (node("grp_c_bias"), node("grp_c_anchor"), node("grp_c_confirm"));
    // Asymmetric: items 1, 17, 33… sit in both 认知偏差 and its child 锚定效应.
    let query = |group_id: &str, include_descendants: bool| {
        let spec = QuerySpec { group: Some(GroupScope { group_id: group_id.into(), include_descendants }), ..QuerySpec::default() };
        client.library_query(&spec, Sensitivity::Restricted, 0, MAX_QUERY_LIMIT, None).unwrap()
    };
    let direct = query("grp_c_bias", false);
    let deep = query("grp_c_bias", true);
    let anchor_items = query("grp_c_anchor", false);
    let ids = |w: &QueryWindow| w.items.iter().map(|i| i.item_id.clone()).collect::<Vec<_>>();
    let shared = "item_fixture_000001".to_string();
    assert!(ids(&direct).contains(&shared) && ids(&anchor_items).contains(&shared));
    assert_eq!(ids(&deep).iter().filter(|i| **i == shared).count(), 1, "an item in parent and child is listed once");
    // Counts: direct members only vs the distinct union; never the sum of children.
    assert_eq!(direct.total, bias.item_count);
    assert_eq!(deep.total, bias.subtree_item_count);
    assert!(bias.subtree_item_count < bias.item_count + anchor.item_count + confirm.item_count);
    let mut union: Vec<String> = ids(&direct).into_iter().chain(ids(&anchor_items)).chain(ids(&query("grp_c_confirm", false))).collect();
    union.sort();
    union.dedup();
    assert_eq!(union.len(), bias.subtree_item_count);
    let mut deep_ids = ids(&deep);
    deep_ids.sort();
    assert_eq!(deep_ids, union);
    // Leaves: both views agree. Roots carry their own id; imported folders are not read-only.
    assert_eq!(anchor.item_count, anchor.subtree_item_count);
    let root = node("grp_z_psych");
    assert_eq!((root.parent_id.as_deref(), root.root_group_id.as_str()), (None, "grp_z_psych"));
    assert_eq!(bias.root_group_id, "grp_z_psych");
    assert!(groups.iter().all(|g| !g.read_only));
    // Navigation counts ignore the text query; the filtered query applies it.
    let spec = QuerySpec { text: "城市".into(), group: Some(GroupScope { group_id: "grp_c_bias".into(), include_descendants: true }), ..QuerySpec::default() };
    assert!(client.library_query(&spec, Sensitivity::Restricted, 0, 50, None).unwrap().total < deep.total);
}

#[test]
fn group_scope_is_sent_explicitly_only_when_a_folder_is_selected() {
    let plain = serde_json::to_value(Command::LibraryQuery { query: QuerySpec::default(), max_sensitivity: Sensitivity::Public, offset: 0, limit: 1, expected_view: None }).unwrap();
    assert!(plain["query"].get("group_id").is_none() && plain["query"].get("include_descendants").is_none());
    let spec = QuerySpec { group: Some(GroupScope { group_id: "g".into(), include_descendants: false }), ..QuerySpec::default() };
    let scoped = serde_json::to_value(Command::LibraryQuery { query: spec, max_sensitivity: Sensitivity::Public, offset: 0, limit: 1, expected_view: None }).unwrap();
    assert_eq!(scoped["query"]["group_id"], "g");
    assert_eq!(scoped["query"]["include_descendants"], false);
}

#[test]
fn published_date_is_displayed_at_its_precision() {
    let d = |value: Option<&str>, precision, raw: Option<&str>| PartialDate { value: value.map(Into::into), precision, raw: raw.map(Into::into) };
    let year = d(Some("1974"), DatePrecision::Year, Some("1974"));
    assert_eq!((year.display(), year.year()), ("1974".into(), Some("1974".into())));
    // A month is never expanded to a day, nor a year to January 1.
    assert_eq!(d(Some("2024-02"), DatePrecision::Month, Some("2024年2月")).display(), "2024-02");
    assert_eq!(d(Some("2024-02-29"), DatePrecision::Day, None).display(), "2024-02-29");
    let unparsed = d(None, DatePrecision::Unparsed, Some("2019年春季"));
    assert_eq!((unparsed.display(), unparsed.year()), ("2019年春季".into(), None));
}

#[test]
fn legacy_title_tag_writer_keeps_its_exact_shape() {
    let sent = serde_json::to_value(Command::UpdateItemMetadata { item_id: "i".into(), expected_revision_id: "r".into(), title: "t".into(), manual_tags: vec![] }).unwrap();
    assert_eq!(sent, serde_json::json!({"command":"update_item_metadata","item_id":"i","expected_revision_id":"r","title":"t","manual_tags":[]}));
}

#[test]
fn bibliographic_requests_have_the_agents_exact_shapes() {
    use crate::biblio::{IdentifierQuery, MetadataEdit};
    let json = |c: Command| serde_json::to_value(c).unwrap();
    assert_eq!(json(Command::Capabilities), serde_json::json!({"command":"capabilities"}));
    assert_eq!(
        json(Command::BibliographicMetadataRead { item_id: "i".into(), max_sensitivity: Sensitivity::Restricted }),
        serde_json::json!({"command":"bibliographic_metadata_read","item_id":"i","max_sensitivity":"restricted"})
    );
    let mut set = serde_json::Map::new();
    // Identifiers go out as owner input only: never raw_value or server-computed views.
    let mut id = Identifier::input("isbn", "978-0-306-40615-7", Some("print".into()));
    id.raw_value = Some("x".into());
    id.normalized_value = Some("9780306406157".into());
    set.insert("identifiers".into(), serde_json::to_value(vec![id]).unwrap());
    set.insert("published_date".into(), serde_json::to_value(PartialDate { value: Some("2024".into()), precision: DatePrecision::Year, raw: None }).unwrap());
    let edit = MetadataEdit { metadata_schema_version: 2, item_id: "i".into(), expected_revision_id: "r".into(), request_id: "q".into(), set, clear: vec!["pages".into()], preview_id: None, selected_candidate_ids: vec![] };
    assert_eq!(
        json(Command::BibliographicMetadataEdit { max_sensitivity: Sensitivity::Restricted, edit }),
        serde_json::json!({"command":"bibliographic_metadata_edit","max_sensitivity":"restricted","edit":{
            "metadata_schema_version":2,"item_id":"i","expected_revision_id":"r","request_id":"q",
            "set":{"identifiers":[{"kind":"isbn","value":"978-0-306-40615-7","qualifier":"print"}],
                   "published_date":{"value":"2024","precision":"year","raw":null}},
            "clear":["pages"],"preview_id":null,"selected_candidate_ids":[]}})
    );
    assert_eq!(
        json(Command::BibliographicDuplicateCheck { identifiers: vec![IdentifierQuery { kind: "doi".into(), value: "10.1/x".into() }], exclude_item_id: Some("i".into()), max_sensitivity: Sensitivity::Restricted }),
        serde_json::json!({"command":"bibliographic_duplicate_check","identifiers":[{"kind":"doi","value":"10.1/x"}],"exclude_item_id":"i","max_sensitivity":"restricted"})
    );
    assert_eq!(
        json(Command::BibliographicLookupPreview { item_id: "i".into(), expected_revision_id: "r".into(), identifier: IdentifierQuery { kind: "doi".into(), value: "10.1/x".into() }, max_sensitivity: Sensitivity::Restricted }),
        serde_json::json!({"command":"bibliographic_lookup_preview","item_id":"i","expected_revision_id":"r","identifier":{"kind":"doi","value":"10.1/x"},"max_sensitivity":"restricted"})
    );
}

#[test]
fn agent_shaped_responses_decode() {
    // Shapes as the agent's serde types emit them (provenance envelopes, extra fields).
    let caps: Response = serde_json::from_str(r#"{"ok":true,"job":null,"jobs":null,"next_cursor":null,"reason_code":null,"capabilities":{"schema_version":1,"features":["bibliographic_metadata_v2"],"bibliographic_metadata_v2":{"metadata_schema_version":2,"normalizer_version":"memex.identifier_normalizer.v1","lookup_adapter_version":"memex.bibliographic_lookup.v1","lookup_sources":[{"provider":"crossref","identifier_kind":"doi","route":"https://api.crossref.org/works/{doi}"}],"lookup_preview_ttl_seconds":86400,"lookup_batch":false}}}"#).unwrap();
    let c = Capabilities::from_agent(caps.capabilities.as_ref().unwrap());
    assert!(c.bibliographic && c.lookup_source("doi").is_some() && c.lookup_source("isbn").is_none());
    let read: Response = serde_json::from_str(r#"{"ok":true,"bibliographic_metadata":{"metadata_schema_version":2,"item":{"item_id":"i","revision_id":"r","revision_number":3,"item_type":"journal_article","title":"T","source_url":"https://x.invalid","canonical_url":null,"sensitivity":"private","captured_at":"2026-01-02T03:04:05Z","metadata":{"creators":{"value":[{"display_name":"Ada Example","role":"author","creator_id":"c1","name_type":"person","family_name":"Example","given_name":"Ada","institution_ids":["n1"]}],"provenance":{"origin":"owner","provider_id":"owner","provider_version":"1"}},"published_date":{"value":"2024-02","provenance":{"origin":"owner","provider_id":"owner","provider_version":"1"}},"number_of_pages":{"value":12,"provenance":{"origin":"owner","provider_id":"o","provider_version":"1"}}}},"identifiers":[{"kind":"doi","value":"10.1000/ipc","raw_value":"https://doi.org/10.1000/IPC","qualifier":null,"normalized_value":"10.1000/ipc","comparison_key":"10.1000/ipc","validation":{"status":"valid","reason":null,"normalizer_version":"v1"}}],"published_date":{"value":"2024-02","precision":"month","raw":"2024年2月"},"published_date_origin":"legacy_string"}}"#).unwrap();
    let b: Bibliographic = read.bibliographic_metadata.unwrap().into();
    assert_eq!((b.creators[0].institution_ids.clone(), b.number_of_pages), (vec!["n1".to_string()], Some(12)));
    assert!(b.identifiers[0].is_valid());
    assert_eq!(b.published_date.unwrap().display(), "2024-02");
    // The current agent does not send added_at; captured_at is kept as provenance only.
    assert_eq!((b.added_at, b.captured_at.as_deref()), (None, Some("2026-01-02T03:04:05Z")));
    assert_eq!(crate::biblio::added_at_local(None), None);
}

#[test]
fn without_the_capability_no_bibliographic_command_is_sent() {
    // An older agent rejects `capabilities`; nothing else is ever sent.
    let dir = tempfile::tempdir().unwrap();
    let (backend, path) = agent(&dir);
    let server = serve_once(&path, 0o600, Some(r#"{"ok":false,"reason_code":"invalid_request"}"#));
    let client = Client::new(backend);
    assert_eq!(client.capabilities(), Ok(Capabilities::default()));
    let sent: serde_json::Value = serde_json::from_slice(&server.join().unwrap()).unwrap();
    assert_eq!(sent, serde_json::json!({"command":"capabilities"}));
    // The listener is gone: any request would now be Unavailable, not Unsupported.
    assert_eq!(client.bibliographic("i", "r", Sensitivity::Restricted), Err(ClientError::Unsupported));
    assert_eq!(client.lookup_preview("i", "r", "doi", "10.1/x", Sensitivity::Restricted), Err(ClientError::Unsupported));
    assert_eq!(client.identifier_duplicates(vec![], None, Sensitivity::Restricted), Err(ClientError::Unsupported));

    // Same with a counting fixture in legacy mode: one probe, then silence.
    use std::sync::{atomic::{AtomicUsize, Ordering}, Arc};
    struct Counting(FixtureBackend, Arc<AtomicUsize>);
    impl Backend for Counting {
        fn send(&self, c: &Command) -> Result<Response, ClientError> {
            self.1.fetch_add(1, Ordering::SeqCst);
            self.0.send(c)
        }
        fn mode(&self) -> BackendMode {
            self.0.mode()
        }
    }
    let fixture = FixtureBackend::new(4);
    fixture.set_controls(FixtureControls { legacy_agent: true, ..Default::default() });
    let calls = Arc::new(AtomicUsize::new(0));
    let client = Client::new(Counting(fixture, calls.clone()));
    let edit = crate::biblio::MetadataEdit { metadata_schema_version: 2, item_id: "i".into(), expected_revision_id: "r".into(), request_id: "q".into(), set: Default::default(), clear: vec![], preview_id: None, selected_candidate_ids: vec![] };
    assert_eq!(client.bibliographic_edit(edit, Sensitivity::Restricted), Err(ClientError::Unsupported));
    assert_eq!(client.bibliographic("item_fixture_000001", "r", Sensitivity::Restricted), Err(ClientError::Unsupported));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    // A transport failure is not remembered as "no capability".
    let offline = FixtureBackend::new(1);
    offline.set_controls(FixtureControls { offline: true, ..Default::default() });
    let c = Client::new(offline.clone());
    assert_eq!(c.capabilities(), Err(ClientError::Unavailable));
    offline.set_controls(FixtureControls::default());
    assert!(c.capabilities().unwrap().bibliographic);
}

fn v2(client: &Client, item: &str, request_id: &str, set: serde_json::Value, clear: &[&str]) -> Result<ItemUpdate, ClientError> {
    let rev = client.item_read(item).unwrap().revision_id;
    let edit = crate::biblio::MetadataEdit {
        metadata_schema_version: 2,
        item_id: item.into(),
        expected_revision_id: rev,
        request_id: request_id.into(),
        set: set.as_object().unwrap().clone(),
        clear: clear.iter().map(|s| s.to_string()).collect(),
        preview_id: None,
        selected_candidate_ids: vec![],
    };
    client.bibliographic_edit(edit, Sensitivity::Restricted)
}

#[test]
fn fixture_bibliographic_edit_follows_the_agents_rules() {
    let fixture = FixtureBackend::new(8);
    let client = Client::new(fixture.clone());
    let id = "item_fixture_000003"; // a book
    let rev = client.item_read(id).unwrap().revision_id;
    let before = client.bibliographic(id, &rev, Sensitivity::Restricted).unwrap();
    assert_eq!(before.item_type, "book");
    assert!(before.identifier("isbn").unwrap().is_valid());
    // Bound to the revision it was asked for.
    assert!(client.bibliographic(id, "rev_other", Sensitivity::Restricted).unwrap_err().is_stale_revision());
    // Field errors are field-scoped; nothing is written.
    let bad = v2(&client, id, "q1", serde_json::json!({"identifiers":[{"kind":"isbn","value":"978-0-306-40615-8"}]}), &[]).unwrap_err();
    assert_eq!(bad.field_error(), Some(("identifiers".into(), "checksum".into())));
    assert_eq!(client.item_read(id).unwrap().revision_id, rev);
    assert_eq!(v2(&client, id, "q2", serde_json::json!({"number_of_pages":0}), &[]).unwrap_err().field_error(), Some(("number_of_pages".into(), "invalid".into())));
    assert_eq!(v2(&client, id, "q3", serde_json::json!({"added_at":"2026"}), &[]).unwrap_err().field_error(), Some(("added_at".into(), "readonly".into())));
    let date = serde_json::json!({"published_date":{"value":"2023-02-29","precision":"day","raw":null}});
    assert_eq!(v2(&client, id, "q4", date, &[]).unwrap_err().field_error(), Some(("published_date".into(), "format".into())));
    // Unchanged is its own answer.
    let same = serde_json::json!({"publisher": before.publisher.clone().unwrap()});
    assert_eq!(v2(&client, id, "q5", same, &[]), Err(ClientError::Daemon("metadata_unchanged".into())));
    // A valid edit: normalized identifier with the raw input retained; one new revision.
    let ok = v2(&client, id, "q6", serde_json::json!({"identifiers":[{"kind":"isbn","value":"0-306-40615-2"}],"edition":"第 2 版"}), &["series_title"]).unwrap();
    assert_eq!(ok.previous_revision_id.as_deref(), Some(rev.as_str()));
    let after = client.bibliographic(id, &ok.revision_id, Sensitivity::Restricted).unwrap();
    assert_eq!(after.identifier("isbn").map(|i| (i.value.as_str(), i.raw_value.as_deref())), Some(("9780306406157", Some("0-306-40615-2"))));
    assert_eq!((after.edition.as_deref(), after.series_title.as_deref()), (Some("第 2 版"), None));
    // Replaying the same request returns the same receipt; another body under the id conflicts.
    let replay = crate::biblio::MetadataEdit {
        metadata_schema_version: 2, item_id: id.into(), expected_revision_id: rev.clone(), request_id: "q6".into(),
        set: serde_json::json!({"identifiers":[{"kind":"isbn","value":"0-306-40615-2"}],"edition":"第 2 版"}).as_object().unwrap().clone(),
        clear: vec!["series_title".into()], preview_id: None, selected_candidate_ids: vec![],
    };
    assert_eq!(client.bibliographic_edit(replay.clone(), Sensitivity::Restricted), Ok(ok.clone()));
    let mut other = replay;
    other.clear.clear();
    assert_eq!(client.bibliographic_edit(other, Sensitivity::Restricted), Err(ClientError::Daemon("request_id_conflict".into())));
    // A stale expected revision writes nothing.
    fixture.simulate_external_edit(id);
    let stale = crate::biblio::MetadataEdit { request_id: "q7".into(), expected_revision_id: ok.revision_id.clone(), ..crate::biblio::MetadataEdit {
        metadata_schema_version: 2, item_id: id.into(), expected_revision_id: String::new(), request_id: String::new(),
        set: serde_json::json!({"edition":"第 3 版"}).as_object().unwrap().clone(), clear: vec![], preview_id: None, selected_candidate_ids: vec![] } };
    assert!(client.bibliographic_edit(stale, Sensitivity::Restricted).unwrap_err().is_stale_revision());
}

#[test]
fn fixture_lookup_preview_conflicts_dependencies_expiry_and_errors() {
    let fixture = FixtureBackend::new(8);
    let client = Client::new(fixture.clone());
    let id = "item_fixture_000001"; // a journal article
    let rev = client.item_read(id).unwrap().revision_id;
    let doi = client.bibliographic(id, &rev, Sensitivity::Restricted).unwrap().identifier("doi").unwrap().display();
    let preview = client.lookup_preview(id, &rev, "doi", &doi, Sensitivity::Restricted).unwrap();
    assert_eq!(preview.provider, "crossref");
    let by_field = |f: &str| preview.candidates.iter().find(|c| c.field == f).unwrap().clone();
    assert!(by_field("title").unchanged && !by_field("title").conflict);
    assert!(by_field("creators").conflict);
    assert_eq!(by_field("creators").requires, ["institutions"]);
    assert!(!by_field("institutions").conflict && by_field("institutions").current_value.is_none());
    assert!(preview.notes.iter().any(|n| n == "cited_references_not_supplied"));
    let apply = |ids: Vec<String>, request_id: &str, rev: &str| {
        let edit = crate::biblio::MetadataEdit { metadata_schema_version: 2, item_id: id.into(), expected_revision_id: rev.into(), request_id: request_id.into(), set: Default::default(), clear: vec![], preview_id: Some(preview.preview_id.clone()), selected_candidate_ids: ids };
        client.bibliographic_edit(edit, Sensitivity::Restricted)
    };
    // The server still enforces the dependency the UI gates.
    let missing = apply(vec![by_field("creators").candidate_id], "a1", &rev).unwrap_err();
    assert_eq!(missing, ClientError::Daemon("candidate_dependency_missing:creators:institutions".into()));
    assert_eq!(missing.field_error(), Some(("creators".into(), "requires:institutions".into())));
    let receipt = apply(vec![by_field("creators").candidate_id, by_field("institutions").candidate_id], "a2", &rev).unwrap();
    let applied = client.bibliographic(id, &receipt.revision_id, Sensitivity::Restricted).unwrap();
    assert_eq!(applied.creators[0].institution_ids, ["inst_fixture_1"]);
    assert_eq!(applied.institutions.len(), 1);
    // The preview was bound to the old revision: applying it again is stale.
    assert!(apply(vec![by_field("abstract_text").candidate_id], "a3", &receipt.revision_id).unwrap_err().is_stale_revision());
    // Expiry, errors and lookups the agent refuses.
    let fresh = client.lookup_preview(id, &receipt.revision_id, "doi", &doi, Sensitivity::Restricted).unwrap();
    fixture.expire_lookup_previews();
    let edit = crate::biblio::MetadataEdit { metadata_schema_version: 2, item_id: id.into(), expected_revision_id: receipt.revision_id.clone(), request_id: "a4".into(), set: Default::default(), clear: vec![], preview_id: Some(fresh.preview_id.clone()), selected_candidate_ids: vec![fresh.candidates[0].candidate_id.clone()] };
    assert_eq!(client.bibliographic_edit(edit, Sensitivity::Restricted), Err(ClientError::Daemon("preview_expired".into())));
    assert_eq!(client.lookup_preview(id, &receipt.revision_id, "doi", "10.9999/not-on-item", Sensitivity::Restricted), Err(ClientError::Daemon("lookup_identifier_not_on_item".into())));
    // A book's ISBN goes to Open Library, whose notes say no author names are given.
    let book = "item_fixture_000003";
    let brev = client.item_read(book).unwrap().revision_id;
    let isbn = client.bibliographic(book, &brev, Sensitivity::Restricted).unwrap().identifier("isbn").unwrap().display();
    let p = client.lookup_preview(book, &brev, "isbn", &isbn, Sensitivity::Restricted).unwrap();
    assert_eq!(p.provider, "open_library");
    assert!(p.notes.iter().any(|n| n == "creators_not_resolved_author_keys_only") && !p.candidates.iter().any(|c| c.field == "creators"));
    fixture.set_controls(FixtureControls { lookup_error: Some("lookup_not_found".into()), ..Default::default() });
    let not_found = client.lookup_preview(book, &brev, "isbn", &isbn, Sensitivity::Restricted).unwrap_err();
    assert!(not_found.message().contains("不代表"), "not found is never worded as invalid");
    // Duplicates: item 49 shares item 1's DOI.
    let many = Client::new(FixtureBackend::new(60));
    let dups = many.identifier_duplicates(vec![crate::biblio::IdentifierQuery { kind: "doi".into(), value: "DOI:10.5555/FIXTURE.0001".into() }], Some("item_fixture_000001".into()), Sensitivity::Restricted).unwrap();
    assert_eq!(dups.iter().map(|d| d.item_id.as_str()).collect::<Vec<_>>(), ["item_fixture_000049"]);
}

#[test]
fn added_at_is_shown_in_local_time_and_never_replaced_by_captured_at() {
    use crate::biblio::{format_at_offset, parse_rfc3339};
    // UTC evening is the next morning in Asia/Taipei (UTC+8).
    let t = parse_rfc3339("2026-10-28T17:30:00Z").unwrap();
    assert_eq!(format_at_offset(t, 8 * 3600), "2026-10-29 01:30");
    assert_eq!(format_at_offset(t, 0), "2026-10-28 17:30");
    assert_eq!(parse_rfc3339("2026-10-29T01:30:00.250+08:00"), Some(t));
    assert_eq!(parse_rfc3339("2024-02-29T00:00:00Z").map(|e| format_at_offset(e, -3600)), Some("2024-02-28 23:00".into()));
    assert_eq!(parse_rfc3339("2026-10-28"), None);
    // Fixture: some items record added_at, others do not, whatever their captured_at.
    let client = Client::new(FixtureBackend::new(12));
    let read = |id: &str| client.bibliographic(id, &client.item_read(id).unwrap().revision_id, Sensitivity::Restricted).unwrap();
    let missing = read("item_fixture_000004");
    assert!(missing.added_at.is_none() && missing.captured_at.is_some());
    let present = read("item_fixture_000001");
    assert_ne!(present.added_at, present.captured_at, "added_at is its own commit time");
    assert!(crate::biblio::added_at_local(present.added_at.as_deref()).is_some());
}

#[test]
fn item_types_and_translation_reasons_read_as_words() {
    // Wire values are labelled; fixture labels are kept in the interface language; unknown values stay as given.
    assert_eq!(item_type_label("webpage"), "网页");
    assert_eq!(item_type_label("journal_article"), "期刊论文");
    assert_eq!(item_type_label("book"), "书籍");
    assert_eq!(item_type_label("论文"), "论文");
    assert_eq!(item_type_label("lab_notebook"), "lab_notebook");
    // Zotero's camelCase types read the same as snake_case.
    assert_eq!(item_type_label("journalArticle"), "期刊论文");
    assert_eq!(item_type_label("blogPost"), "博客文章");
    assert_eq!(item_type_label("note"), "笔记");
    assert_eq!(item_type_label("labNotebook"), "labNotebook", "unknown camelCase stays as given");
    assert_eq!(translation_reason_label("source_is_chinese"), "原文为中文，无需翻译");
    assert_eq!(translation_reason_label("provider_timeout"), "翻译服务响应超时");
    assert_eq!(translation_reason_label("new_reason"), "代理原因：new_reason");
    // The reason of a failed translation is read once, from the read-only status.
    let client = Client::new(FixtureBackend::new(16));
    let failed = client.library_item_read("item_fixture_000014").unwrap();
    assert_eq!((failed.translation_state, failed.translation_reason.as_deref()), (AnalysisState::Failed, Some("provider_timeout")));
    // A Chinese source is not queued; the request says why.
    let chinese = client.translation_request("item_fixture_000008").unwrap();
    assert_eq!((chinese.state.as_str(), chinese.reason.as_deref()), ("not_requested", Some("source_is_chinese")));
    let done = client.item_detail("item_fixture_000005").unwrap();
    assert_eq!((done.analysis.translation_state, done.analysis.translation_reason), (AnalysisState::Succeeded, None));
}

#[test]
fn group_tree_pages_are_bound_to_one_view_and_capability_gated() {
    let dir = tempfile::tempdir().unwrap();
    let (backend, path) = agent(&dir);
    // An agent without groups_v1: nothing beyond `capabilities` is sent.
    let server = serve_once(&path, 0o600, Some(r#"{"ok":true,"capabilities":{"schema_version":1,"features":[]}}"#));
    assert_eq!(Client::new(backend).groups(), Err(ClientError::Unsupported));
    let sent: serde_json::Value = serde_json::from_slice(&server.join().unwrap()).unwrap();
    assert_eq!(sent, serde_json::json!({"command":"capabilities"}));
    let command = Command::GroupTreeRead { max_sensitivity: Sensitivity::Restricted, offset: 200, limit: 200, expected_view: Some("sha256:v".into()) };
    assert_eq!(serde_json::to_value(&command).unwrap(), serde_json::json!({"command":"group_tree_read","max_sensitivity":"restricted","offset":200,"limit":200,"expected_view":"sha256:v"}));
    // Every node carries the version a write to it must name.
    let nodes = Client::new(FixtureBackend::new(8)).groups().unwrap();
    assert_eq!(nodes.len(), 15);
    assert!(nodes[0].parent_id.is_none() && nodes.iter().all(|n| n.version == "v1"));
}

#[test]
fn ai_minutes_round_trip_and_an_older_agent_without_them_hides_the_feature() {
    let item = |extra: &str| -> TranslationItem {
        serde_json::from_str(&format!(r#"{{"item_id":"i","revision_id":"r","summary_state":"not_requested","translation_state":"succeeded","title":"译名"{extra}}}"#)).unwrap()
    };
    // Older agent: no field at all.
    assert_eq!(item("").ai_minutes, None);
    let done = item(r#","ai_minutes":{"state":"succeeded","reason":null,"text":"纪要正文"}"#).ai_minutes.unwrap();
    assert_eq!((done.state.clone(), done.text()), (MinutesState::Succeeded, Some("纪要正文")));
    assert_eq!(done.state_line(), None);
    let running = item(r#","ai_minutes":{"state":"running","reason":null,"text":"不应显示"}"#).ai_minutes.unwrap();
    assert!(running.is_pending() && running.text().is_none());
    // Either language: another test in this process switches it briefly.
    assert!(matches!(running.state_line().as_deref(), Some("纪要生成中…" | "Generating minutes…")));
    let failed = item(r#","ai_minutes":{"state":"failed","reason":"provider_timeout","text":null}"#).ai_minutes.unwrap();
    assert!(matches!(failed.state_line().as_deref(), Some("纪要生成失败：模型服务响应超时" | "Minutes failed: The model service timed out")));
    // A newer state is kept; a malformed field never fails the read.
    assert_eq!(item(r#","ai_minutes":{"state":"paused"}"#).ai_minutes.unwrap().state, MinutesState::Other("paused".into()));
    assert_eq!(item(r#","ai_minutes":"oops""#).ai_minutes, None);
    // translation_request replies carry minutes next to `translation`.
    let r: Response = serde_json::from_str(r#"{"ok":true,"translation":{"state":"not_requested","reason":"source_is_chinese","revision_id":"r"},"ai_minutes":{"state":"queued","reason":null,"text":null}}"#).unwrap();
    assert_eq!(r.ai_minutes.unwrap().state, MinutesState::Queued);
    // An agent without `minutes_request_v1` makes minutes in the translation request.
    let fixture = FixtureBackend::new(12);
    fixture.set_controls(FixtureControls { no_minutes_request: true, ..Default::default() });
    let client = Client::new(fixture.clone());
    let zh = client.translation_request("item_fixture_000008").unwrap();
    assert_eq!((zh.state.as_str(), zh.reason.as_deref()), ("not_requested", Some("source_is_chinese")));
    assert!(zh.ai_minutes.unwrap().is_pending(), "a Chinese source still gets minutes");
    fixture.set_controls(FixtureControls { omit_ai_minutes: true, ..Default::default() });
    assert_eq!(client.library_item_read("item_fixture_000005").unwrap().ai_minutes, None);
}

#[test]
fn capabilities_read_chat_minutes_and_metadata() {
    let caps: biblio::AgentCapabilities = serde_json::from_str(r#"{"schema_version":1,"features":["item_chat_v1"],"translation_provider_v1":{"available":true,"minutes":true,"metadata":false,"purpose_routes":{"chat":"m1"}}}"#).unwrap();
    let c = Capabilities::from_agent(&caps);
    assert!(c.item_chat && c.chat_ready());
    assert_eq!((c.minutes, c.metadata_enrichment), (Some(true), Some(false)));
    let caps: biblio::AgentCapabilities = serde_json::from_str(r#"{"schema_version":1,"features":["item_chat_v1"],"translation_provider_v1":{"available":true,"purpose_routes":{"chat":null}}}"#).unwrap();
    assert!(!Capabilities::from_agent(&caps).chat_ready(), "no chat route");
    let caps: biblio::AgentCapabilities = serde_json::from_str(r#"{"schema_version":1,"features":[]}"#).unwrap();
    let c = Capabilities::from_agent(&caps);
    assert!(!c.item_chat && c.minutes.is_none());
}

#[test]
fn model_provenance_and_urls_are_read_and_unknown_origins_still_decode() {
    let client = Client::new(FixtureBackend::new(30));
    let id = chat_item();
    let rev = client.item_read(id).unwrap().revision_id;
    let b = client.bibliographic(id, &rev, Sensitivity::Restricted).unwrap();
    assert!(b.from_model("keywords") && b.from_model("creators") && b.from_model("published_date"));
    assert!(!b.from_model("language"));
    assert_eq!(b.link(), Some("https://weekly.example/a/recurrent-memory"));
    assert_eq!(b.link_host().as_deref(), Some("weekly.example"));
    assert!(b.site_name.is_none());
    let read: biblio::MetadataV2Read = serde_json::from_value(serde_json::json!({
        "metadata_schema_version": 2,
        "item": {"item_id": "i", "revision_id": "r", "item_type": "webpage", "source_url": "javascript:alert(1)",
            "metadata": {"keywords": {"value": ["k"], "provenance": {"origin": "some_future_origin", "provider_id": "x", "provider_version": "1"}}}}
    }))
    .unwrap();
    let b: Bibliographic = read.into();
    assert_eq!(b.keywords, ["k"]);
    assert_eq!(b.origins.get("keywords").map(String::as_str), Some("some_future_origin"));
    assert!(!b.from_model("keywords") && b.link().is_none(), "only http(s) links open");
}

fn chat_item() -> &'static str {
    fixture::SHOWCASE_ITEM
}

#[test]
fn item_chat_has_the_agents_shape_its_own_deadline_and_needs_the_capability() {
    use chat::*;
    let turns = vec![ChatTurn::user("第一问"), ChatTurn::assistant("第一答"), ChatTurn::user("第二问")];
    let request = ItemChatRequest { item_id: "item_a".into(), request_id: "chat-0123456789abcdef".into(), messages: turns };
    let command = Command::ItemChat { item_id: request.item_id.clone(), max_sensitivity: Sensitivity::Restricted, request_id: request.request_id.clone(), messages: request.messages.clone() };
    assert_eq!(
        serde_json::to_value(&command).unwrap(),
        serde_json::json!({"command":"item_chat","item_id":"item_a","max_sensitivity":"restricted","request_id":"chat-0123456789abcdef",
            "messages":[{"role":"user","content":"第一问"},{"role":"assistant","content":"第一答"},{"role":"user","content":"第二问"}]})
    );
    let id = new_chat_request_id();
    assert!((16..=128).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'));
    assert_ne!(id, new_chat_request_id());

    // Over the socket: the reply decodes and the slow call outlives the 300 ms library deadline.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agent.sock");
    let listener = UnixListener::bind(&path).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let server = thread::spawn(move || {
        let mut sent = vec![];
        for reply in [r#"{"ok":true,"schema_version":1,"capabilities":{"schema_version":1,"features":["item_chat_v1"]}}"#, r#"{"ok":true,"item_chat":{"request_id":"chat-0123456789abcdef","reply":"**答**","model":"m","finish":"length"}}"#] {
            let (mut stream, _) = listener.accept().unwrap();
            let mut header = [0u8; 4];
            stream.read_exact(&mut header).unwrap();
            let mut body = vec![0u8; u32::from_be_bytes(header) as usize];
            stream.read_exact(&mut body).unwrap();
            if !sent.is_empty() {
                thread::sleep(Duration::from_millis(700));
            }
            sent.push(body);
            stream.write_all(&Framing::encode(reply.as_bytes())).unwrap();
        }
        sent
    });
    let client = Client::new(AgentBackend::new(path).with_timeout(Duration::from_millis(300)));
    let reply = client.item_chat(&request).unwrap();
    assert!(reply.truncated() && reply.reply == "**答**");
    let sent = server.join().unwrap();
    assert_eq!(serde_json::from_slice::<serde_json::Value>(&sent[1]).unwrap(), serde_json::to_value(&command).unwrap());

    // Without item_chat_v1 nothing is sent.
    let fixture = FixtureBackend::new(8);
    fixture.set_controls(FixtureControls { no_item_chat: true, ..Default::default() });
    assert_eq!(Client::new(fixture).item_chat(&request), Err(ClientError::Unsupported));
}

#[test]
fn chat_history_is_trimmed_oldest_first_within_the_agents_limits() {
    use chat::*;
    // 50 turns: only the last 40 fit, and the window starts with a user turn.
    let mut history: Vec<ChatTurn> = (0..50).map(|i| if i % 2 == 0 { ChatTurn::user(format!("问{i}")) } else { ChatTurn::assistant(format!("答{i}")) }).collect();
    history.push(ChatTurn::user("最后一问"));
    let sent = trim_history(&history).unwrap();
    assert!(sent.len() <= MAX_CHAT_MESSAGES && valid_messages(&sent));
    assert_eq!(sent.first().unwrap().role, ChatRole::User);
    assert_eq!(sent.last().unwrap().content, "最后一问");
    assert_eq!(sent[sent.len() - 2].content, "答49");
    // Total bytes: oldest long turns go first; an over-long old turn is cut at a char boundary.
    let big = "长".repeat(6000); // 18 000 bytes
    let history = vec![ChatTurn::user(big.clone()), ChatTurn::assistant(big.clone()), ChatTurn::user(big.clone()), ChatTurn::assistant(big.clone()), ChatTurn::user(big.clone()), ChatTurn::assistant("短"), ChatTurn::user("新问题")];
    let sent = trim_history(&history).unwrap();
    assert!(valid_messages(&sent));
    assert!(sent.iter().map(|t| t.content.len()).sum::<usize>() <= MAX_CHAT_TOTAL_BYTES);
    assert!(sent.iter().all(|t| t.content.len() <= MAX_CHAT_MESSAGE_BYTES && std::str::from_utf8(t.content.as_bytes()).is_ok()));
    assert_eq!(sent.last().unwrap().content, "新问题");
    // A new message that alone breaks the limit, or an empty one, is not sent.
    assert!(trim_history(&[ChatTurn::user("长".repeat(6000))]).is_none());
    assert!(trim_history(&[ChatTurn::user("  ")]).is_none());
    assert!(trim_history(&[ChatTurn::assistant("答")]).is_none());
}

#[test]
fn translated_title_and_progress_round_trip_and_older_agents_omit_them() {
    let item = |extra: &str| -> TranslationItem {
        serde_json::from_str(&format!(r#"{{"item_id":"i","revision_id":"r","summary_state":"not_requested","translation_state":"failed","title":null{extra}}}"#)).unwrap()
    };
    let old = item("");
    assert_eq!((old.translated_title.clone(), old.translation_progress), (None, None));
    assert_eq!(old.chinese_title("Original"), None);
    let new = item(r#","translated_title":"中文标题","translation_progress":{"done_batches":5,"total_batches":18}"#);
    assert_eq!(new.translated_title.as_deref(), Some("中文标题"));
    assert_eq!(new.translation_progress, Some(TranslationProgress { done_batches: 5, total_batches: 18, started_at: None }));
    // The document failed, the title step did not: the title is still shown.
    assert_eq!(new.chinese_title("Original").as_deref(), Some("中文标题"));
    assert_eq!(new.chinese_title(" 中文标题 "), None, "same as the original: not shown");
    let back: TranslationItem = serde_json::from_value(serde_json::to_value(&new).unwrap()).unwrap();
    assert_eq!(back, new);
    // A malformed progress never fails the read.
    assert_eq!(item(r#","translation_progress":"5/18""#).translation_progress, None);
    // translation_request projection: new fields, null, and absent.
    let s: TranslationStatus = serde_json::from_str(r#"{"state":"running","reason":null,"revision_id":"r","translated_title":"中文标题","progress":{"done_batches":2,"total_batches":9}}"#).unwrap();
    assert_eq!((s.translated_title.as_deref(), s.progress.map(|p| p.done_batches)), (Some("中文标题"), Some(2)));
    let s: TranslationStatus = serde_json::from_str(r#"{"state":"queued","translated_title":null,"progress":null}"#).unwrap();
    assert_eq!((s.translated_title, s.progress), (None, None));
    let s: TranslationStatus = serde_json::from_str(r#"{"state":"queued"}"#).unwrap();
    assert_eq!((s.translated_title, s.progress), (None, None));
}

#[test]
fn progress_labels_show_fractions_only_when_known() {
    let p = |d, t| TranslationProgress { done_batches: d, total_batches: t, started_at: None };
    assert_eq!(p(5, 18).fraction().as_deref(), Some("5/18"));
    assert_eq!(p(20, 18).fraction().as_deref(), Some("18/18"), "never more than the total");
    assert_eq!(p(0, 0).fraction(), None);
    // Either language: another test switches it briefly.
    assert!(matches!(translating_label(Some(&p(5, 18))).as_str(), "翻译中 5/18" | "Translating 5/18"));
    assert!(matches!(translating_label(None).as_str(), "翻译中" | "Translating"));
    assert!(matches!(translating_label(Some(&p(0, 0))).as_str(), "翻译中" | "Translating"));
    assert!(matches!(unfinished_label(Some(&p(5, 18))).as_deref(), Some("翻译未完成（已完成 5/18）" | "Translation incomplete (5/18 done)")));
    assert_eq!(unfinished_label(Some(&p(0, 18))), None, "nothing finished: an ordinary failure");
    assert_eq!(unfinished_label(None), None);
}

#[test]
fn fixture_partial_translation_reports_title_and_progress_and_resumes() {
    let fixture = FixtureBackend::new(30);
    let client = Client::new(fixture.clone());
    let id = crate::fixture::PARTIAL_TRANSLATION_ITEM;
    let read = client.library_item_read(id).unwrap();
    assert_eq!(read.translation_state, AnalysisState::Failed);
    assert_eq!(read.translation_progress, Some(TranslationProgress { done_batches: 5, total_batches: 18, started_at: None }));
    assert!(read.title.is_none() && read.translated_title.is_some(), "title stays success-only; translated_title is early");
    let queued = client.translation_request(id).unwrap();
    assert_eq!(queued.state, "queued");
    assert_eq!(queued.progress.map(|p| p.done_batches), Some(5), "resumes from the stored batches");
}

#[test]
fn trash_needs_the_capability_lists_and_restores_then_the_item_reads_again() {
    use std::sync::{Arc, Mutex};
    struct Recording(FixtureBackend, Arc<Mutex<Vec<String>>>);
    impl Backend for Recording {
        fn send(&self, c: &Command) -> Result<Response, ClientError> {
            self.1.lock().unwrap().push(serde_json::to_value(c).unwrap()["command"].as_str().unwrap().to_string());
            self.0.send(c)
        }
        fn mode(&self) -> BackendMode {
            self.0.mode()
        }
    }
    assert_eq!(serde_json::to_value(Command::TrashList { cursor: None }).unwrap(), serde_json::json!({"command":"trash_list","cursor":null}));
    assert_eq!(serde_json::to_value(Command::ItemTrashRestore { item_id: "i".into() }).unwrap(), serde_json::json!({"command":"item_trash_restore","item_id":"i"}));
    let fixture = FixtureBackend::new(30);
    let sent = Arc::new(Mutex::new(vec![]));
    fixture.set_controls(FixtureControls { no_trash: true, ..Default::default() });
    let client = Client::new(Recording(fixture.clone(), sent.clone()));
    assert!(!client.capabilities().unwrap().trash);
    assert_eq!(client.trash_all(), Err(ClientError::Unsupported));
    assert_eq!(client.trash_restore("item_fixture_000030"), Err(ClientError::Unsupported));
    assert!(sent.lock().unwrap().iter().all(|c| c == "capabilities"), "{:?}", sent.lock().unwrap());

    fixture.set_controls(FixtureControls::default());
    let client = Client::new(Recording(fixture, sent.clone()));
    assert!(client.capabilities().unwrap().trash);
    let trash = client.trash_all().unwrap();
    assert_eq!(trash.len(), 3);
    let first = trash[0].clone();
    assert_eq!((first.reason.as_str(), first.kept_item_id.as_deref()), ("duplicate", Some("item_fixture_000001")));
    // A trashed item reads as not found and is left out of the list.
    assert!(client.item_read(&first.item_id).is_err());
    let before = client.library_query(&QuerySpec::default(), Sensitivity::Restricted, 0, 1, None).unwrap().total;
    let receipt = client.trash_restore(&first.item_id).unwrap();
    assert!(receipt.restored && receipt.item_id == first.item_id);
    assert_eq!(client.trash_all().unwrap().len(), 2);
    assert!(client.item_read(&first.item_id).is_ok());
    assert_eq!(client.library_query(&QuerySpec::default(), Sensitivity::Restricted, 0, 1, None).unwrap().total, before + 1);
    // Restoring again: the agent says it is not trashed.
    assert_eq!(client.trash_restore(&first.item_id), Err(ClientError::Daemon("item_not_trashed".into())));
}

#[test]
fn minutes_request_needs_its_capability_and_translation_no_longer_starts_minutes() {
    use std::sync::{Arc, Mutex};
    struct Recording(FixtureBackend, Arc<Mutex<Vec<String>>>);
    impl Backend for Recording {
        fn send(&self, c: &Command) -> Result<Response, ClientError> {
            self.1.lock().unwrap().push(serde_json::to_value(c).unwrap()["command"].as_str().unwrap().to_string());
            self.0.send(c)
        }
        fn mode(&self) -> BackendMode {
            self.0.mode()
        }
    }
    assert_eq!(
        serde_json::to_value(Command::MinutesRequest { item_id: "i".into(), max_sensitivity: Sensitivity::Restricted }).unwrap(),
        serde_json::json!({"command":"minutes_request","item_id":"i","max_sensitivity":"restricted"})
    );
    // An older agent: nothing is sent, and translation_request still makes minutes.
    let fixture = FixtureBackend::new(12);
    let sent = Arc::new(Mutex::new(vec![]));
    fixture.set_controls(FixtureControls { no_minutes_request: true, ..Default::default() });
    let old = Client::new(Recording(fixture.clone(), sent.clone()));
    assert!(!old.capabilities().unwrap().minutes_request);
    assert_eq!(old.minutes_request("item_fixture_000008"), Err(ClientError::Unsupported));
    assert!(sent.lock().unwrap().iter().all(|c| c == "capabilities"), "{:?}", sent.lock().unwrap());
    let reply = old.translation_request("item_fixture_000008").unwrap();
    assert_eq!(reply.ai_minutes.map(|m| m.state), Some(MinutesState::Queued));

    // A newer agent: the translation request leaves the minutes alone ...
    let fixture = FixtureBackend::new(12);
    let sent = Arc::new(Mutex::new(vec![]));
    let client = Client::new(Recording(fixture.clone(), sent.clone()));
    assert!(client.capabilities().unwrap().minutes_request);
    let reply = client.translation_request("item_fixture_000008").unwrap();
    assert_eq!((reply.state.as_str(), reply.reason.as_deref()), ("not_requested", Some("source_is_chinese")));
    assert_eq!(reply.ai_minutes.map(|m| m.state), Some(MinutesState::NotRequested));
    // ... and minutes_request starts them (Chinese source included).
    let minutes = client.minutes_request("item_fixture_000008").unwrap();
    assert!(minutes.is_pending() && minutes.text().is_none());
    assert_eq!(sent.lock().unwrap().last().map(String::as_str), Some("minutes_request"));
    assert!(client.library_item_read("item_fixture_000008").unwrap().ai_minutes.is_some_and(|m| m.is_pending()));
    assert_eq!(client.minutes_request("item_fixture_999999"), Err(ClientError::Daemon("library_resource_not_found".into())));
    // No model configured: the agent says so, in words the panel shows.
    fixture.set_controls(FixtureControls { no_minutes_provider: true, ..Default::default() });
    let err = client.minutes_request(crate::fixture::MINUTES_FAIL_ITEM).unwrap_err();
    assert_eq!(err, ClientError::Daemon("no_minutes_provider".into()));
    assert!(matches!(err.message().as_str(), "还没有配置纪要模型" | "No minutes model is configured"));
}

#[test]
fn conversation_read_wire_shape_capability_and_absent_fields() {
    let first = serde_json::to_value(Command::ConversationRead { item_id: "item_a".into(), max_sensitivity: Sensitivity::Restricted, cursor: None }).unwrap();
    assert_eq!(first, serde_json::json!({"command": "conversation_read", "item_id": "item_a", "max_sensitivity": "restricted"}));
    let next = serde_json::to_value(Command::ConversationRead { item_id: "item_a".into(), max_sensitivity: Sensitivity::Restricted, cursor: Some("-1".into()) }).unwrap();
    assert_eq!(next["cursor"], "-1");
    let old: Response = serde_json::from_str(r#"{"ok":true}"#).unwrap();
    assert!(old.conversation.is_none());
    // The history feature counts only together with item_chat_v1.
    let caps = |features: &[&str]| {
        let agent: biblio::AgentCapabilities = serde_json::from_value(serde_json::json!({"schema_version": 1, "features": features})).unwrap();
        biblio::Capabilities::from_agent(&agent).item_chat_history
    };
    assert!(caps(&["item_chat_v1", "item_chat_history_v1"]));
    assert!(!caps(&["item_chat_history_v1"]));
    assert!(!caps(&["item_chat_v1"]));
}

/// A page as the agent's `conversation_read` serializes it (shape of
/// `item_chat::Conversation` / `ConversationTurn` in the daemon tests: restored turns
/// with negative `seq` first, `truncated` only when set), plus a field from the future.
#[test]
fn conversation_page_from_the_agent_decodes_with_origin_and_unknown_fields() {
    let wire = r#"{"ok":true,"conversation":{"item_id":"item_a","version":"v1:4:2","next_cursor":"1","future_page_field":true,"turns":[
        {"seq":-2,"role":"user","content":"Restored question?","request_id":"restored:abc:00","created_at_ms":1000,"model":null,"origin":"restored_from_user_screenshot"},
        {"seq":-1,"role":"assistant","content":"Restored **answer**.","request_id":"restored:abc:01","created_at_ms":2000,"model":null,"origin":"restored_from_user_screenshot"},
        {"seq":1,"role":"user","content":"Live question","request_id":"history-request-0001","created_at_ms":1793000000000,"model":null,"origin":null,"future_turn_field":"x"},
        {"seq":2,"role":"assistant","content":"长","request_id":"history-request-0001","created_at_ms":1793000000001,"model":"Synthetic Sol","origin":null,"truncated":true}
    ]}}"#;
    let page = serde_json::from_str::<Response>(wire).unwrap().conversation.unwrap();
    assert_eq!((page.version.as_str(), page.next_cursor.as_deref()), ("v1:4:2", Some("1")));
    assert_eq!(page.turns.iter().map(|t| t.seq).collect::<Vec<_>>(), [-2, -1, 1, 2]);
    assert!(page.turns[0].restored() && page.turns[1].restored() && !page.turns[2].restored());
    assert_eq!(page.turns[3].model.as_deref(), Some("Synthetic Sol"));
    assert!(page.turns[3].truncated && !page.turns[2].truncated);
    assert_eq!(page.turns[2].chat_turn(), ChatTurn::user("Live question"));
    // An origin a newer agent adds still decodes (shown without the label).
    let other: chat::StoredTurn = serde_json::from_str(r#"{"seq":5,"role":"user","content":"q","request_id":"r","created_at_ms":1,"origin":"imported_elsewhere"}"#).unwrap();
    assert!(!other.restored());
}

/// Records every command; the fixture pages its history 3 turns at a time.
struct Commands(FixtureBackend, std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>);
impl Backend for Commands {
    fn send(&self, c: &Command) -> Result<Response, ClientError> {
        self.1.lock().unwrap().push(serde_json::to_value(c).unwrap());
        self.0.send(c)
    }
    fn mode(&self) -> BackendMode {
        BackendMode::Agent { socket: "/nonexistent".into() }
    }
}

#[test]
fn conversation_read_follows_next_cursor_across_negative_seq_until_null() {
    let fixture = FixtureBackend::new(30);
    fixture.set_controls(FixtureControls { instant_chat: true, ..Default::default() });
    let sent = std::sync::Arc::new(std::sync::Mutex::new(vec![]));
    let client = Client::new(Commands(fixture, sent.clone()));
    let turns = client.conversation_read(crate::fixture::CHAT_HISTORY_ITEM).unwrap();
    assert_eq!(turns.iter().map(|t| t.seq).collect::<Vec<_>>(), [-2, -1, 1, 2]);
    assert!(turns[0].restored() && !turns[3].restored());
    let reads: Vec<_> = sent.lock().unwrap().iter().filter(|c| c["command"] == "conversation_read").cloned().collect();
    assert_eq!(reads.len(), 2, "two pages");
    assert!(reads[0].get("cursor").is_none());
    assert_eq!(reads[1]["cursor"], "1");
    // Ask more: the history grows past another page boundary and is still read whole.
    for q in ["一", "二", "三"] {
        let r = ItemChatRequest { item_id: crate::fixture::CHAT_HISTORY_ITEM.into(), request_id: chat::new_chat_request_id(), messages: vec![ChatTurn::user(q)] };
        client.item_chat(&r).unwrap();
    }
    let turns = client.conversation_read(crate::fixture::CHAT_HISTORY_ITEM).unwrap();
    assert_eq!(turns.len(), 10);
    assert!(turns.windows(2).all(|w| w[0].seq < w[1].seq));
    assert_eq!(turns[9].content.contains("三"), true);
}

#[test]
fn item_chat_replay_returns_the_stored_reply_and_reuse_elsewhere_is_a_conflict() {
    let fixture = FixtureBackend::new(30);
    fixture.set_controls(FixtureControls { instant_chat: true, ..Default::default() });
    let client = Client::new(fixture);
    let item = crate::fixture::SHOWCASE_ITEM;
    let r = ItemChatRequest { item_id: item.into(), request_id: chat::new_chat_request_id(), messages: vec![ChatTurn::user("问")] };
    let first = client.item_chat(&r).unwrap();
    assert_eq!(client.item_chat(&r).unwrap().reply, first.reply, "a replay answers the same");
    assert_eq!(client.conversation_read(item).unwrap().len(), 2, "and stores nothing more");
    let other_question = ItemChatRequest { messages: vec![ChatTurn::user("另一个问题")], ..r.clone() };
    assert_eq!(client.item_chat(&other_question), Err(ClientError::Daemon("request_identity_conflict".into())));
    let other_item = ItemChatRequest { item_id: crate::fixture::CHAT_HISTORY_ITEM.into(), ..r };
    assert_eq!(client.item_chat(&other_item), Err(ClientError::Daemon("request_identity_conflict".into())));
    assert!(ClientError::Daemon("request_identity_conflict".into()).message().contains("请求编号"));
}

#[test]
fn query_items_read_translated_titles_and_added_at_and_tolerate_their_absence() {
    // As the agent serializes `memex_query::ItemSummary` (2026-11-03 candidate), plus a field
    // this client does not know.
    let wire = r#"{"schema_version":1,"generation":"g","view":"v","total":2,"offset":0,"next_offset":null,
        "sources":[],"item_types":[],
        "items":[
          {"item_id":"a","revision_id":"r","title":"Attention Is Not All You Need","source":"https://x.example","item_type":"web_page",
           "captured_at":null,"added_at":"2026-11-02T08:00:00Z","sensitivity":"public","group_ids":["g1"],
           "translated_title":"注意力并非全部","title_source":"title_only","future_field":{"x":1}},
          {"item_id":"b","revision_id":"r","title":"旧条目","source":"opaque","item_type":"note","captured_at":null,"sensitivity":"public"}]}"#;
    let w: QueryWindow = serde_json::from_str(wire).unwrap();
    let (a, b) = (&w.items[0], &w.items[1]);
    assert_eq!((a.translated_title.as_deref(), a.title_source.as_deref(), a.added_at.as_deref()), (Some("注意力并非全部"), Some("title_only"), Some("2026-11-02T08:00:00Z")));
    assert_eq!((b.translated_title.as_deref(), b.title_source.as_deref(), b.added_at.as_deref()), (None, None, None));
    // Display: translation, a visible separator, the original; the original alone otherwise.
    assert_eq!(display_title(&a.title, a.translated_title.as_deref()), "注意力并非全部 ｜ Attention Is Not All You Need");
    assert_eq!(display_title(&b.title, None), "旧条目");
    assert_eq!(display_title("同名", Some(" 同名 ")), "同名", "a translation equal to the original is not repeated");
    assert_eq!(display_title("T", Some("  ")), "T");
    assert_eq!(serde_json::to_value(SortKey::AddedAt).unwrap(), "added_at");
}

#[test]
fn highlights_v2_spans_and_sides_decode_and_v1_items_still_do() {
    // The agent's v2 wire shape (`memex-agent` insights.rs `Highlight`/`Span`, `InsightProjection`).
    let wire = r#"{"highlights":{"state":"succeeded","reason":null,"dropped":1,"sides":"original_and_translation","items":[
          {"block_index":2,"quote":"key term","style":"positive","label":null,
           "original":{"text":"key term","block":2,"start":9,"end":17},"translation":{"text":"关键词","block":2,"start":12,"end":21}},
          {"block_index":3,"quote":"只在译文","style":"bold","label":"要点","original":null,"translation":{"text":"只在译文","block":3,"start":0,"end":12}},
          {"block_index":4,"quote":"v1 quote","style":"highlight"},
          {"block_index":5,"quote":"bad span","style":"negative","original":{"text":"bad span","block":5,"start":-1,"end":3}}]},
        "quotes":{"state":"not_requested","reason":null,"items":null,"dropped":0},
        "fact_check":{"state":"unavailable","reason":"insight_translation_changed","items":null,"dropped":0,"basis":"model_knowledge"}}"#;
    let ai: crate::insights::AiInsights = serde_json::from_str(wire).unwrap();
    let h = ai.highlights.unwrap();
    assert_eq!(h.sides.as_deref(), Some(crate::insights::SIDES_BOTH));
    let items = h.done().unwrap();
    assert_eq!(items[0].original.as_ref().map(|s| (s.start, s.end)), Some((9, 17)));
    assert_eq!(items[0].translation.as_ref().map(|s| s.text.as_str()), Some("关键词"));
    assert!(items[1].original.is_none() && items[1].has_spans());
    assert!(!items[2].has_spans(), "a v1 item: no spans, found by its quote");
    assert!(!items[3].has_spans(), "a malformed span reads as absent, the section still decodes");
    // A v1 section (no `sides`) is unchanged.
    let v1: crate::insights::InsightSection<crate::insights::HighlightItem> = serde_json::from_str(r#"{"state":"succeeded","items":[{"block_index":0,"quote":"q","style":"bold"}]}"#).unwrap();
    assert_eq!((v1.sides, v1.items[0].has_spans()), (None, false));
}

#[test]
fn added_at_sort_puts_unknown_times_last_and_an_older_agent_refuses_it() {
    let fixture = FixtureBackend::new(40);
    let client = Client::new(fixture.clone());
    for descending in [true, false] {
        let spec = QuerySpec { sort: SortKey::AddedAt, descending, ..Default::default() };
        let w = client.library_query(&spec, Sensitivity::Restricted, 0, 40, None).unwrap();
        let known: Vec<&str> = w.items.iter().filter_map(|i| i.added_at.as_deref()).collect();
        let first_unknown = w.items.iter().position(|i| i.added_at.is_none()).unwrap();
        assert!(first_unknown > 0 && w.items[first_unknown..].iter().all(|i| i.added_at.is_none()), "unknown last ({descending})");
        let mut sorted = known.clone();
        sorted.sort();
        if descending {
            sorted.reverse();
        }
        assert_eq!(known, sorted);
    }
    fixture.set_controls(FixtureControls { no_added_at_sort: true, ..Default::default() });
    let spec = QuerySpec { sort: SortKey::AddedAt, ..Default::default() };
    assert_eq!(client.library_query(&spec, Sensitivity::Restricted, 0, 10, None), Err(ClientError::Unsupported));
    assert!(client.library_query(&QuerySpec::default(), Sensitivity::Restricted, 0, 10, None).is_ok());
}

#[test]
fn agent_version_has_the_agents_shape_and_a_null_hash_decodes() {
    assert_eq!(serde_json::to_value(Command::AgentVersion).unwrap(), serde_json::json!({"command": "agent_version"}));
    let r: Response = serde_json::from_str(r#"{"ok":true,"agent_version":{"version":"0.1.0","binary_sha256":null}}"#).unwrap();
    assert_eq!(r.agent_version, Some(AgentVersion { version: "0.1.0".into(), binary_sha256: None, query_origin: false }));
    let fixture = FixtureBackend::new(3);
    fixture.set_controls(FixtureControls { legacy_agent: true, ..Default::default() });
    assert_eq!(Client::new(fixture).agent_version(), Err(ClientError::Unsupported), "an older agent: unknown, not an error screen");
}

#[test]
fn long_original_reads_join_utf8_segments_and_reject_tampering() {
    let fixture = FixtureBackend::new(1);
    let client = Client::new(fixture.clone());
    let old = client.item_read("item_fixture_000000").unwrap();
    let markdown = format!("# 合成首端\n\n{}\n\n合成末端", "中文🌹x".repeat(30_001));
    client.edit_markdown(crate::reading::MarkdownEdit { item_id: old.item_id.clone(), expected_revision_id: old.revision_id, request_id: "long-body-test".into(), markdown: markdown.clone() }).unwrap();
    assert_eq!(client.item_read(&old.item_id).unwrap().markdown, markdown);
    struct Damaged(FixtureBackend);
    impl Backend for Damaged {
        fn mode(&self) -> BackendMode { self.0.mode() }
        fn send(&self, command: &Command) -> Result<Response, ClientError> {
            let mut response = self.0.send(command)?;
            if let Some(slice) = &mut response.context { slice.content_sha256 = "sha256:wrong".into(); }
            Ok(response)
        }
    }
    assert_eq!(Client::new(Damaged(fixture)).item_read(&old.item_id), Err(ClientError::InvalidFrame));
}

#[test]
fn receipt_origin_is_an_optional_server_query_with_bound_pages() {
    let fixture = FixtureBackend::new(7);
    let client = Client::new(fixture.clone());
    assert!(client.agent_version().unwrap().query_origin);
    let mut spec = QuerySpec { origin: Some(ItemOrigin::Import), ..Default::default() };
    assert_eq!(serde_json::to_value(&spec).unwrap()["origin"], "import");
    let first = client.library_query(&spec, Sensitivity::Restricted, 0, 2, None).unwrap();
    assert_eq!((first.total, first.items.len()), (3, 2));
    assert_eq!(first.sources.iter().map(|f| f.count).sum::<usize>(), 2, "item 6 has no source URL but remains imported");
    let last = client.library_query(&spec, Sensitivity::Restricted, 2, 2, Some(first.view.clone())).unwrap();
    let mut ids: Vec<_> = first.items.iter().chain(&last.items).map(|i| i.item_id.as_str()).collect();
    ids.sort();
    assert_eq!(ids, ["item_fixture_000000", "item_fixture_000003", "item_fixture_000006"]);
    spec.origin = Some(ItemOrigin::Capture);
    assert!(client.library_query(&spec, Sensitivity::Restricted, 2, 2, Some(first.view.clone())).unwrap_err().is_stale_view());
    assert_eq!(client.library_query(&spec, Sensitivity::Restricted, 0, 2, None).unwrap().total, 4);
    spec.origin = None;
    assert!(serde_json::to_value(&spec).unwrap().get("origin").is_none(), "old daemons get the old query shape");
    fixture.set_controls(FixtureControls { no_origin_filter: true, ..Default::default() });
    assert!(!client.agent_version().unwrap().query_origin);
    assert_eq!(client.library_query(&spec, Sensitivity::Restricted, 0, 2, None).unwrap().total, 7);
}

#[test]
fn reader_v1_serves_a_paired_book_by_edition_never_the_canonical_text_as_original() {
    use crate::book::ReaderDocument;
    let fixture = FixtureBackend::new(30);
    let client = Client::new(fixture);
    let item = "item_fixture_000028";
    let canonical = client.item_read(item).unwrap();
    let doc: ReaderDocument = client.reader_toc(item, &canonical.revision_id).unwrap();
    assert!(doc.is_book());
    assert_eq!((doc.edition("original").unwrap().language.as_str(), doc.edition("translation").unwrap().language.as_str()), ("en", "zh"));
    // Reading order: the translator's preface has no counterpart and keeps its own stop.
    let entries = doc.entries();
    assert_eq!(entries.len(), 4);
    assert_eq!((entries[0].original, entries[0].translation), (None, Some(0)));
    assert_eq!((entries[3].original, entries[3].translation), (Some(2), Some(3)));
    // A chapter larger than one chunk is read whole, by following next_offset.
    let en = client.reader_chapter(&doc, "original", "chapter-3").unwrap();
    let zh = client.reader_chapter(&doc, "translation", "chapter-3").unwrap();
    assert!(en.len() > crate::book::CHUNK_LIMIT && zh.len() > crate::book::CHUNK_LIMIT);
    let ch = &doc.edition("original").unwrap().chapters[2];
    assert_eq!(en.len(), ch.end - ch.start);
    assert!(en.starts_with("## Chapter 3") && en.trim_end().ends_with("(3.140)"));
    assert!(zh.starts_with("## 第三章") && zh.trim_end().ends_with("（3.200）"));
    // The canonical text is the translation, not the English original.
    assert!(canonical.markdown.contains(&zh) && !canonical.markdown.contains("Chapter 3"));
    // A revision other than the manifest's is refused, not served.
    assert!(matches!(client.reader_toc(item, "rev_other"), Err(ClientError::Daemon(c)) if c == "stale_item_revision"));
}

#[test]
fn chapter_alignment_pages_validate_and_a_repair_job_commits_a_new_revision() {
    use crate::book::{AlignmentStatus, JobState, PairKind, Relation};
    let fixture = FixtureBackend::new(30);
    let client = Client::new(fixture.clone());
    let item = "item_fixture_000028";
    let rev = client.item_read(item).unwrap().revision_id;
    let doc = client.reader_toc(item, &rev).unwrap();
    let text = |doc: &crate::book::ReaderDocument, side: &str, key: &str| client.reader_chapter(doc, side, key).unwrap();

    // Chapter 1: aligned, with n:1, 1:n and the translator's note as its own row.
    let a = client.reader_alignment(&doc, "chapter-1").unwrap();
    assert_eq!(a.status, AlignmentStatus::Aligned);
    a.validate(&text(&doc, "original", "chapter-1"), Some(&text(&doc, "translation", "chapter-1"))).unwrap();
    let last = a.pairs.last().unwrap();
    assert_eq!((last.kind, last.relation, last.o), (PairKind::Note, Relation::TranslationOnly, None));
    // A tampered range is refused rather than displayed.
    let mut bad = a.clone();
    bad.pairs[1].o = Some([bad.pairs[1].o.unwrap()[0], bad.pairs[1].o.unwrap()[1] - 1]);
    assert!(bad.validate(&text(&doc, "original", "chapter-1"), Some(&text(&doc, "translation", "chapter-1"))).is_err());

    // Chapter 2: partial; unresolved rows carry one side each.
    let a = client.reader_alignment(&doc, "chapter-2").unwrap();
    assert_eq!(a.status, AlignmentStatus::Partial);
    assert!(a.pairs.iter().filter(|p| p.relation == Relation::Unresolved).all(|p| p.o.is_some() != p.t.is_some()));

    // Chapter 3: no index yet; its repair job runs and commits a new revision.
    let missing = client.reader_alignment(&doc, "chapter-3").unwrap();
    assert!(missing.is_missing() && missing.pairs.is_empty());
    let job = client.reader_align_request(&doc, "chapter-3", "req-fixture-1", "missing").unwrap();
    assert!(!job.state.is_terminal());
    // Replaying the same id and payload returns the same job; another payload conflicts.
    assert_eq!(client.reader_align_request(&doc, "chapter-3", "req-fixture-1", "missing").unwrap().job_id, job.job_id);
    assert!(matches!(client.reader_align_request(&doc, "chapter-2", "req-fixture-1", "missing"), Err(ClientError::Daemon(c)) if c == "request_identity_conflict"));
    assert!(matches!(client.reader_align_request(&doc, "chapter-2", "req-fixture-2", &a.version), Err(ClientError::Daemon(c)) if c == "reader_job_in_progress"));
    std::thread::sleep(std::time::Duration::from_millis(2300));
    let done = client.reader_align_status(item, &job.job_id).unwrap();
    assert_eq!(done.state, JobState::Succeeded);
    let new_rev = done.result_revision_id.clone().unwrap();
    assert_ne!(new_rev, rev);
    // The old revision is stale; the new one reads the committed index, paged.
    assert!(matches!(client.reader_alignment(&doc, "chapter-3"), Err(ClientError::Daemon(c)) if c == "stale_item_revision"));
    let doc = client.reader_toc(item, &new_rev).unwrap();
    let a = client.reader_alignment(&doc, "chapter-3").unwrap();
    assert!(a.pairs.len() > crate::book::ALIGNMENT_PAGE, "two pages");
    assert_eq!(Some(a.version.clone()), done.alignment_version);
    a.validate(&text(&doc, "original", "chapter-3"), Some(&text(&doc, "translation", "chapter-3"))).unwrap();

    // A failed job is reported, and needs a new request.
    fixture.fail_next_repair();
    let v = client.reader_alignment(&doc, "chapter-2").unwrap().version;
    let job = client.reader_align_request(&doc, "chapter-2", "req-fixture-3", &v).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let failed = client.reader_align_status(item, &job.job_id).unwrap();
    assert_eq!((failed.state, failed.error_code.as_deref()), (JobState::Failed, Some("reader_repair_failed")));
}

/// `reader_chapter_edit_v1` against a stand-in agent that follows the frozen
/// contract: staged begin/chunk/commit, exact begin replay, lost responses.
mod chapter_edit {
    use super::*;
    use crate::book::{edit_chunks, sha256_hex, ChapterEdit, EDIT_CHUNK_BYTES, EDIT_FEATURE};
    use serde_json::{json, Value};
    use std::sync::Mutex;

    const BASE: &str = "# First light\n\nOne morning.\n";

    #[derive(Default)]
    struct State {
        features: Vec<&'static str>,
        /// Reader requests as sent.
        sent: Vec<Value>,
        revision: String,
        chapter: String,
        /// (request id, begin request, received bytes).
        stage: Option<(String, Value, Vec<u8>)>,
        committed: Option<(String, Value)>,
        /// The reader call with this index is applied, then its response is lost.
        lose: Option<usize>,
        /// Corrupt the reply of this op.
        tamper: Option<(&'static str, &'static str)>,
        stale: bool,
    }

    struct Agent(Mutex<State>);

    impl Backend for Agent {
        fn send(&self, c: &Command) -> Result<Response, ClientError> {
            let mut s = self.0.lock().unwrap();
            let ok = |v: Value| Ok(Response { ok: true, reader: Some(v), ..Default::default() });
            let err = |c: &str| Err(ClientError::Daemon(c.into()));
            let Command::Reader { request, .. } = c else {
                let features: Vec<Value> = s.features.iter().map(|f| json!(f)).collect();
                return Ok(serde_json::from_value(json!({"ok": true, "capabilities": {"schema_version": 1, "features": features}})).unwrap());
            };
            let r = serde_json::to_value(request).unwrap();
            let index = s.sent.len();
            s.sent.push(r.clone());
            let mut reply = match r["op"].as_str().unwrap() {
                "edit_begin" => {
                    if s.stale { return err("stale_item_revision"); }
                    let id = r["request_id"].as_str().unwrap().to_string();
                    let mut begin = r.clone();
                    begin.as_object_mut().unwrap().remove("op");
                    if let Some((rid, receipt)) = &s.committed {
                        if *rid == id { json!({"edit_id": "edit_1", "received_bytes": r["bytes"], "max_chunk_bytes": 8192, "state": "committed", "receipt": receipt}) } else { return err("request_id_conflict"); }
                    } else {
                        match &s.stage {
                            Some((rid, b, _)) if *rid == id && *b != begin => return err("request_id_conflict"),
                            Some((rid, _, _)) if *rid == id => {}
                            _ => s.stage = Some((id, begin, Vec::new())),
                        }
                        let received = s.stage.as_ref().unwrap().2.len();
                        json!({"edit_id": "edit_1", "received_bytes": received, "max_chunk_bytes": 8192, "state": "uploading", "receipt": null})
                    }
                }
                "edit_chunk" => {
                    let md = r["markdown"].as_str().unwrap().as_bytes().to_vec();
                    let offset = r["offset"].as_u64().unwrap() as usize;
                    let Some((_, _, buf)) = s.stage.as_mut() else { return err("reader_edit_not_found") };
                    if md.is_empty() || md.len() > EDIT_CHUNK_BYTES { return err("invalid_reader_edit"); }
                    if offset != buf.len() { return err("reader_edit_upload_conflict"); }
                    buf.extend(md);
                    json!({"edit_id": r["edit_id"], "received_bytes": buf.len()})
                }
                "edit_commit" => {
                    if let Some((_, receipt)) = &s.committed { receipt.clone() } else {
                        let (rid, begin, buf) = s.stage.take().unwrap();
                        let body = String::from_utf8(buf).unwrap();
                        assert_eq!(body.len(), begin["bytes"].as_u64().unwrap() as usize);
                        assert_eq!(sha256_hex(&body), begin["sha256"].as_str().unwrap());
                        let changed = body != s.chapter;
                        if changed { s.revision = format!("{}x", s.revision); s.chapter = body.clone(); }
                        let receipt = json!({"item_id": begin["item_id"], "request_id": rid, "revision_id": s.revision, "side": begin["side"], "chapter": begin["chapter"], "chapter_sha256": sha256_hex(&body), "changed": changed});
                        s.committed = Some((rid, receipt.clone()));
                        receipt
                    }
                }
                op => panic!("unexpected {op}"),
            };
            if let Some((op, field)) = s.tamper {
                if r["op"] == op { reply[field] = match &reply[field] { Value::Bool(b) => json!(!b), Value::Number(n) => json!(n.as_u64().unwrap() + 1), _ => json!("tampered") }; }
            }
            if s.lose == Some(index) { return Err(ClientError::Timeout); }
            ok(reply)
        }
        fn mode(&self) -> BackendMode {
            BackendMode::Agent { socket: "/nonexistent".into() }
        }
    }

    fn start() -> (Client, std::sync::Arc<Agent>) {
        let a = std::sync::Arc::new(Agent(Mutex::new(State { features: vec![crate::book::READER_FEATURE, EDIT_FEATURE], revision: "revision_1".into(), chapter: BASE.into(), ..Default::default() })));
        struct Shared(std::sync::Arc<Agent>);
        impl Backend for Shared {
            fn send(&self, c: &Command) -> Result<Response, ClientError> { self.0.send(c) }
            fn mode(&self) -> BackendMode { self.0.mode() }
        }
        (Client::new(Shared(a.clone())), a)
    }

    fn edit(markdown: &str) -> ChapterEdit {
        ChapterEdit { item_id: "item_1".into(), revision_id: "revision_1".into(), side: "original".into(), chapter: "chapter-1".into(),
            expected_chapter_sha256: sha256_hex(BASE), request_id: "desktop-edit-1".into(), markdown: markdown.into() }
    }

    #[test]
    fn chunks_end_on_code_points_and_cover_the_text() {
        let text = format!("a{}", "晨光🙂".repeat(3000));
        let parts = edit_chunks(&text, 0, EDIT_CHUNK_BYTES);
        assert!(parts.len() > 2);
        assert_eq!(parts.first().unwrap().start, 0);
        assert_eq!(parts.last().unwrap().end, text.len());
        for w in parts.windows(2) { assert_eq!(w[0].end, w[1].start); }
        for p in &parts { assert!(p.len() <= EDIT_CHUNK_BYTES && p.len() > EDIT_CHUNK_BYTES - 4 || p.end == text.len()); assert!(text.is_char_boundary(p.end)); }
        // Resuming starts exactly at the acknowledged offset.
        assert_eq!(edit_chunks(&text, parts[1].start, EDIT_CHUNK_BYTES)[0], parts[1]);
        assert!(edit_chunks(&text, text.len(), EDIT_CHUNK_BYTES).is_empty());
    }

    #[test]
    fn exact_request_shapes_and_a_changed_receipt() {
        let (client, agent) = start();
        let body = "# First light\n\nOne swallow.\n";
        let receipt = client.reader_edit_chapter(&edit(body)).unwrap();
        assert!(receipt.changed);
        assert_eq!(receipt.revision_id, "revision_1x");
        let sent = agent.0.lock().unwrap().sent.clone();
        assert_eq!(sent, vec![
            json!({"op": "edit_begin", "item_id": "item_1", "revision_id": "revision_1", "side": "original", "chapter": "chapter-1",
                "expected_chapter_sha256": sha256_hex(BASE), "request_id": "desktop-edit-1", "bytes": body.len(), "sha256": sha256_hex(body)}),
            json!({"op": "edit_chunk", "item_id": "item_1", "edit_id": "edit_1", "offset": 0, "markdown": body}),
            json!({"op": "edit_commit", "item_id": "item_1", "edit_id": "edit_1"}),
        ]);
        // The whole envelope, as the agent reads it.
        let command = Command::Reader { max_sensitivity: Sensitivity::Restricted, request: crate::book::ReaderRequest::EditCommit { item_id: "i".into(), edit_id: "e".into() } };
        assert_eq!(serde_json::to_value(command).unwrap(), json!({"command": "reader", "max_sensitivity": "restricted", "request": {"op": "edit_commit", "item_id": "i", "edit_id": "e"}}));
    }

    #[test]
    fn identical_body_is_unchanged_on_the_same_revision() {
        let (client, _) = start();
        let receipt = client.reader_edit_chapter(&edit(BASE)).unwrap();
        assert_eq!((receipt.changed, receipt.revision_id.as_str()), (false, "revision_1"));
    }

    #[test]
    fn multi_chunk_cjk_upload_resumes_from_received_bytes_after_a_lost_response() {
        let (client, agent) = start();
        let body = format!("# 晨光\n\n{}\n", "合成读者沿着晨光前行。🙂".repeat(1200));
        assert!(body.len() > 3 * EDIT_CHUNK_BYTES);
        // The second chunk is written, but its reply is lost.
        agent.0.lock().unwrap().lose = Some(2);
        assert_eq!(client.reader_edit_chapter(&edit(&body)), Err(ClientError::Timeout));
        let acknowledged = agent.0.lock().unwrap().stage.as_ref().unwrap().2.len();
        agent.0.lock().unwrap().lose = None;
        let receipt = client.reader_edit_chapter(&edit(&body)).unwrap();
        assert!(receipt.changed);
        let s = agent.0.lock().unwrap();
        assert_eq!(s.chapter, body);
        let chunks: Vec<&Value> = s.sent.iter().filter(|r| r["op"] == "edit_chunk").collect();
        for c in &chunks { assert!(c["markdown"].as_str().unwrap().len() <= EDIT_CHUNK_BYTES); }
        // After the replayed begin, the upload continued at the acknowledged byte.
        let replay = s.sent.iter().rposition(|r| r["op"] == "edit_begin").unwrap();
        assert_eq!(s.sent[replay + 1]["offset"], json!(acknowledged));
        assert_eq!(s.sent.iter().filter(|r| r["op"] == "edit_commit").count(), 1);
    }

    #[test]
    fn a_lost_commit_reply_replays_to_the_original_receipt() {
        let (client, agent) = start();
        agent.0.lock().unwrap().lose = Some(2);
        let body = "# First light\n\nTwo birds.\n";
        assert_eq!(client.reader_edit_chapter(&edit(body)), Err(ClientError::Timeout));
        let receipt = client.reader_edit_chapter(&edit(body)).unwrap();
        assert_eq!((receipt.changed, receipt.revision_id.as_str()), (true, "revision_1x"));
        let s = agent.0.lock().unwrap();
        assert_eq!(s.sent.len(), 4, "the replay is only edit_begin");
        assert_eq!(s.revision, "revision_1x", "no second revision");
    }

    #[test]
    fn mismatched_replies_are_not_accepted() {
        for (op, field) in [("edit_chunk", "received_bytes"), ("edit_chunk", "edit_id"), ("edit_commit", "chapter_sha256"), ("edit_commit", "changed"),
            ("edit_commit", "request_id"), ("edit_commit", "side"), ("edit_begin", "state")] {
            let (client, agent) = start();
            agent.0.lock().unwrap().tamper = Some((op, field));
            assert_eq!(client.reader_edit_chapter(&edit("# x\n\ny\n")), Err(ClientError::InvalidFrame), "{op}.{field}");
        }
    }

    #[test]
    fn invalid_bodies_and_missing_capability_send_nothing_and_stale_is_reported() {
        let (client, agent) = start();
        for body in ["", " \n\t ", "a\0b"] {
            assert_eq!(client.reader_edit_chapter(&edit(body)), Err(ClientError::Daemon("invalid_reader_edit".into())));
        }
        let big = "x".repeat(crate::book::MAX_EDIT_BYTES + 1);
        assert_eq!(client.reader_edit_chapter(&edit(&big)), Err(ClientError::Daemon("reader_edit_too_large".into())));
        assert!(agent.0.lock().unwrap().sent.is_empty());
        agent.0.lock().unwrap().stale = true;
        let e = client.reader_edit_chapter(&edit("# changed\n")).unwrap_err();
        assert!(e.is_stale_revision());
        assert!(!ClientError::Daemon("reader_edit_upload_conflict".into()).message().contains("reader_edit"));

        let (client, agent) = start();
        agent.0.lock().unwrap().features = vec![crate::book::READER_FEATURE];
        assert!(!client.reader_edit_supported());
        assert_eq!(client.reader_edit_chapter(&edit("# changed\n")), Err(ClientError::Unsupported));
        assert!(agent.0.lock().unwrap().sent.is_empty());
    }
}

/// Repair jobs gained additive keys (operation, failure_stage/_reason, per-dispatch
/// usage); old agents omit them. Both decode to the same job.
#[test]
fn repair_jobs_decode_with_and_without_the_additive_job_fields() {
    use crate::book::JobState;
    use serde_json::json;
    struct One(serde_json::Value);
    impl Backend for One {
        fn send(&self, _: &Command) -> Result<Response, ClientError> { Ok(Response { ok: true, reader: Some(self.0.clone()), ..Default::default() }) }
        fn mode(&self) -> BackendMode { BackendMode::Agent { socket: "/nonexistent".into() } }
    }
    let old = json!({"job_id": "job_1", "request_id": "r1", "item_id": "item_1", "revision_id": "revision_1", "chapter": "chapter-1",
        "state": "failed", "stage": "done", "result_revision_id": null, "alignment_version": null, "alignment_status": null, "error_code": "reader_repair_failed",
        "budget": {"max_requests": 2, "max_input_bytes_per_request": 8192, "max_completion_tokens_per_request": 1024},
        "usage": {"requests_dispatched": 2, "reported_total_tokens": 50, "unknown_usage_requests": 0}, "elapsed_ms": 12});
    let mut new = old.clone();
    for (k, v) in [("operation", json!("repair")), ("failure_stage", json!(null)), ("failure_reason", json!(null))] { new[k] = v; }
    new["usage"]["dispatches"] = json!([{"input_tokens": null, "output_tokens": 7, "total_tokens": 37}, {"input_tokens": 11, "output_tokens": 2, "total_tokens": 13}]);
    new["usage"]["input_tokens"] = json!(null);
    new["usage"]["output_tokens"] = json!(9);
    let mut failed = new.clone();
    failed["failure_stage"] = json!("matching");
    failed["failure_reason"] = json!("invalid_model_output");
    let mut decoded = vec![];
    for job in [old, new, failed] {
        let client = Client::new(One(json!({"job": job})));
        decoded.push(client.reader_align_status("item_1", "job_1").unwrap());
    }
    assert_eq!(decoded[0].state, JobState::Failed);
    assert_eq!(decoded[0].error_code.as_deref(), Some("reader_repair_failed"));
    assert!(decoded.iter().all(|j| *j == decoded[0]));
}

/// `reader_chapter_translation_v1` against a stand-in agent following the frozen
/// contract: preview → quote, request admission with same-id replay, status,
/// chunked reads of the published projection (reads never dispatch).
mod chapter_translation {
    use super::*;
    use crate::book::{sha256_hex, JobState, ReaderDocument, TRANSLATION_FEATURE};
    use serde_json::{json, Value};
    use std::sync::{Arc, Mutex};

    const AI: &str = "# 附录\n\n合成中文译文。🙂\n";

    struct State {
        features: Vec<&'static str>,
        sent: Vec<Value>,
        /// request_id → (request, job).
        jobs: Vec<(Value, Value)>,
        dispatches: u64,
        /// Usage reported for the next dispatch.
        usage: Value,
        published: bool,
    }

    struct Agent(Arc<Mutex<State>>);

    fn meta() -> Value {
        json!({"version": "v".repeat(64), "item_id": "item_1", "revision_id": "revision_1", "original_chapter": "appendix", "source_sha256": "s".repeat(64),
            "target_language": "zh-CN", "sha256": sha256_hex(AI), "bytes": AI.len(), "provenance": "machine_translation",
            "route": {"provider_id": "synthetic", "model_id": "synthetic-model", "reasoning_effort": "low"}})
    }

    impl Backend for Agent {
        fn send(&self, c: &Command) -> Result<Response, ClientError> {
            let mut s = self.0.lock().unwrap();
            let Command::Reader { request, .. } = c else {
                let features: Vec<Value> = s.features.iter().map(|f| json!(f)).collect();
                return Ok(serde_json::from_value(json!({"ok": true, "capabilities": {"schema_version": 1, "features": features}})).unwrap());
            };
            let r = serde_json::to_value(request).unwrap();
            s.sent.push(r.clone());
            let reply = match r["op"].as_str().unwrap() {
                "translate_preview" if r["chapter"] == "chapter-1" => json!({"eligible": false, "reason": "reader_translation_not_needed", "quote": null, "translation": null}),
                "translate_preview" if s.published => json!({"eligible": true, "reason": null, "quote": null, "translation": meta()}),
                "translate_preview" => json!({"eligible": true, "reason": null, "translation": null, "quote": {
                    "quote_id": "quote_1", "expires_at": 900100, "item_id": r["item_id"], "revision_id": r["revision_id"], "chapter": r["chapter"],
                    "source_sha256": "s".repeat(64), "target_language": "zh-CN", "settings_revision": 1,
                    "route": {"provider_id": "synthetic", "model_id": "synthetic-model", "reasoning_effort": "low"},
                    "budget": {"max_requests": 1, "max_input_bytes_per_request": 32768, "max_completion_tokens_per_request": 8192, "max_total_input_bytes": 32768, "max_total_completion_tokens": 8192},
                    "source_bytes": 32, "planned_requests": 1, "estimated_cost": null}}),
                "translate_request" => {
                    let mut req = r.clone();
                    req.as_object_mut().unwrap().remove("op");
                    if let Some((old, job)) = s.jobs.iter().find(|(old, _)| old["request_id"] == req["request_id"]) {
                        if *old != req { return Err(ClientError::Daemon("request_identity_conflict".into())); }
                        job.clone()
                    } else {
                        // One synthetic dispatch, published at once.
                        s.dispatches += 1;
                        s.published = true;
                        let mut job = req.clone();
                        for (k, v) in [("job_id", json!(format!("job_{}", s.jobs.len() + 1))), ("operation", json!("chapter_translation")), ("state", json!("succeeded")),
                            ("stage", json!("done")), ("result_revision_id", json!("revision_1")), ("alignment_version", json!(null)), ("alignment_status", json!(null)),
                            ("error_code", json!(null)), ("failure_stage", json!(null)), ("failure_reason", json!(null)),
                            ("budget", json!({"max_requests": 1, "max_input_bytes_per_request": 32768, "max_completion_tokens_per_request": 8192, "max_total_input_bytes": 32768, "max_total_completion_tokens": 8192})),
                            ("usage", s.usage.clone()), ("elapsed_ms", json!(500)), ("completed_blocks", json!(2)), ("total_blocks", json!(2)), ("translation", meta())] {
                            job[k] = v;
                        }
                        s.jobs.push((req, job.clone()));
                        job
                    }
                }
                "translate_status" => s.jobs.iter().find(|(_, j)| j["job_id"] == r["job_id"]).map(|(_, j)| j.clone()).ok_or(ClientError::Daemon("reader_job_not_found".into()))?,
                "translate_chunk" => {
                    if r["expected_version"] != meta()["version"] { return Err(ClientError::Daemon("stale_reader_translation".into())); }
                    let offset = r["offset"].as_u64().unwrap() as usize;
                    // Small pages so the client follows next_offset across a CJK/emoji text.
                    let mut end = (offset + 9).min(AI.len());
                    while !AI.is_char_boundary(end) { end -= 1; }
                    let mut m = meta();
                    for (k, v) in [("start", json!(offset)), ("end", json!(end)), ("markdown", json!(&AI[offset..end])), ("next_offset", json!((end < AI.len()).then_some(end)))] { m[k] = v; }
                    m
                }
                op => panic!("unexpected {op}"),
            };
            Ok(Response { ok: true, reader: Some(if r["op"] == "translate_request" || r["op"] == "translate_status" { json!({"job": reply}) } else { reply }), ..Default::default() })
        }
        fn mode(&self) -> BackendMode { BackendMode::Agent { socket: "/nonexistent".into() } }
    }

    fn start(features: Vec<&'static str>) -> (Client, Arc<Mutex<State>>) {
        let usage = json!({"requests_dispatched": 1, "reported_total_tokens": 17, "unknown_usage_requests": 0,
            "dispatches": [{"input_tokens": 11, "output_tokens": 3, "total_tokens": 17}], "input_tokens": 11, "output_tokens": 3});
        let s = Arc::new(Mutex::new(State { features, sent: vec![], jobs: vec![], dispatches: 0, usage, published: false }));
        (Client::new(Agent(s.clone())), s)
    }

    fn doc(original_language: &str) -> ReaderDocument {
        serde_json::from_value(json!({"item_id": "item_1", "revision_id": "revision_1", "default_side": "translation", "alignment": "chapter_order", "editions": [
            {"side": "original", "language": original_language, "sha256": "o".repeat(64), "bytes": 60, "chapters": [
                {"key": "chapter-1", "title": "One", "level": 1, "start": 0, "end": 30, "counterpart": "chapter-1"},
                {"key": "appendix", "title": "Appendix", "level": 1, "start": 30, "end": 60, "counterpart": null}]},
            {"side": "translation", "language": "zh", "sha256": "t".repeat(64), "bytes": 30, "chapters": [
                {"key": "chapter-1", "title": "一", "level": 1, "start": 0, "end": 30, "counterpart": "chapter-1"}]}]})).unwrap()
    }

    #[test]
    fn only_english_chapters_without_a_counterpart_are_eligible() {
        let d = doc("en");
        assert!(d.translation_eligible("appendix"));
        assert!(!d.translation_eligible("chapter-1"), "a paired chapter is never offered");
        assert!(!d.translation_eligible("missing"));
        assert!(doc("en-GB").translation_eligible("appendix"));
        assert!(!doc("zh").translation_eligible("appendix"));
        assert!(!doc("eng").translation_eligible("appendix"));
    }

    #[test]
    fn exact_request_shapes_and_a_published_read() {
        let (client, s) = start(vec![crate::book::READER_FEATURE, TRANSLATION_FEATURE]);
        let d = doc("en");
        let preview = client.reader_translate_preview(&d, "appendix").unwrap();
        let quote = preview.quote.unwrap();
        assert_eq!((quote.route.model_id.as_str(), quote.route.reasoning_effort.as_deref(), quote.planned_requests, quote.budget.max_requests), ("synthetic-model", Some("low"), 1, 1));
        let job = client.reader_translate_request(&d, "appendix", "mac-tr-1", &quote.quote_id).unwrap();
        assert_eq!((job.state, job.completed_blocks, job.total_blocks), (JobState::Succeeded, 2, 2));
        let usage = job.usage.clone().unwrap();
        assert_eq!((usage.input_tokens, usage.output_tokens, usage.known_total()), (Some(11), Some(3), Some(17)));
        let again = client.reader_translate_status("item_1", &job.job_id).unwrap();
        assert_eq!(again, job);
        let text = client.reader_translation_text(job.translation.as_ref().unwrap()).unwrap();
        assert_eq!(text, AI);
        let sent = s.lock().unwrap().sent.clone();
        assert_eq!(sent[0], json!({"op": "translate_preview", "item_id": "item_1", "revision_id": "revision_1", "chapter": "appendix"}));
        assert_eq!(sent[1], json!({"op": "translate_request", "item_id": "item_1", "revision_id": "revision_1", "chapter": "appendix",
            "request_id": "mac-tr-1", "quote_id": "quote_1", "accept_cost": true, "expected_translation_version": "missing"}));
        assert_eq!(sent[2], json!({"op": "translate_status", "item_id": "item_1", "job_id": job.job_id}));
        assert_eq!(sent[3], json!({"op": "translate_chunk", "item_id": "item_1", "revision_id": "revision_1", "chapter": "appendix",
            "expected_version": "v".repeat(64), "offset": 0, "limit": crate::book::CHUNK_LIMIT}));
        assert!(sent[4..].iter().all(|r| r["op"] == "translate_chunk"), "the text is read across pages");
        // Reads dispatched nothing; an existing result previews without a quote.
        assert_eq!(s.lock().unwrap().dispatches, 1);
        let cached = client.reader_translate_preview(&d, "appendix").unwrap();
        assert!(cached.quote.is_none() && cached.translation.is_some());
        let paired = client.reader_translate_preview(&d, "chapter-1").unwrap();
        assert_eq!((paired.eligible, paired.reason.as_deref(), paired.quote.is_none()), (false, Some("reader_translation_not_needed"), true));
    }

    #[test]
    fn replaying_the_same_request_returns_the_original_job_without_a_second_dispatch() {
        let (client, s) = start(vec![crate::book::READER_FEATURE, TRANSLATION_FEATURE]);
        let d = doc("en");
        let first = client.reader_translate_request(&d, "appendix", "mac-tr-1", "quote_1").unwrap();
        let replay = client.reader_translate_request(&d, "appendix", "mac-tr-1", "quote_1").unwrap();
        assert_eq!(replay, first);
        assert_eq!(s.lock().unwrap().dispatches, 1);
        // The same id with another payload is refused, not a new job.
        assert_eq!(client.reader_translate_request(&d, "appendix", "mac-tr-1", "quote_2"), Err(ClientError::Daemon("request_identity_conflict".into())));
        assert_eq!(s.lock().unwrap().dispatches, 1);
    }

    #[test]
    fn unknown_usage_splits_stay_null_and_are_never_derived() {
        let (client, s) = start(vec![crate::book::READER_FEATURE, TRANSLATION_FEATURE]);
        s.lock().unwrap().usage = json!({"requests_dispatched": 2, "reported_total_tokens": 50, "unknown_usage_requests": 0,
            "dispatches": [{"input_tokens": null, "output_tokens": 7, "total_tokens": 37}, {"input_tokens": 11, "output_tokens": 2, "total_tokens": 13}],
            "input_tokens": null, "output_tokens": 9});
        let job = client.reader_translate_request(&doc("en"), "appendix", "mac-tr-1", "quote_1").unwrap();
        let u = job.usage.unwrap();
        assert_eq!((u.input_tokens, u.output_tokens, u.known_total()), (None, Some(9), Some(50)));
        assert_eq!(u.dispatches.unwrap()[0].input_tokens, None);
        // Older receipts omit the new keys: they read as null, not zero.
        s.lock().unwrap().usage = json!({"requests_dispatched": 1, "reported_total_tokens": 0, "unknown_usage_requests": 1});
        let job = client.reader_translate_request(&doc("en"), "appendix", "mac-tr-2", "quote_1").unwrap();
        let u = job.usage.unwrap();
        assert_eq!((u.known_total(), u.input_tokens, u.output_tokens, u.dispatches), (None, None, None, None));
    }

    #[test]
    fn without_the_capability_nothing_is_sent_and_bad_replies_are_refused() {
        let (client, s) = start(vec![crate::book::READER_FEATURE]);
        assert!(!client.reader_translation_supported());
        assert_eq!(client.reader_translate_preview(&doc("en"), "appendix"), Err(ClientError::Unsupported));
        assert_eq!(client.reader_translate_request(&doc("en"), "appendix", "mac-tr-1", "quote_1"), Err(ClientError::Unsupported));
        assert!(s.lock().unwrap().sent.is_empty());
        // A text whose hash does not match its metadata is never shown.
        let (client, _) = start(vec![crate::book::READER_FEATURE, TRANSLATION_FEATURE]);
        let mut m: crate::book::TranslationMeta = serde_json::from_value(meta()).unwrap();
        m.sha256 = "0".repeat(64);
        assert_eq!(client.reader_translation_text(&m), Err(ClientError::InvalidFrame));
    }
}

/// `reader_revision_text_v1` against a stand-in that serves `revision_chunk` as
/// the contract freezes it: byte offsets, `expected_sha256` "any" then the hash.
mod revision_text {
    use super::*;
    use crate::book::{sha256_hex, RevisionTarget, REVISION_CHUNK_BYTES, REVISION_TEXT_FEATURE};
    use serde_json::{json, Value};
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct State {
        features: Vec<&'static str>,
        texts: std::collections::HashMap<String, String>,
        sent: Vec<Value>,
        /// Corrupt the reply to the chunk with this index.
        tamper: Option<(usize, &'static str)>,
        /// Fail the chunk with this index with this code.
        fail: Option<(usize, &'static str)>,
    }

    struct Agent(Arc<Mutex<State>>);

    impl Backend for Agent {
        fn send(&self, c: &Command) -> Result<Response, ClientError> {
            let mut s = self.0.lock().unwrap();
            let Command::Reader { request, .. } = c else {
                let features: Vec<Value> = s.features.iter().map(|f| json!(f)).collect();
                return Ok(serde_json::from_value(json!({"ok": true, "capabilities": {"schema_version": 1, "features": features}})).unwrap());
            };
            let r = serde_json::to_value(request).unwrap();
            let index = s.sent.len();
            s.sent.push(r.clone());
            assert_eq!(r["op"], "revision_chunk");
            if let Some((i, code)) = s.fail { if i == index { return Err(ClientError::Daemon(code.into())); } }
            let Some(text) = s.texts.get(r["revision_id"].as_str().unwrap()).cloned() else { return Err(ClientError::Daemon("revision_text_unavailable".into())) };
            let sha = sha256_hex(&text);
            let expected = r["expected_sha256"].as_str().unwrap();
            let start = r["start_byte"].as_u64().unwrap() as usize;
            assert!(expected == sha || (expected == "any" && start == 0), "later chunks are bound to the hash");
            let limit = r["limit_bytes"].as_u64().unwrap() as usize;
            assert!((1..=REVISION_CHUNK_BYTES).contains(&limit));
            let mut end = (start + limit).min(text.len());
            while !text.is_char_boundary(end) { end -= 1; }
            let mut reply = json!({"item_id": r["item_id"], "revision_id": r["revision_id"], "target": r["target"], "sha256": sha, "total_bytes": text.len(),
                "start_byte": start, "end_byte": end, "next_offset": if end < text.len() { json!(end) } else { Value::Null }, "text": &text[start..end]});
            if let Some((i, field)) = s.tamper {
                if i == index {
                    reply[field] = match field {
                        // Same length, other bytes: only the whole-text hash can tell.
                        "text" => { let t = reply["text"].as_str().unwrap(); let c = t.chars().next().unwrap(); json!(format!("戊{}", &t[c.len_utf8()..])) }
                        "next_offset" => Value::Null,
                        "sha256" => json!("0".repeat(64)),
                        "target" => json!({"kind": "body"}),
                        _ => json!(reply[field].as_u64().map_or(json!("other"), |n| json!(n + 1))),
                    };
                }
            }
            Ok(Response { ok: true, reader: Some(reply), ..Default::default() })
        }
        fn mode(&self) -> BackendMode {
            BackendMode::Agent { socket: "/nonexistent".into() }
        }
    }

    fn start(texts: &[(&str, &str)]) -> (Client, Arc<Mutex<State>>) {
        let s = Arc::new(Mutex::new(State { features: vec![crate::book::READER_FEATURE, REVISION_TEXT_FEATURE], texts: texts.iter().map(|(r, t)| (r.to_string(), t.to_string())).collect(), ..Default::default() }));
        (Client::new(Agent(s.clone())), s)
    }

    fn chapter() -> RevisionTarget {
        RevisionTarget::Chapter { side: "translation".into(), chapter: "chapter-1".into() }
    }

    #[test]
    fn a_multi_chunk_cjk_text_is_read_whole_with_exact_requests() {
        let text = format!("# 晨光\n\n{}", "合成读者沿着晨光前行。🙂\n".repeat(900));
        assert!(text.len() > 3 * REVISION_CHUNK_BYTES);
        let (client, s) = start(&[("revision_a", &text)]);
        let seen = Mutex::new(vec![]);
        let got = client.reader_revision_text("item_1", "revision_a", &chapter(), &|r, t| seen.lock().unwrap().push((r, t))).unwrap();
        assert_eq!((got.text.as_str(), got.sha256.as_str(), got.revision_id.as_str()), (text.as_str(), sha256_hex(&text).as_str(), "revision_a"));
        let sent = s.lock().unwrap().sent.clone();
        assert_eq!(sent[0], json!({"op": "revision_chunk", "item_id": "item_1", "revision_id": "revision_a",
            "target": {"kind": "chapter", "side": "translation", "chapter": "chapter-1"}, "start_byte": 0, "limit_bytes": 8192, "expected_sha256": "any"}));
        assert!(sent.len() >= 4);
        for w in sent.windows(2) { assert!(w[1]["start_byte"].as_u64() > w[0]["start_byte"].as_u64()); }
        assert!(sent[1..].iter().all(|r| r["expected_sha256"] == json!(sha256_hex(&text))));
        let seen = seen.into_inner().unwrap();
        assert_eq!(seen.last(), Some(&(text.len(), text.len())));
        // A body target and an empty text.
        let (client, s) = start(&[("revision_b", "")]);
        let got = client.reader_revision_text("item_1", "revision_b", &RevisionTarget::Body {}, &|_, _| {}).unwrap();
        assert_eq!(got.text, "");
        assert_eq!(s.lock().unwrap().sent[0]["target"], json!({"kind": "body"}));
    }

    #[test]
    fn anything_short_of_the_complete_verified_text_is_an_error_with_progress() {
        let text = "甲乙丙丁".repeat(2000);
        for (index, field) in [(1, "text"), (1, "next_offset"), (2, "sha256"), (1, "start_byte"), (0, "target"), (1, "revision_id"), (2, "total_bytes"), (1, "end_byte")] {
            let (client, s) = start(&[("revision_a", &text)]);
            s.lock().unwrap().tamper = Some((index, field));
            let e = client.reader_revision_text("item_1", "revision_a", &chapter(), &|_, _| {}).unwrap_err();
            assert_eq!(e.error, ClientError::InvalidFrame, "{index}.{field}");
            assert!(e.read_bytes <= text.len(), "{index}.{field}");
            if index > 0 { assert_eq!(e.total_bytes, Some(text.len()), "{index}.{field}"); }
        }
        // A daemon error mid-way reports how far the read got.
        let (client, s) = start(&[("revision_a", &text)]);
        s.lock().unwrap().fail = Some((2, "stale_item_revision"));
        let e = client.reader_revision_text("item_1", "revision_a", &chapter(), &|_, _| {}).unwrap_err();
        assert_eq!((e.error, e.total_bytes), (ClientError::Daemon("stale_item_revision".into()), Some(text.len())));
        assert!(e.read_bytes > REVISION_CHUNK_BYTES);
        // An unknown revision, and no capability: nothing read, nothing sent.
        let (client, _) = start(&[]);
        assert_eq!(client.reader_revision_text("item_1", "revision_x", &chapter(), &|_, _| {}).unwrap_err().error, ClientError::Daemon("revision_text_unavailable".into()));
        let (client, s) = start(&[("revision_a", &text)]);
        s.lock().unwrap().features = vec![crate::book::READER_FEATURE];
        assert!(!client.reader_revision_text_supported());
        assert_eq!(client.reader_revision_text("item_1", "revision_a", &chapter(), &|_, _| {}).unwrap_err(), crate::book::RevisionReadError { error: ClientError::Unsupported, read_bytes: 0, total_bytes: None });
        assert!(s.lock().unwrap().sent.is_empty());
    }
}

/// Stand-in agent for `library_loading`: answers every frame with `reply(command)`
/// and records each command. Runs until the test process ends.
fn serve_script(path: &Path, reply: impl Fn(&serde_json::Value, usize) -> String + Send + 'static) -> std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>> {
    let listener = UnixListener::bind(path).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = seen.clone();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let mut header = [0u8; 4];
            if stream.read_exact(&mut header).is_err() { continue; }
            let mut body = vec![0u8; u32::from_be_bytes(header) as usize];
            stream.read_exact(&mut body).unwrap();
            let command: serde_json::Value = serde_json::from_slice(&body).unwrap();
            let n = { let mut l = log.lock().unwrap(); l.push(command.clone()); l.iter().filter(|c| op(c) == op(&command)).count() };
            let _ = stream.write_all(&Framing::encode(reply(&command, n).as_bytes()));
        }
    });
    seen
}

/// `command`, or `reader:<op>` for a reader request.
fn op(c: &serde_json::Value) -> String {
    match c["command"].as_str() {
        Some("reader") => format!("reader:{}", c["request"]["op"].as_str().unwrap_or("")),
        other => other.unwrap_or("").to_string(),
    }
}

const BUSY: &str = r#"{"ok":false,"reason_code":"library_loading"}"#;
const H64: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn busy_agent(dir: &tempfile::TempDir) -> (AgentBackend, std::path::PathBuf) {
    let path = dir.path().join("agent.sock");
    let pauses = vec![Duration::from_millis(1); BUSY_BACKOFF_MS.len()];
    (AgentBackend::new(path.clone()).with_timeout(Duration::from_millis(500)).with_busy_backoff(pauses), path)
}

fn caps_reply() -> String {
    serde_json::json!({"ok": true, "capabilities": {"schema_version": 1, "features": [
        crate::book::READER_FEATURE, crate::book::TRANSLATION_FEATURE, crate::book::REVISION_TEXT_FEATURE,
        crate::book::REPAIR_FEATURE, crate::book::EDIT_FEATURE]}}).to_string()
}

fn busy_doc() -> crate::book::ReaderDocument {
    serde_json::from_value(serde_json::json!({"item_id": "item_a", "revision_id": "rev_1", "editions": [
        {"side": "original", "language": "en", "sha256": H64, "bytes": 3, "chapters": [{"key": "c1", "title": "One", "start": 0, "end": 3}]}]})).unwrap()
}

fn translation_meta() -> serde_json::Value {
    serde_json::json!({"version": H64, "item_id": "item_a", "revision_id": "rev_1", "original_chapter": "c1", "source_sha256": H64,
        "target_language": "zh-CN", "sha256": crate::book::sha256_hex("abc"), "bytes": 3, "provenance": "machine_translation",
        "route": {"provider_id": "p", "model_id": "m"}})
}

#[test]
fn busy_reads_recover_with_the_same_revision_hash_and_version() {
    let dir = tempfile::tempdir().unwrap();
    let (backend, path) = busy_agent(&dir);
    let busy_first = 2;
    let seen = serve_script(&path, move |c, n| {
        let reply = |reader: serde_json::Value| serde_json::json!({"ok": true, "reader": reader}).to_string();
        match op(c).as_str() {
            "capabilities" => caps_reply(),
            _ if n <= busy_first => BUSY.into(),
            "reader:chunk" => reply(serde_json::json!({"item_id": "item_a", "revision_id": "rev_1", "side": "original", "sha256": H64, "chapter": "c1", "start": 0, "end": 3, "next_offset": null, "markdown": "abc"})),
            "reader:translate_chunk" => { let mut m = translation_meta(); m["start"] = 0.into(); m["end"] = 3.into(); m["markdown"] = "abc".into(); m["next_offset"] = serde_json::Value::Null; reply(m) }
            "reader:revision_chunk" => reply(serde_json::json!({"item_id": "item_a", "revision_id": "rev_0", "target": {"kind": "body"}, "sha256": crate::book::sha256_hex("abc"),
                "total_bytes": 3, "start_byte": 0, "end_byte": 3, "next_offset": null, "text": "abc"})),
            other => panic!("unexpected {other}"),
        }
    });
    let client = Client::new(backend);
    assert_eq!(client.reader_chapter(&busy_doc(), "original", "c1").unwrap(), "abc");
    let meta: crate::book::TranslationMeta = serde_json::from_value(translation_meta()).unwrap();
    assert_eq!(client.reader_translation_text(&meta).unwrap(), "abc");
    let text = client.reader_revision_text("item_a", "rev_0", &crate::book::RevisionTarget::Body {}, &|_, _| {}).unwrap();
    assert_eq!((text.text.as_str(), text.revision_id.as_str()), ("abc", "rev_0"));
    let seen = seen.lock().unwrap();
    for name in ["reader:chunk", "reader:translate_chunk", "reader:revision_chunk"] {
        let sent: Vec<_> = seen.iter().filter(|c| op(c) == name).collect();
        assert_eq!(sent.len(), busy_first + 1, "{name}: two busy replies, then the answer");
        assert!(sent.iter().all(|c| *c == sent[0]), "{name}: every attempt repeats the same revision / hash / version");
    }
}

#[test]
fn busy_reads_give_up_after_the_bounded_retries_as_busy_not_corrupt() {
    let dir = tempfile::tempdir().unwrap();
    let (backend, path) = busy_agent(&dir);
    let seen = serve_script(&path, |c, _| if op(c) == "capabilities" { caps_reply() } else { BUSY.into() });
    let client = Client::new(backend);
    let error = client.reader_chapter(&busy_doc(), "original", "c1").unwrap_err();
    assert!(error.is_busy() && !error.is_stale_revision() && error != ClientError::InvalidFrame, "{error:?}");
    let meta: crate::book::TranslationMeta = serde_json::from_value(translation_meta()).unwrap();
    assert!(client.reader_translation_text(&meta).unwrap_err().is_busy());
    let revision = client.reader_revision_text("item_a", "rev_0", &crate::book::RevisionTarget::Body {}, &|_, _| {}).unwrap_err();
    assert!(revision.error.is_busy() && revision.read_bytes == 0);
    assert!(client.library_query(&QuerySpec::default(), Sensitivity::Public, 0, 10, None).unwrap_err().is_busy());
    crate::i18n::set_language(crate::i18n::Lang::Zh);
    assert_eq!(ClientError::Daemon("library_loading".into()).message(), "Mac 正忙，稍后重试");
    let seen = seen.lock().unwrap();
    for name in ["reader:chunk", "reader:translate_chunk", "reader:revision_chunk", "library_query"] {
        assert_eq!(seen.iter().filter(|c| op(c) == name).count(), 1 + BUSY_BACKOFF_MS.len(), "{name}: bounded");
    }
}

#[test]
fn busy_writes_and_model_requests_are_sent_once_and_keep_their_identity() {
    let dir = tempfile::tempdir().unwrap();
    let (backend, path) = busy_agent(&dir);
    let seen = serve_script(&path, |c, _| match op(c).as_str() {
        "capabilities" => caps_reply(),
        "reader:edit_begin" => serde_json::json!({"ok": true, "reader": {"edit_id": "edit_1", "received_bytes": 0, "max_chunk_bytes": 8192, "state": "uploading", "receipt": null}}).to_string(),
        "reader:edit_chunk" => serde_json::json!({"ok": true, "reader": {"edit_id": "edit_1", "received_bytes": 3}}).to_string(),
        _ => BUSY.into(),
    });
    let client = Client::new(backend);
    let doc = busy_doc();
    let align = client.reader_align_request(&doc, "c1", "mac-repair-1", "missing").unwrap_err();
    let translate = client.reader_translate_request(&doc, "c1", "mac-tr-1", "quote_1").unwrap_err();
    let edit = crate::book::ChapterEdit { item_id: "item_a".into(), revision_id: "rev_1".into(), side: "original".into(), chapter: "c1".into(),
        expected_chapter_sha256: H64.into(), request_id: "mac-edit-1".into(), markdown: "abd".into() };
    let commit = client.reader_edit_chapter(&edit).unwrap_err();
    let metadata = client.update_metadata("item_a", "rev_1", "t", &[]).unwrap_err();
    for e in [&align, &translate, &commit, &metadata] {
        // Reported as is; callers treat it as an unknown outcome and keep the request id.
        assert!(e.is_busy() && e.is_transport(), "{e:?}");
    }
    let seen = seen.lock().unwrap();
    for name in ["reader:align_request", "reader:translate_request", "reader:edit_commit", "update_item_metadata"] {
        assert_eq!(seen.iter().filter(|c| op(c) == name).count(), 1, "{name} must not be retried automatically: {:?}", seen.iter().map(op).collect::<Vec<_>>());
    }
}

/// `reader_content_format_v1` against a stand-in that answers with the frozen
/// shapes: exact request params, reply checks, permission refusals, no retries.
mod content_format {
    use super::*;
    use crate::book::RevisionTarget;
    use crate::format::*;
    use serde_json::{json, Value};
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct State {
        features: Vec<&'static str>,
        sent: Vec<Value>,
        /// Reply to the op with this name (overrides the default).
        replies: std::collections::HashMap<&'static str, Result<Value, ClientError>>,
        /// Previews answer `preparing` this many times first.
        preparing: usize,
        selection: Option<Value>,
    }

    struct Agent(Arc<Mutex<State>>);

    fn quote(selection: &Value) -> Value {
        json!({"quote_id": "quote_1", "expires_at": 1, "selection": selection, "policy_version": "content-format-v1",
            "route": {"provider_id": "synthetic", "model_id": "synthetic", "reasoning_effort": null},
            "counts": {"items": 1, "targets": 1, "eligible": 1, "skipped": 0, "private_targets": 0, "skipped_by_reason": {}},
            "budget": {"max_requests": 1, "max_input_bytes": 8192, "max_completion_tokens": 1024}, "estimated_cost": null})
    }

    fn batch(request_id: &str, state: &str) -> Value {
        json!({"batch": {"batch_id": "batch_1", "request_id": request_id, "state": state, "error_code": null,
            "counts": {"total": 1, "queued": 0, "running": 0, "succeeded": 1, "failed": 0, "outcome_unknown": 0, "skipped": 0},
            "usage": {"requests_dispatched": 1, "reported_total_tokens": 9, "unknown_usage_requests": 0, "input_tokens": 5, "output_tokens": 4}}})
    }

    fn result_row() -> Value {
        json!({"target_id": "target_1", "item_id": "item_1", "revision_id": "rev_1", "target": {"kind": "body"}, "state": "succeeded", "reason": null,
            "job": {"job_id": "job_1", "request_id": "r1", "operation": "content_format", "item_id": "item_1", "revision_id": "rev_1", "target": {"kind": "body"},
                "state": "succeeded", "stage": "done", "result_revision_id": "rev_2", "error_code": null, "failure_stage": null, "failure_reason": null,
                "budget": {"max_requests": 1, "max_input_bytes_per_request": 8192, "max_completion_tokens_per_request": 1024, "max_total_input_bytes": 8192, "max_total_completion_tokens": 1024},
                "usage": {"requests_dispatched": 1, "reported_total_tokens": 9, "unknown_usage_requests": 0, "dispatches": null, "input_tokens": 5, "output_tokens": 4}, "elapsed_ms": 3},
            "formatting": {"status": "complete", "changed": true, "applied_count": 2, "correction_count": 0, "deleted_blocks": 1, "unresolved_count": 0,
                "checked_bytes": 70, "total_bytes": 70, "changes_version": "v1", "quality": "not_certified"}})
    }

    impl Backend for Agent {
        fn send(&self, c: &Command) -> Result<Response, ClientError> {
            let mut s = self.0.lock().unwrap();
            let Command::Reader { request, .. } = c else {
                let features: Vec<Value> = s.features.iter().map(|f| json!(f)).collect();
                return Ok(serde_json::from_value(json!({"ok": true, "capabilities": {"schema_version": 1, "features": features}})).unwrap());
            };
            let r = serde_json::to_value(request).unwrap();
            s.sent.push(r.clone());
            let op = r["op"].as_str().unwrap().to_string();
            if let Some(sel) = r.get("selection") { s.selection = Some(sel.clone()); }
            if let Some(reply) = s.replies.get(op.as_str()) { return reply.clone().map(|v| Response { ok: true, reader: Some(v), ..Default::default() }); }
            let reply = match op.as_str() {
                "format_preview" => {
                    let id = r.get("preview_id").cloned().unwrap_or(json!("preview_1"));
                    if s.preparing > 0 { s.preparing -= 1; json!({"preview_id": id, "state": "preparing", "quote": null, "error_code": null}) }
                    else {
                        let sel = r.get("selection").cloned().or(s.selection.clone()).unwrap();
                        json!({"preview_id": id, "state": "ready", "quote": quote(&sel), "error_code": null})
                    }
                }
                "format_targets" => json!({"preview_id": r["preview_id"], "total": 1, "offset": 0, "next_offset": null, "targets": [{"target_id": "target_1", "item_id": "item_1",
                    "revision_id": "rev_1", "target": {"kind": "body"}, "source_sha256": null, "source_bytes": null, "state": "skipped", "reason": "already_formatted", "budget": null}]}),
                "format_request" => batch(r["request_id"].as_str().unwrap(), "queued"),
                "format_status" | "format_resume" => batch("r1", "done"),
                "format_results" => json!({"batch_id": "batch_1", "total": 1, "offset": 0, "next_offset": null, "results": [result_row()]}),
                "format_changes" if r["view"] == "list" => json!({"version": "v1", "item_id": "item_1", "input_revision_id": "rev_1", "result_revision_id": "rev_2",
                    "target": {"kind": "body"}, "source_sha256": "a", "result_sha256": "b", "total": 1, "offset": 0, "next_offset": null,
                    "changes": [{"id": "c1", "kind": "garbage_delete", "basis": "source_verified", "reason_code": "navigation", "old_range": [0, 6], "new_range": [0, 0],
                        "old_sha256": "x", "new_sha256": "y", "before_bytes": 6, "after_bytes": 0, "source_evidence_ids": []}]}),
                "format_changes" => {
                    let text = "合成🌹正文".repeat(1000);
                    let start = r["start_byte"].as_u64().unwrap() as usize;
                    let mut end = (start + r["limit_bytes"].as_u64().unwrap() as usize).min(text.len());
                    while !text.is_char_boundary(end) { end -= 1; }
                    json!({"version": "v1", "change_id": r["change_id"], "part": r["part"], "total_bytes": text.len(), "start_byte": start, "end_byte": end,
                        "next_offset": if end < text.len() { json!(end) } else { Value::Null }, "text": &text[start..end]})
                }
                "format_restore" => json!({"item_id": "item_1", "request_id": r["request_id"], "revision_id": "rev_3", "changed": true}),
                other => panic!("unexpected {other}"),
            };
            Ok(Response { ok: true, reader: Some(reply), ..Default::default() })
        }
        fn mode(&self) -> BackendMode {
            BackendMode::Agent { socket: "/nonexistent".into() }
        }
    }

    fn start() -> (Client, Arc<Mutex<State>>) {
        let s = Arc::new(Mutex::new(State { features: vec![crate::book::READER_FEATURE, FORMAT_FEATURE, FORMAT_BATCH_FEATURE], ..Default::default() }));
        (Client::new(Agent(s.clone())), s)
    }

    fn body() -> FormatSelection {
        FormatSelection::Item { item_id: "item_1".into(), revision_id: "rev_1".into(), target: RevisionTarget::Body {} }
    }

    fn handle() -> TargetHandle {
        TargetHandle { batch_id: "batch_1".into(), target_id: "target_1".into(), item_id: "item_1".into(), target: RevisionTarget::Body {},
            input_revision_id: "rev_1".into(), result_revision_id: "rev_2".into(), changes_version: "v1".into(), changed: true }
    }

    #[test]
    fn exact_request_shapes_of_every_operation() {
        let (client, s) = start();
        let chapter = FormatSelection::Item { item_id: "item_1".into(), revision_id: "rev_1".into(), target: RevisionTarget::Chapter { side: "translation".into(), chapter: "chapter-1".into() } };
        s.lock().unwrap().preparing = 1;
        let p = client.format_preview(&chapter, std::time::Duration::from_millis(1)).unwrap();
        assert_eq!(p.state, "ready");
        client.format_preview_start(&FormatSelection::Group { group_id: "group_1".into(), include_descendants: true }).unwrap();
        assert_eq!(client.format_targets("preview_1", 0, 100).unwrap().targets[0].reason.as_deref(), Some("already_formatted"));
        let b = client.format_request("quote_1", "mac-fmt-1").unwrap();
        client.format_status(&b.batch_id).unwrap();
        let page = client.format_results("batch_1", 0, RESULTS_PAGE).unwrap();
        let h = page.results[0].handle("batch_1").unwrap();
        assert_eq!(h, handle());
        client.format_changes(&h, 0).unwrap();
        client.format_change_text(&h, "c1", "before", 300).unwrap();
        client.format_restore(&h, "mac-fmt-restore-1").unwrap();
        client.format_resume("batch_1").unwrap();
        let sent = s.lock().unwrap().sent.clone();
        assert_eq!(sent[0], json!({"op": "format_preview", "selection": {"kind": "item", "item_id": "item_1", "revision_id": "rev_1", "target": {"kind": "chapter", "side": "translation", "chapter": "chapter-1"}}}));
        assert_eq!(sent[1], json!({"op": "format_preview", "preview_id": "preview_1"}));
        assert_eq!(sent[2], json!({"op": "format_preview", "selection": {"kind": "group", "group_id": "group_1", "include_descendants": true}}));
        assert_eq!(sent[3], json!({"op": "format_targets", "preview_id": "preview_1", "offset": 0, "limit": 100}));
        assert_eq!(sent[4], json!({"op": "format_request", "quote_id": "quote_1", "request_id": "mac-fmt-1", "accept_cost": true, "accept_model_processing": true}));
        assert_eq!(sent[5], json!({"op": "format_status", "batch_id": "batch_1"}));
        assert_eq!(sent[6], json!({"op": "format_results", "batch_id": "batch_1", "offset": 0, "limit": 20}));
        assert_eq!(sent[7], json!({"op": "format_changes", "view": "list", "batch_id": "batch_1", "target_id": "target_1", "expected_version": "v1", "offset": 0, "limit": 64}));
        assert_eq!(sent[8], json!({"op": "format_changes", "view": "text", "batch_id": "batch_1", "target_id": "target_1", "expected_version": "v1", "change_id": "c1", "part": "before", "start_byte": 0, "limit_bytes": 300}));
        assert_eq!(sent[9], json!({"op": "format_restore", "batch_id": "batch_1", "target_id": "target_1", "expected_revision_id": "rev_2", "request_id": "mac-fmt-restore-1"}));
        assert_eq!(sent[10], json!({"op": "format_resume", "batch_id": "batch_1"}));
        // The whole envelope, as the agent reads it.
        let command = Command::Reader { max_sensitivity: Sensitivity::Restricted, request: crate::book::ReaderRequest::FormatStatus { batch_id: "b".into() } };
        assert_eq!(serde_json::to_value(command).unwrap(), json!({"command": "reader", "max_sensitivity": "restricted", "request": {"op": "format_status", "batch_id": "b"}}));
    }

    #[test]
    fn replies_that_do_not_match_the_request_are_not_accepted() {
        let cases: Vec<(&str, Value, Box<dyn Fn(&Client) -> Result<(), ClientError>>)> = vec![
            // A quote for another selection.
            ("format_preview", json!({"preview_id": "p", "state": "ready", "quote": quote(&json!({"kind": "item", "item_id": "other", "revision_id": "rev_1", "target": {"kind": "body"}})), "error_code": null}),
                Box::new(|c: &Client| c.format_preview_start(&body()).map(|_| ())) ),
            ("format_preview", json!({"preview_id": "p", "state": "ready", "quote": null, "error_code": null}), Box::new(|c: &Client| c.format_preview_start(&body()).map(|_| ()))),
            ("format_request", batch("someone-else", "queued"), Box::new(|c: &Client| c.format_request("quote_1", "mac-fmt-1").map(|_| ()))),
            ("format_status", json!({"batch": {"batch_id": "batch_2", "request_id": "r", "state": "done", "counts": {}, "usage": {"requests_dispatched": 0, "reported_total_tokens": 0, "unknown_usage_requests": 0}}}),
                Box::new(|c: &Client| c.format_status("batch_1").map(|_| ()))),
            ("format_results", json!({"batch_id": "batch_1", "total": 2, "offset": 0, "next_offset": null, "results": [result_row()]}), Box::new(|c: &Client| c.format_results("batch_1", 0, 20).map(|_| ()))),
            ("format_changes", json!({"version": "v2", "item_id": "item_1", "input_revision_id": "rev_1", "result_revision_id": "rev_2", "target": {"kind": "body"}, "source_sha256": "a", "result_sha256": "b", "total": 0, "offset": 0, "next_offset": null, "changes": []}),
                Box::new(|c: &Client| c.format_changes(&handle(), 0).map(|_| ()))),
            ("format_restore", json!({"item_id": "item_1", "request_id": "mac-fmt-restore-1", "revision_id": "rev_3", "changed": false}), Box::new(|c: &Client| c.format_restore(&handle(), "mac-fmt-restore-1").map(|_| ()))),
        ];
        for (op, reply, call) in cases {
            let (client, s) = start();
            s.lock().unwrap().replies.insert(op, Ok(reply));
            assert_eq!(call(&client), Err(ClientError::InvalidFrame), "{op}");
        }
        // A no-change restore keeps the result revision.
        let (client, s) = start();
        s.lock().unwrap().replies.insert("format_restore", Ok(json!({"item_id": "item_1", "request_id": "q", "revision_id": "rev_2", "changed": false})));
        assert!(!client.format_restore(&handle(), "q").unwrap().changed);
    }

    #[test]
    fn permission_refusals_unknown_outcomes_and_no_retry() {
        // A refused whole-batch read is classified as such, not as deleted or cancelled.
        let (client, s) = start();
        for op in ["format_status", "format_results", "format_resume", "format_request"] {
            s.lock().unwrap().replies.insert(op, Err(ClientError::Daemon("library_resource_not_found".into())));
        }
        assert!(whole_batch_denied(&client.format_status("batch_1").unwrap_err()));
        assert!(whole_batch_denied(&client.format_results("batch_1", 0, 20).unwrap_err()));
        assert!(whole_batch_denied(&client.format_resume("batch_1").unwrap_err()));
        assert!(whole_batch_denied(&client.format_request("quote_1", "r1").unwrap_err()));
        // The target-addressed review and restore of a known handle still work.
        assert_eq!(client.format_changes(&handle(), 0).unwrap().changes.len(), 1);
        assert!(client.format_restore(&handle(), "r2").unwrap().changed);
        // Each call sent exactly once: nothing is retried here.
        let ops: Vec<String> = s.lock().unwrap().sent.iter().map(|r| r["op"].as_str().unwrap().to_string()).collect();
        assert_eq!(ops, ["format_status", "format_results", "format_resume", "format_request", "format_changes", "format_restore"]);
        // A lost reply is an unknown outcome and is sent once.
        let (client, s) = start();
        s.lock().unwrap().replies.insert("format_request", Err(ClientError::Timeout));
        let e = client.format_request("quote_1", "mac-fmt-1").unwrap_err();
        assert!(outcome_unknown(&e) && !whole_batch_denied(&e));
        assert!(outcome_unknown(&ClientError::Daemon("library_loading".into())));
        assert!(!outcome_unknown(&ClientError::Daemon("content_format_quote_consumed".into())));
        assert_eq!(s.lock().unwrap().sent.len(), 1);
        // Without the capability nothing is sent; a folder needs the batch capability too.
        let (client, s) = start();
        s.lock().unwrap().features = vec![crate::book::READER_FEATURE, FORMAT_FEATURE];
        assert!(client.format_supported() && !client.format_batch_supported());
        assert_eq!(client.format_preview_start(&FormatSelection::Group { group_id: "g".into(), include_descendants: false }), Err(ClientError::Unsupported));
        assert!(client.format_preview_start(&body()).is_ok());
        assert_eq!(s.lock().unwrap().sent.len(), 1);
        let (client, s) = start();
        s.lock().unwrap().features = vec![crate::book::READER_FEATURE];
        assert_eq!(client.format_request("quote_1", "r"), Err(ClientError::Unsupported));
        assert!(s.lock().unwrap().sent.is_empty());
    }

    #[test]
    fn change_text_reads_bounded_chunks_on_code_points() {
        let (client, s) = start();
        let (text, total) = client.format_change_text(&handle(), "c1", "after", 10_000).unwrap();
        assert_eq!(total, "合成🌹正文".repeat(1000).len());
        assert!(text.len() <= 10_000 && text.len() > 9_990 && "合成🌹正文".repeat(1000).starts_with(&text));
        let (whole, _) = client.format_change_text(&handle(), "c1", "after", 1 << 20).unwrap();
        assert_eq!(whole, "合成🌹正文".repeat(1000));
        let sent = s.lock().unwrap().sent.clone();
        assert!(sent.len() >= 3 && sent.iter().all(|r| r["limit_bytes"].as_u64().unwrap() <= 8192));
    }
}

/// Cross-device AI chapter translation: `translate_metadata` discovery and the
/// recorded `translate_alignment` correspondence (reads only).
mod ai_translation {
    use super::*;
    use crate::book::*;
    use serde_json::{json, Value};
    use sha2::{Digest, Sha256};
    use std::sync::{Arc, Mutex};

    const ORIGINAL: &str = "# Appendix\n\nFirst paragraph.\n\nSecond paragraph.\n";
    const AI: &str = "# 附录\n\n第一段。\n\n第二段，比原文长一些。\n";

    fn sha(s: &str) -> String { format!("{:x}", Sha256::digest(s.as_bytes())) }

    #[derive(Default)]
    struct State {
        features: Vec<&'static str>,
        sent: Vec<Value>,
        /// The reply to each op (a closure over the request).
        reply: Option<Box<dyn Fn(&Value) -> Result<Value, ClientError> + Send>>,
    }

    struct Agent(Arc<Mutex<State>>);

    impl Backend for Agent {
        fn send(&self, c: &Command) -> Result<Response, ClientError> {
            let mut s = self.0.lock().unwrap();
            let Command::Reader { request, .. } = c else {
                let features: Vec<Value> = s.features.iter().map(|f| json!(f)).collect();
                return Ok(serde_json::from_value(json!({"ok": true, "capabilities": {"schema_version": 1, "features": features}})).unwrap());
            };
            let r = serde_json::to_value(request).unwrap();
            s.sent.push(r.clone());
            (s.reply.as_ref().unwrap())(&r).map(|v| Response { ok: true, reader: Some(v), ..Default::default() })
        }
        fn mode(&self) -> BackendMode { BackendMode::Agent { socket: "/nonexistent".into() } }
    }

    fn start(features: Vec<&'static str>, reply: impl Fn(&Value) -> Result<Value, ClientError> + Send + 'static) -> (Client, Arc<Mutex<State>>) {
        let s = Arc::new(Mutex::new(State { features, reply: Some(Box::new(reply)), ..Default::default() }));
        (Client::new(Agent(s.clone())), s)
    }

    fn doc() -> ReaderDocument {
        serde_json::from_value(json!({"item_id": "item_1", "revision_id": "rev_2", "editions": []})).unwrap()
    }

    fn meta_json() -> Value {
        json!({"version": "proj_v_opaque", "item_id": "item_1", "revision_id": "rev_2", "original_chapter": "appendix", "source_sha256": sha(ORIGINAL),
            "target_language": "zh-CN", "sha256": sha(AI), "bytes": AI.len(), "provenance": "machine_translation",
            "route": {"provider_id": "synthetic", "model_id": "synthetic-model", "reasoning_effort": null}})
    }

    fn meta() -> TranslationMeta { serde_json::from_value(meta_json()).unwrap() }

    /// The rows of ORIGINAL/AI by block: heading, two paragraphs (separators to the preceding row).
    fn pairs() -> Vec<Value> {
        let (o1, o2) = (ORIGINAL.find("First").unwrap(), ORIGINAL.find("Second").unwrap());
        let (t1, t2) = (AI.find("第一段").unwrap(), AI.find("第二段").unwrap());
        vec![json!({"o": [0, o1], "t": [0, t1], "kind": "heading", "relation": "matched"}),
             json!({"o": [o1, o2], "t": [t1, t2], "kind": "paragraph", "relation": "matched"}),
             json!({"o": [o2, ORIGINAL.len()], "t": [t2, AI.len()], "kind": "paragraph", "relation": "matched"})]
    }

    fn page(r: &Value, status: &str, all: &[Value]) -> Value {
        let (offset, limit) = (r["offset"].as_u64().unwrap() as usize, r["limit"].as_u64().unwrap() as usize);
        let end = (offset + limit).min(all.len());
        json!({"item_id": "item_1", "revision_id": "rev_2", "version": "proj_v_opaque", "original_chapter": "appendix", "translation_chapter": null,
            "original_sha256": sha(ORIGINAL), "translation_sha256": sha(AI), "original_bytes": ORIGINAL.len(), "translation_bytes": AI.len(),
            "status": status, "quality": "not_certified", "basis": "translation_blocks", "offset": offset, "total": all.len(),
            "next_offset": if end < all.len() { json!(end) } else { Value::Null }, "pairs": &all[offset..end]})
    }

    #[test]
    fn discovery_is_one_exact_read_and_a_miss_is_none() {
        let hit = Arc::new(Mutex::new(false));
        let h = hit.clone();
        let (client, s) = start(vec![READER_FEATURE, TRANSLATION_FEATURE, TRANSLATION_METADATA_FEATURE], move |r| {
            assert_eq!(r["op"], "translate_metadata");
            Ok(json!({"item_id": "item_1", "revision_id": "rev_2", "chapter": "appendix", "translation": if *h.lock().unwrap() { meta_json() } else { Value::Null }}))
        });
        assert_eq!(client.reader_translate_metadata(&doc(), "appendix").unwrap(), None);
        // Published later at the same revision: the next read finds it (nothing cached).
        *hit.lock().unwrap() = true;
        assert_eq!(client.reader_translate_metadata(&doc(), "appendix").unwrap(), Some(meta()));
        let sent = s.lock().unwrap().sent.clone();
        assert_eq!(sent, vec![json!({"op": "translate_metadata", "item_id": "item_1", "revision_id": "rev_2", "chapter": "appendix"}); 2]);
        // Never a preview or request on discovery.
        assert!(sent.iter().all(|r| r["op"] == "translate_metadata"));
    }

    #[test]
    fn discovery_rejects_other_identities_and_needs_the_capability() {
        let (client, s) = start(vec![READER_FEATURE, TRANSLATION_FEATURE], |_| unreachable!());
        assert!(matches!(client.reader_translate_metadata(&doc(), "appendix"), Err(ClientError::Unsupported)));
        assert!(s.lock().unwrap().sent.is_empty(), "no read without reader_chapter_translation_metadata_v1");
        // A reply for another revision is not this chapter's answer.
        let (client, _) = start(vec![READER_FEATURE, TRANSLATION_METADATA_FEATURE], |_| Ok(json!({"item_id": "item_1", "revision_id": "rev_1", "chapter": "appendix", "translation": null})));
        assert!(matches!(client.reader_translate_metadata(&doc(), "appendix"), Err(ClientError::InvalidFrame)));
        let mut other = meta_json();
        other["original_chapter"] = json!("front-matter");
        let (client, _) = start(vec![READER_FEATURE, TRANSLATION_METADATA_FEATURE], move |_| Ok(json!({"item_id": "item_1", "revision_id": "rev_2", "chapter": "appendix", "translation": other.clone()})));
        assert!(matches!(client.reader_translate_metadata(&doc(), "appendix"), Err(ClientError::InvalidFrame)));
    }

    #[test]
    fn alignment_pages_at_one_version_and_covers_both_texts() {
        let (client, s) = start(vec![READER_FEATURE, TRANSLATION_ALIGNMENT_FEATURE], |r| Ok(page(r, "aligned", &pairs())));
        let AiAlignment::Aligned(a) = client.reader_translate_alignment(&meta(), 1).unwrap() else { panic!("aligned") };
        assert_eq!(a.pairs.len(), 3);
        a.validate(ORIGINAL, Some(AI)).unwrap();
        let sent = s.lock().unwrap().sent.clone();
        assert_eq!(sent[0], json!({"op": "translate_alignment", "item_id": "item_1", "revision_id": "rev_2", "chapter": "appendix", "expected_version": "proj_v_opaque", "offset": 0, "limit": 1}));
        assert_eq!(sent.iter().map(|r| r["offset"].as_u64().unwrap()).collect::<Vec<_>>(), vec![0, 1, 2]);
        // AI rows index the AI text: a human-translation text does not validate.
        assert!(a.validate(ORIGINAL, Some("# 人工译本\n\n不同的文字。\n")).is_err());
        // Limit is clamped to the contract's 1..=256.
        let (client, s) = start(vec![READER_FEATURE, TRANSLATION_ALIGNMENT_FEATURE], |r| Ok(page(r, "aligned", &pairs())));
        client.reader_translate_alignment(&meta(), 10_000).unwrap();
        assert_eq!(s.lock().unwrap().sent[0]["limit"], 256);
    }

    #[test]
    fn legacy_missing_and_malformed_maps() {
        let (client, _) = start(vec![READER_FEATURE, TRANSLATION_ALIGNMENT_FEATURE], |r| Ok(page(r, "missing", &[])));
        assert_eq!(client.reader_translate_alignment(&meta(), 256).unwrap(), AiAlignment::Missing);
        let tweak = |f: fn(&mut Value)| move |r: &Value| { let mut p = page(r, "aligned", &pairs()); f(&mut p); Ok(p) };
        for (why, f) in [
            ("certified", (|p: &mut Value| p["quality"] = json!("certified")) as fn(&mut Value)),
            ("human counterpart", |p| p["translation_chapter"] = json!("chapter-9")),
            ("other basis", |p| p["basis"] = json!("semantic")),
            ("unmatched row", |p| p["pairs"][0]["relation"] = json!("original_only")),
            ("other body", |p| p["translation_sha256"] = json!("0")),
            ("missing with rows", |p| p["status"] = json!("missing")),
        ] {
            let (client, _) = start(vec![READER_FEATURE, TRANSLATION_ALIGNMENT_FEATURE], tweak(f));
            assert!(matches!(client.reader_translate_alignment(&meta(), 256), Err(ClientError::InvalidFrame)), "{why}");
        }
        // A version race is the caller's to refresh (metadata, text and map together).
        let (client, _) = start(vec![READER_FEATURE, TRANSLATION_ALIGNMENT_FEATURE], |_| Err(ClientError::Daemon("stale_reader_translation".into())));
        assert!(matches!(client.reader_translate_alignment(&meta(), 256), Err(ClientError::Daemon(c)) if c == "stale_reader_translation"));
        let (client, s) = start(vec![READER_FEATURE], |_| unreachable!());
        assert!(matches!(client.reader_translate_alignment(&meta(), 256), Err(ClientError::Unsupported)));
        assert!(s.lock().unwrap().sent.is_empty());
    }
}

/// Previews may write a quote or job: a busy (or timed-out) reply never proves
/// they were not admitted, so they are sent once and the user retries.
#[test]
fn busy_previews_are_sent_exactly_once() {
    let dir = tempfile::tempdir().unwrap();
    let (backend, path) = busy_agent(&dir);
    let seen = serve_script(&path, |c, _| if op(c) == "capabilities" {
        serde_json::json!({"ok": true, "capabilities": {"schema_version": 1, "features": [
            crate::book::READER_FEATURE, crate::book::TRANSLATION_FEATURE, crate::format::FORMAT_FEATURE, crate::format::FORMAT_BATCH_FEATURE]}}).to_string()
    } else { BUSY.into() });
    let client = Client::new(backend);
    assert!(client.reader_translate_preview(&busy_doc(), "c1").unwrap_err().is_busy());
    let body = crate::format::FormatSelection::Item { item_id: "item_a".into(), revision_id: "rev_1".into(), target: crate::book::RevisionTarget::Body {} };
    assert!(client.format_preview(&body, Duration::from_millis(1)).unwrap_err().is_busy());
    let group = crate::format::FormatSelection::Group { group_id: "group_a".into(), include_descendants: true };
    assert!(client.format_preview_start(&group).unwrap_err().is_busy());
    assert!(client.format_preview_read("preview_a", &group).unwrap_err().is_busy());
    let seen = seen.lock().unwrap();
    assert_eq!(seen.iter().filter(|c| op(c) == "reader:translate_preview").count(), 1, "translate_preview: once");
    assert_eq!(seen.iter().filter(|c| op(c) == "reader:format_preview").count(), 3, "format_preview: once per call (start, start, read)");
    // Nothing else that could write a quote or job was sent meanwhile.
    assert!(seen.iter().all(|c| matches!(op(c).as_str(), "capabilities" | "reader:translate_preview" | "reader:format_preview")));
    for command in [Command::Reader { max_sensitivity: Sensitivity::Restricted, request: crate::book::ReaderRequest::TranslatePreview { item_id: "i".into(), revision_id: "r".into(), chapter: "c".into() } },
        Command::Reader { max_sensitivity: Sensitivity::Restricted, request: crate::book::ReaderRequest::FormatPreview(crate::format::FormatPreviewParams::Status { preview_id: "p".into() }) }] {
        assert!(!crate::busy_retryable(&command), "{command:?}");
    }
}

#[test]
fn inbox_unfiled_is_absent_unless_set_and_never_sent_without_the_capability() {
    let wire = |spec: QuerySpec| serde_json::to_value(Command::LibraryQuery { query: spec, max_sensitivity: Sensitivity::Restricted, offset: 0, limit: 1, expected_view: None }).unwrap()["query"].clone();
    // `unfiled: false` keeps today's request shape exactly; true adds only the field.
    let plain = wire(QuerySpec::default());
    assert!(plain.get("unfiled").is_none(), "{plain}");
    let inbox = wire(QuerySpec { unfiled: true, sort: SortKey::AddedAt, ..Default::default() });
    assert_eq!(inbox["unfiled"], serde_json::json!(true));
    assert!(inbox.get("group_id").is_none() && inbox.get("include_descendants").is_none());

    // An agent without `groups_v1.query_unfiled`: only `capabilities` goes out.
    let dir = tempfile::tempdir().unwrap();
    let (backend, path) = agent(&dir);
    let server = serve_once(&path, 0o600, Some(r#"{"ok":true,"capabilities":{"schema_version":1,"features":[],"groups_v1":{"tree_page_limit":200,"writes":true,"write_ops":["reorder_root"]}}}"#));
    let refused = Client::new(backend).library_query(&QuerySpec { unfiled: true, ..Default::default() }, Sensitivity::Restricted, 0, 1, None);
    assert_eq!(refused, Err(ClientError::Unsupported));
    let sent: serde_json::Value = serde_json::from_slice(&server.join().unwrap()).unwrap();
    assert_eq!(sent, serde_json::json!({"command":"capabilities"}));

    // The fixture with it: unfiled = in no folder; without it, refused.
    let fixture = FixtureBackend::new(40);
    let client = Client::new(fixture.clone());
    let all = client.library_query(&QuerySpec::default(), Sensitivity::Restricted, 0, 40, None).unwrap().total;
    let unfiled = client.library_query(&QuerySpec { unfiled: true, ..Default::default() }, Sensitivity::Restricted, 0, 40, None).unwrap();
    assert!(unfiled.total > 0 && unfiled.total < all, "{} of {all}", unfiled.total);
    let filed: std::collections::HashSet<String> = client.groups().unwrap().iter().map(|g| g.id.clone()).collect::<Vec<_>>().into_iter()
        .flat_map(|g| client.library_query(&QuerySpec { group: Some(GroupScope { group_id: g, include_descendants: false }), ..Default::default() }, Sensitivity::Restricted, 0, 40, None).unwrap().items.into_iter().map(|i| i.item_id))
        .collect();
    assert!(unfiled.items.iter().all(|i| !filed.contains(&i.item_id)));
    assert_eq!(unfiled.total + filed.len(), all);
    fixture.set_controls(FixtureControls { no_unfiled_query: true, ..Default::default() });
    client.forget_capabilities();
    assert_eq!(client.library_query(&QuerySpec { unfiled: true, ..Default::default() }, Sensitivity::Restricted, 0, 1, None), Err(ClientError::Unsupported));
}

#[test]
fn group_delete_commands_wire_shapes_and_capability_gate() {
    let r = Sensitivity::Restricted;
    let wire = |c: Command| serde_json::to_value(c).unwrap();
    assert_eq!(wire(Command::GroupDeletePreview { group_id: "g".into(), max_sensitivity: r }), serde_json::json!({"command":"group_delete_preview","group_id":"g","max_sensitivity":"restricted"}));
    assert_eq!(wire(Command::GroupDelete { group_id: "g".into(), confirmation_token: "t".into(), request_id: "q".into(), max_sensitivity: r }), serde_json::json!({"command":"group_delete","group_id":"g","confirmation_token":"t","request_id":"q","max_sensitivity":"restricted"}));
    // The first page names its cursor as null.
    assert_eq!(wire(Command::GroupDeleteList { cursor: None, max_sensitivity: r }), serde_json::json!({"command":"group_delete_list","cursor":null,"max_sensitivity":"restricted"}));
    assert_eq!(wire(Command::GroupDeleteRestore { deletion_id: "d".into(), request_id: "q".into(), max_sensitivity: r }), serde_json::json!({"command":"group_delete_restore","deletion_id":"d","request_id":"q","max_sensitivity":"restricted"}));

    // Without `groups_v1.node_delete` (even with writes): only `capabilities` goes out.
    let dir = tempfile::tempdir().unwrap();
    let (backend, path) = agent(&dir);
    let server = serve_once(&path, 0o600, Some(r#"{"ok":true,"capabilities":{"schema_version":1,"features":[],"groups_v1":{"tree_page_limit":200,"writes":true,"write_ops":["reorder_root","delete"]}}}"#));
    assert_eq!(Client::new(backend).group_delete_preview("g"), Err(ClientError::Unsupported));
    let sent: serde_json::Value = serde_json::from_slice(&server.join().unwrap()).unwrap();
    assert_eq!(sent, serde_json::json!({"command":"capabilities"}));

    // Reply bodies are flat beside `ok`; a preview not promising kept items is refused.
    let flat: Response = serde_json::from_str(r#"{"ok":true,"group_id":"g","name":"睡眠","confirmation_token":"t","expires_at":"x","affected_node_count":2,"affected_membership_count":5,"distinct_item_count":4,"includes_descendants":true,"items_retained":true,"recoverable":true}"#).unwrap();
    assert_eq!((flat.group_delete.affected_node_count, flat.group_delete.confirmation_token.as_deref()), (Some(2), Some("t")));

    // Fixture: delete the child, then the parent; restore the parent keeps the
    // child deleted; restoring the child first is refused; a replay returns the receipt.
    let fixture = FixtureBackend::new(40);
    let client = Client::new(fixture.clone());
    let has = |id: &str| client.groups().unwrap().iter().any(|g| g.id == id);
    let p = client.group_delete_preview("grp_c_bias").unwrap();
    assert_eq!(p.affected_node_count, 3);
    let child = client.group_delete("grp_c_bias", &p.confirmation_token, "q-child").unwrap();
    assert_eq!(client.group_delete("grp_c_bias", &p.confirmation_token, "q-child").unwrap(), child, "exact replay");
    let p = client.group_delete_preview("grp_c_cog").unwrap();
    // A stale token (the tree changed) or an expired one writes nothing.
    client.group_edit("q-r", Some("v1"), GroupEditOp::Rename { id: "grp_c_attention".into(), name: "专注".into() }).unwrap();
    assert!(matches!(client.group_delete("grp_c_cog", &p.confirmation_token, "q-p1"), Err(ClientError::Daemon(c)) if c.contains("stale")));
    let p = client.group_delete_preview("grp_c_cog").unwrap();
    fixture.set_controls(FixtureControls { expire_delete_tokens: true, ..Default::default() });
    assert!(matches!(client.group_delete("grp_c_cog", &p.confirmation_token, "q-p2"), Err(ClientError::Daemon(c)) if c == "group_delete_preview_expired"));
    assert!(has("grp_c_cog"));
    fixture.set_controls(FixtureControls::default());
    let p = client.group_delete_preview("grp_c_cog").unwrap();
    let parent = client.group_delete("grp_c_cog", &p.confirmation_token, "q-p3").unwrap();
    assert_eq!(client.group_delete_list().unwrap().len(), 2);
    assert!(matches!(client.group_delete_restore(&child, "q-rc"), Err(ClientError::Daemon(c)) if c == "group_delete_restore_conflict"));
    client.group_delete_restore(&parent, "q-rp").unwrap();
    assert!(has("grp_c_cog") && !has("grp_c_bias"));
    client.group_delete_restore(&child, "q-rc2").unwrap();
    assert!(has("grp_c_bias") && has("grp_c_anchor") && client.group_delete_list().unwrap().is_empty());
}

#[test]
fn reader_state_and_annotation_ops_have_the_contract_wire_shapes() {
    use crate::book::ReaderRequest;
    use crate::reader_state::*;
    let wire = |r: ReaderRequest| serde_json::to_value(Command::Reader { max_sensitivity: Sensitivity::Restricted, request: r }).unwrap();
    assert_eq!(wire(ReaderRequest::State { item_id: "i".into() })["request"], serde_json::json!({"op": "state", "item_id": "i"}));
    assert_eq!(wire(ReaderRequest::Resume)["request"], serde_json::json!({"op": "resume"}));
    let anchor = ReaderAnchor { revision_id: "r".into(), side: "source".into(), sha256: "h".into(), chapter: "section-1".into(), offset: 5 };
    let state = ReaderState { visit_id: "v".into(), position: Some(anchor.clone()), bookmarks: vec![Bookmark { id: "b".into(), label: "书签 1".into(), anchor }] };
    let save = wire(ReaderRequest::Save { item_id: "i".into(), expected_version: "x".into(), state });
    assert_eq!(save["request"]["op"], "save");
    assert_eq!(save["request"]["state"]["position"], serde_json::json!({"revision_id": "r", "side": "source", "sha256": "h", "chapter": "section-1", "offset": 5}));
    // The first list page sends a null version; later pages the version read.
    let first = wire(ReaderRequest::AnnotationsList { item_id: "i".into(), offset: 0, limit: 100, expected_version: None });
    assert_eq!(first["request"], serde_json::json!({"op": "annotations_list", "item_id": "i", "offset": 0, "limit": 100, "expected_version": null}));
    let mark = Annotation { id: "m".into(), style: MarkStyle::Blue, anchor: MarkAnchor { revision_id: "r".into(), side: "source".into(), sha256: "h".into(), segments: vec![Segment { chapter: "section-0".into(), start: 1, end: 4 }] } };
    let put = wire(ReaderRequest::AnnotationsEdit { item_id: "i".into(), expected_version: "v1".into(), change: AnnotationChange::Put { annotation: mark } });
    assert_eq!(put["request"], serde_json::json!({"op": "annotations_edit", "item_id": "i", "expected_version": "v1", "change": {"op": "put", "annotation": {"id": "m", "style": "blue",
        "anchor": {"revision_id": "r", "side": "source", "sha256": "h", "segments": [{"chapter": "section-0", "start": 1, "end": 4}]}}}}));
    let del = wire(ReaderRequest::AnnotationsEdit { item_id: "i".into(), expected_version: "v2".into(), change: AnnotationChange::Delete { id: "m".into() } });
    assert_eq!(del["request"]["change"], serde_json::json!({"op": "delete", "id": "m"}));
}

#[test]
fn reader_state_saves_by_cas_keeps_bookmarks_and_binds_to_the_revision() {
    use crate::reader_state::*;
    let fixture = FixtureBackend::new(30);
    let client = Client::new(fixture.clone());
    let id = "item_fixture_000002";
    let md = client.item_read(id).unwrap();
    let doc = client.reader_toc(id, &md.revision_id).unwrap();
    let src = doc.editions.iter().find(|e| e.side == "source").unwrap().clone();
    assert!(src.chapters.len() > 1 && src.chapters.iter().all(|c| c.key.starts_with("section-")));
    // A body byte at a chapter boundary is the next chapter at offset 0; EOF the last end.
    let boundary = src.chapters[1].start;
    assert_eq!(anchor_at(&src, &md.revision_id, boundary).unwrap().chapter, src.chapters[1].key);
    assert_eq!(anchor_at(&src, &md.revision_id, boundary).unwrap().offset, 0);
    let eof = anchor_at(&src, &md.revision_id, md.markdown.len()).unwrap();
    assert_eq!(body_at(&src, &md.revision_id, &eof), Some(md.markdown.len()));
    let empty = client.reader_state(id).unwrap();
    assert!(empty.state.position.is_none() && empty.state.bookmarks.is_empty());
    let at = anchor_at(&src, &md.revision_id, boundary + 3).unwrap();
    let bookmark = Bookmark { id: "mac-bm-1".into(), label: "书签 1".into(), anchor: at.clone() };
    let one = ReaderState { visit_id: "mac-visit-1".into(), position: Some(at.clone()), bookmarks: vec![bookmark.clone()] };
    let saved = client.reader_save_state(id, &empty.version, &one).unwrap();
    // An identical replay succeeds; an old version for a different state is stale.
    assert_eq!(client.reader_save_state(id, &empty.version, &one).unwrap().version, saved.version);
    let moved = ReaderState { position: Some(anchor_at(&src, &md.revision_id, 0).unwrap()), ..one.clone() };
    assert_eq!(client.reader_save_state(id, &empty.version, &moved), Err(ClientError::Daemon("stale_reader_state".into())));
    let after = client.reader_save_state(id, &saved.version, &moved).unwrap();
    assert_eq!(after.state.bookmarks, vec![bookmark]);
    // A new revision: the old anchor no longer binds and is not accepted anew.
    fixture.simulate_external_edit(id);
    let newer = client.item_read(id).unwrap();
    assert!(body_at(&src, &newer.revision_id, &at).is_none());
    let bad = ReaderState { bookmarks: vec![Bookmark { id: "mac-bm-2".into(), label: "x".into(), anchor: at }], ..moved };
    assert!(client.reader_save_state(id, &after.version, &bad).is_err());
}

#[test]
fn five_style_marks_round_trip_overlap_cross_chapters_and_reconcile_lost_replies() {
    use crate::reader_state::*;
    let fixture = FixtureBackend::new(30);
    let client = Client::new(fixture.clone());
    let id = "item_fixture_000002";
    let md = client.item_read(id).unwrap();
    let src = client.reader_toc(id, &md.revision_id).unwrap().editions.remove(0);
    let anchor = |r: std::ops::Range<usize>| MarkAnchor { revision_id: md.revision_id.clone(), side: "source".into(), sha256: src.sha256.clone(), segments: segments_of(&src, r) };
    let ledger = client.annotations_list(id).unwrap();
    assert!(ledger.annotations.is_empty());
    let mut version = ledger.version;
    // Multibyte: the emoji's own four bytes; a range inside it is rejected.
    let emoji = md.markdown.find('🌹').unwrap();
    let inside = Annotation { id: "m-bad".into(), style: MarkStyle::Yellow, anchor: anchor(emoji + 1..emoji + 3) };
    assert!(matches!(client.annotations_edit(id, &version, &AnnotationChange::Put { annotation: inside }), EditOutcome::Rejected(_)));
    // Across a chapter boundary: one mark with two segments.
    let boundary = src.chapters[1].start;
    let cross = anchor(boundary - 5..boundary + 5);
    assert_eq!(cross.segments.len(), 2);
    let mut put = |v: &mut String, idx: &str, style, a: MarkAnchor| match client.annotations_edit(id, v, &AnnotationChange::Put { annotation: Annotation { id: idx.into(), style, anchor: a } }) {
        EditOutcome::Saved(r) => { *v = r.version; r.outcome }
        other => panic!("{other:?}"),
    };
    for (k, style) in MarkStyle::ALL.into_iter().enumerate() {
        put(&mut version, &format!("m-{k}"), style, anchor(emoji..emoji + 4));
    }
    assert_eq!(put(&mut version, "m-x", MarkStyle::Red, cross.clone()), "created");
    // Overlapping distinct marks are kept; the exact duplicate is rejected.
    assert!(matches!(client.annotations_edit(id, &version, &AnnotationChange::Put { annotation: Annotation { id: "m-dup".into(), style: MarkStyle::Red, anchor: cross.clone() } }), EditOutcome::Rejected(c) if c == "reader_annotation_conflict"));
    // Changing a mark's style keeps its id and anchor.
    assert_eq!(put(&mut version, "m-x", MarkStyle::Underline, cross), "updated");
    let all = client.annotations_list(id).unwrap();
    assert_eq!(all.annotations.len(), 6);
    assert!(all.annotations.iter().all(ListedAnnotation::is_current));
    assert_eq!(ranges_of(&src, &md.revision_id, &all.annotations.iter().find(|a| a.id == "m-x").unwrap().anchor).unwrap().len(), 2);
    // Paging: more than one page, one version.
    for k in 0..120 {
        put(&mut version, &format!("p-{k:03}"), MarkStyle::Bold, anchor(k..k + 1));
    }
    assert_eq!(client.annotations_list(id).unwrap().annotations.len(), 126);
    // A lost reply: the edit happened; an exact retry returns the same result,
    // the old version for anything else is stale.
    fixture.set_controls(FixtureControls { lose_next_annotation_reply: true, ..Default::default() });
    let change = AnnotationChange::Delete { id: "m-0".into() };
    assert!(matches!(client.annotations_edit(id, &version, &change), EditOutcome::Unknown(_)));
    let EditOutcome::Saved(replayed) = client.annotations_edit(id, &version, &change) else { panic!() };
    assert_eq!(replayed.outcome, "deleted");
    assert_eq!(client.annotations_edit(id, &version, &AnnotationChange::Delete { id: "m-1".into() }), EditOutcome::Stale);
    // Committed, then internal_error (sync after rename failed): unknown, not
    // "not saved"; the exact replay returns the committed result.
    let version = replayed.version.clone();
    fixture.set_controls(FixtureControls { fail_after_next_annotation_commit: true, ..Default::default() });
    let change = AnnotationChange::Delete { id: "m-1".into() };
    assert!(matches!(client.annotations_edit(id, &version, &change), EditOutcome::Unknown(_)));
    assert!(client.annotations_list(id).unwrap().annotations.iter().all(|a| a.id != "m-1"));
    let EditOutcome::Saved(replayed) = client.annotations_edit(id, &version, &change) else { panic!() };
    assert_eq!(replayed.outcome, "deleted");
    // A new revision leaves marks stale (kept, not rebound).
    fixture.simulate_external_edit(id);
    assert!(client.annotations_list(id).unwrap().annotations.iter().all(|a| !a.is_current()));
    // Without the capability nothing is sent.
    fixture.set_controls(FixtureControls { no_reader_annotations: true, ..Default::default() });
    client.forget_capabilities();
    assert!(!client.annotations_supported());
    assert_eq!(client.annotations_list(id), Err(ClientError::Unsupported));
}

#[test]
fn ai_translation_mark_ops_have_the_contract_wire_shapes() {
    use crate::ai_marks::*;
    use crate::book::ReaderRequest;
    use crate::reader_state::MarkStyle;
    let wire = |r: ReaderRequest| serde_json::to_value(Command::Reader { max_sensitivity: Sensitivity::Restricted, request: r }).unwrap()["request"].clone();
    let first = wire(ReaderRequest::AiAnnotationsList { item_id: "i".into(), offset: 0, limit: 100, expected_version: None, annotation_space: AI_SPACE.into(), projection: None, expected_view_version: None });
    assert_eq!(first, serde_json::json!({"op": "annotations_list", "item_id": "i", "offset": 0, "limit": 100, "expected_version": null,
        "annotation_space": "ai_translation", "expected_view_version": null}));
    let segment = AiSegment { chapter: "c1".into(), translation_version: "v".into(), sha256: "h".into(), start: 3, end: 9 };
    let mark = AiAnnotation { id: "m".into(), style: MarkStyle::Red, anchor: AiAnchor { revision_id: "r".into(), side: AI_SIDE.into(), segments: vec![segment] } };
    let put = wire(ReaderRequest::AiAnnotationsEdit { item_id: "i".into(), expected_version: "e".into(), annotation_space: AI_SPACE.into(), change: AiAnnotationChange::Put { annotation: mark } });
    assert_eq!(put, serde_json::json!({"op": "annotations_edit", "item_id": "i", "expected_version": "e", "annotation_space": "ai_translation",
        "change": {"op": "put", "annotation": {"id": "m", "style": "red", "anchor": {"revision_id": "r", "side": "ai_translation",
            "segments": [{"chapter": "c1", "translation_version": "v", "sha256": "h", "start": 3, "end": 9}]}}}}));
    // A delete always names the space (without it only the edition ledger is touched).
    let del = wire(ReaderRequest::AiAnnotationsEdit { item_id: "i".into(), expected_version: "e".into(), annotation_space: AI_SPACE.into(), change: AiAnnotationChange::Delete { id: "m".into() } });
    assert_eq!((del["annotation_space"].clone(), del["change"].clone()), (serde_json::json!("ai_translation"), serde_json::json!({"op": "delete", "id": "m"})));
}

/// Records every command a fixture receives, serialized as on the wire.
struct Recording(FixtureBackend, std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>);
impl Backend for Recording {
    fn send(&self, command: &Command) -> Result<Response, ClientError> {
        self.1.lock().unwrap().push(serde_json::to_value(command).unwrap());
        self.0.send(command)
    }
    fn mode(&self) -> BackendMode { BackendMode::SyntheticFixture }
}

#[test]
fn ai_translation_marks_bind_to_chunked_utf8_bytes_two_chapters_and_go_stale_whole() {
    use crate::ai_marks::*;
    use crate::book::ReaderRequest;
    use crate::reader_state::{EditOutcome, MarkStyle};
    let fixture = FixtureBackend::new(40);
    fixture.set_controls(FixtureControls { chapter_translations: true, ..Default::default() });
    let sent = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let client = Client::new(Recording(fixture.clone(), sent.clone()));
    let id = crate::fixture::AI_BOOK_ITEM;
    let rev = client.item_read(id).unwrap().revision_id;
    let doc = client.reader_toc(id, &rev).unwrap();
    let meta1 = client.reader_translate_metadata(&doc, "chapter-1").unwrap().unwrap();
    let meta2 = client.reader_translate_metadata(&doc, "chapter-2").unwrap().unwrap();
    assert_ne!((&meta1.version, &meta1.sha256), (&meta2.version, &meta2.sha256), "two chapters, two identities");
    let text1 = client.reader_translation_text(&meta1).unwrap();
    let text2 = client.reader_translation_text(&meta2).unwrap();
    // The fixture serves 512-byte chunks: the second chunk of chapter 1 starts at
    // 512 (chapter-relative) and holds 「铜齿」 at its local bytes 15..21, so the
    // anchor is 512 + 15 = 527 .. 533 — chunk.start plus the local offset.
    let chunk = client.reader(ReaderRequest::TranslateChunk { item_id: id.into(), revision_id: rev.clone(), chapter: "chapter-1".into(),
        expected_version: meta1.version.clone(), offset: 512, limit: crate::book::CHUNK_LIMIT }).unwrap();
    assert_eq!((chunk["start"].as_u64(), &chunk["markdown"].as_str().unwrap().as_bytes()[15..21]), (Some(512), "铜齿".as_bytes()));
    assert_eq!(&text1.as_bytes()[527..533], "铜齿".as_bytes());
    // Each CJK character is three bytes: a range through the middle of one is refused locally.
    assert!(ai_segment(&meta1, &text1, 528..533).is_none());
    assert!(ai_segment(&meta1, &text1, 527..527).is_none());
    let in_chunk2 = ai_segment(&meta1, &text1, 527..533).unwrap();
    assert_eq!((in_chunk2.start, in_chunk2.end, in_chunk2.chapter.as_str()), (527, 533, "chapter-1"));
    // The same words twice (「潮水来了。潮水来了。」): the second one is its own bytes (253..265).
    let second = ai_segment(&meta1, &text1, 253..265).unwrap();
    assert_eq!(&text1[238..250], &text1[253..265]);
    let anchor = |segments: Vec<AiSegment>| AiAnchor { revision_id: rev.clone(), side: AI_SIDE.into(), segments };
    let ledger = client.ai_annotations_list(id).unwrap();
    assert!(ledger.annotations.is_empty());
    let mut version = ledger.version;
    let put = |v: &mut String, mark: &str, style: MarkStyle, a: AiAnchor| match client.ai_annotations_edit(id, v, &AiAnnotationChange::Put { annotation: AiAnnotation { id: mark.into(), style, anchor: a } }) {
        EditOutcome::Saved(r) => { *v = r.version; r.outcome }
        other => panic!("{other:?}"),
    };
    assert_eq!(put(&mut version, "m-chunk", MarkStyle::Yellow, anchor(vec![in_chunk2])), "created");
    assert_eq!(put(&mut version, "m-second", MarkStyle::Red, anchor(vec![second.clone()])), "created");
    assert_eq!(put(&mut version, "m-second", MarkStyle::Blue, anchor(vec![second])), "style_changed");
    // One mark over two chapters with different versions and hashes (「会。」 and the blank line: 3 + 3 + 2 bytes).
    let tail = ai_segment(&meta1, &text1, text1.len() - 8..text1.len()).unwrap();
    let head = ai_segment(&meta2, &text2, 0..text2.find('\n').unwrap()).unwrap();
    assert_eq!(put(&mut version, "m-two", MarkStyle::Underline, anchor(vec![tail.clone(), head.clone()])), "created");
    // Out of toc order: the whole mark is refused.
    assert!(matches!(client.ai_annotations_edit(id, &version, &AiAnnotationChange::Put { annotation: AiAnnotation { id: "m-bad".into(), style: MarkStyle::Bold, anchor: anchor(vec![head.clone(), tail.clone()]) } }), EditOutcome::Rejected(c) if c == "invalid_reader_anchor"));
    let all = client.ai_annotations_list(id).unwrap();
    assert_eq!(all.annotations.len(), 3);
    assert!(all.annotations.iter().all(ListedAiAnnotation::is_current));
    let two = all.annotations.iter().find(|a| a.id == "m-two").unwrap();
    assert_eq!(ai_ranges(&meta1, text1.len(), &rev, two), Some(vec![text1.len() - 8..text1.len()]));
    assert_eq!(ai_ranges(&meta2, text2.len(), &rev, two), Some(vec![head.start..head.end]));
    // Every AI request names the space; nothing goes to the edition ledger.
    for c in sent.lock().unwrap().iter().filter(|c| matches!(c["request"]["op"].as_str(), Some("annotations_list" | "annotations_edit"))) {
        assert_eq!(c["request"]["annotation_space"], "ai_translation");
    }

    // Chapter 2 is retranslated: the two-chapter mark is stale as a whole (its
    // chapter-1 segment still current), drawn on neither chapter, delete-only.
    fixture.retranslate_ai_chapter("chapter-2");
    let meta2b = client.reader_translate_metadata(&doc, "chapter-2").unwrap().unwrap();
    assert_ne!(meta2b.version, meta2.version);
    let all = client.ai_annotations_list(id).unwrap();
    let two = all.annotations.iter().find(|a| a.id == "m-two").unwrap();
    assert_eq!(two.status, "stale");
    assert_eq!(two.projections.iter().map(|p| (p.chapter.as_str(), p.status.as_str())).collect::<Vec<_>>(), vec![("chapter-1", "current"), ("chapter-2", "stale")]);
    assert_eq!(ai_ranges(&meta1, text1.len(), &rev, two), None);
    assert!(all.annotations.iter().filter(|a| a.id != "m-two").all(ListedAiAnnotation::is_current));
    let mut version = all.version;
    let restyle = AiAnnotationChange::Put { annotation: AiAnnotation { id: "m-two".into(), style: MarkStyle::Bold, anchor: two.anchor.clone() } };
    assert_eq!(client.ai_annotations_edit(id, &version, &restyle), EditOutcome::Rejected("stale_reader_translation".into()));
    // A new mark on the old chapter-2 identity is refused, nothing written.
    assert_eq!(client.ai_annotations_edit(id, &version, &AiAnnotationChange::Put { annotation: AiAnnotation { id: "m-old".into(), style: MarkStyle::Bold, anchor: anchor(vec![head]) } }),
        EditOutcome::Rejected("stale_reader_translation".into()));
    let EditOutcome::Saved(r) = client.ai_annotations_edit(id, &version, &AiAnnotationChange::Delete { id: "m-two".into() }) else { panic!() };
    assert_eq!(r.outcome, "deleted");
    version = r.version;
    assert_eq!(client.ai_annotations_list(id).unwrap().annotations.len(), 2);
    let _ = version;

    // Without the capability nothing AI is sent.
    fixture.set_controls(FixtureControls { chapter_translations: true, no_ai_annotations: true, ..Default::default() });
    client.forget_capabilities();
    sent.lock().unwrap().clear();
    assert!(!client.ai_annotations_supported());
    assert_eq!(client.ai_annotations_list(id), Err(ClientError::Unsupported));
    assert!(matches!(client.ai_annotations_edit(id, "x", &AiAnnotationChange::Delete { id: "m-chunk".into() }), EditOutcome::Rejected(_)));
    assert!(sent.lock().unwrap().iter().all(|c| c["command"] == "capabilities"), "only the capability probe");
}

#[test]
fn an_unknown_ai_mark_edit_is_reconciled_by_reading_and_resent_only_unchanged() {
    use crate::ai_marks::*;
    use crate::reader_state::{EditOutcome, MarkStyle};
    let fixture = FixtureBackend::new(40);
    fixture.set_controls(FixtureControls { chapter_translations: true, ..Default::default() });
    let sent = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let client = Client::new(Recording(fixture.clone(), sent.clone()));
    let id = crate::fixture::AI_BOOK_ITEM;
    let rev = client.item_read(id).unwrap().revision_id;
    let doc = client.reader_toc(id, &rev).unwrap();
    let meta = client.reader_translate_metadata(&doc, "chapter-1").unwrap().unwrap();
    let text = client.reader_translation_text(&meta).unwrap();
    let version = client.ai_annotations_list(id).unwrap().version;
    let change = AiAnnotationChange::Put { annotation: AiAnnotation { id: "m-lost".into(), style: MarkStyle::Yellow,
        anchor: AiAnchor { revision_id: rev.clone(), side: AI_SIDE.into(), segments: vec![ai_segment(&meta, &text, 86..98).unwrap()] } } };
    // Applied, reply lost: unknown, never "not saved".
    fixture.set_controls(FixtureControls { chapter_translations: true, lose_next_annotation_reply: true, ..Default::default() });
    assert!(matches!(client.ai_annotations_edit(id, &version, &change), EditOutcome::Unknown(_)));
    // Reconciled by reading: the mark is there.
    assert!(client.ai_annotations_list(id).unwrap().annotations.iter().any(|a| a.id == "m-lost" && a.is_current()));
    // The same request (same expected version, same change) returns the stored receipt.
    let edits = |s: &[serde_json::Value]| s.iter().filter(|c| c["request"]["op"] == "annotations_edit").cloned().collect::<Vec<_>>();
    let EditOutcome::Saved(replayed) = client.ai_annotations_edit(id, &version, &change) else { panic!() };
    assert_eq!(replayed.outcome, "created");
    let e = edits(&sent.lock().unwrap());
    assert_eq!(e.len(), 2);
    assert_eq!(e[0], e[1], "the resend is byte-for-byte the original request");
    // Anything else under the old version is stale, never applied blindly.
    assert_eq!(client.ai_annotations_edit(id, &version, &AiAnnotationChange::Delete { id: "m-lost".into() }), EditOutcome::Stale);
    // Committed, then internal_error: unknown as well.
    fixture.set_controls(FixtureControls { chapter_translations: true, fail_after_next_annotation_commit: true, ..Default::default() });
    let del = AiAnnotationChange::Delete { id: "m-lost".into() };
    assert!(matches!(client.ai_annotations_edit(id, &replayed.version, &del), EditOutcome::Unknown(_)));
    assert!(client.ai_annotations_list(id).unwrap().annotations.is_empty());
    let EditOutcome::Saved(r) = client.ai_annotations_edit(id, &replayed.version, &del) else { panic!() };
    assert_eq!(r.outcome, "deleted");
    // The edition ledger of the same item is untouched throughout.
    assert!(client.annotations_list(id).unwrap().annotations.is_empty());
}

#[test]
fn ai_mark_pages_bind_both_versions_and_restart_from_page_one_on_drift() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agent.sock");
    let (v, w) = ("1".repeat(64), "2".repeat(64));
    let mark = |id: &str| serde_json::json!({"id": id, "style": "bold", "status": "current", "order": 1,
        "anchor": {"revision_id": "r", "side": "ai_translation", "segments": [{"chapter": "c1", "translation_version": "tv", "sha256": "h", "start": 0, "end": 3}]},
        "projections": [{"chapter": "c1", "translation_version": "tv", "sha256": "h", "status": "current"}]});
    let (v2, w2) = (v.clone(), w.clone());
    let seen = serve_script(&path, move |c, n| {
        if c["command"] == "capabilities" {
            return serde_json::json!({"ok": true, "capabilities": {"schema_version": 1, "features": [crate::book::READER_FEATURE, crate::ai_marks::AI_ANNOTATIONS_FEATURE]}}).to_string();
        }
        let r = &c["request"];
        let page = |offset: usize, next: Option<usize>| serde_json::json!({"ok": true, "reader": {"item_id": "i", "annotation_space": "ai_translation",
            "version": v2, "view_version": w2, "annotations": [mark(&format!("m{offset}"))], "total": 2, "next_offset": next}}).to_string();
        match (r["offset"].as_u64(), n) {
            (Some(0), _) => page(0, Some(1)),
            // The view moved between pages once: page 1 again, then it holds.
            (Some(1), 2) => r#"{"ok":false,"reason_code":"stale_reader_annotations"}"#.into(),
            (Some(1), _) => page(1, None),
            _ => r#"{"ok":false,"reason_code":"invalid_reader_annotation"}"#.into(),
        }
    });
    let client = Client::new(AgentBackend::new(path).with_timeout(Duration::from_millis(500)));
    let ledger = client.ai_annotations_list("i").unwrap();
    assert_eq!((ledger.version.as_str(), ledger.view_version.as_str(), ledger.annotations.len()), (v.as_str(), w.as_str(), 2));
    let pages: Vec<serde_json::Value> = seen.lock().unwrap().iter().filter(|c| c["command"] == "reader").map(|c| c["request"].clone()).collect();
    let first = serde_json::json!({"op": "annotations_list", "item_id": "i", "offset": 0, "limit": 100, "expected_version": null, "annotation_space": "ai_translation", "expected_view_version": null});
    let later = serde_json::json!({"op": "annotations_list", "item_id": "i", "offset": 1, "limit": 100, "expected_version": v, "annotation_space": "ai_translation", "expected_view_version": w});
    assert_eq!(pages, vec![first.clone(), later.clone(), first, later]);
}

#[test]
fn the_ai_book_transcript_has_a_validated_one_to_one_block_correspondence() {
    let fixture = FixtureBackend::new(40);
    fixture.set_controls(FixtureControls { chapter_translations: true, ..Default::default() });
    let client = Client::new(fixture);
    let id = crate::fixture::AI_BOOK_ITEM;
    let rev = client.item_read(id).unwrap().revision_id;
    let doc = client.reader_toc(id, &rev).unwrap();
    let meta = client.reader_translate_metadata(&doc, "chapter-3").unwrap().unwrap();
    let text = client.reader_translation_text(&meta).unwrap();
    let original = client.reader_chapter(&doc, "original", "chapter-3").unwrap();
    let crate::book::AiAlignment::Aligned(a) = client.reader_translate_alignment(&meta, 4).unwrap() else { panic!("aligned") };
    a.validate(&original, Some(&text)).unwrap();
    assert_eq!(a.pairs.len(), 15);
    assert!(a.pairs.iter().all(|p| p.relation == crate::book::Relation::Matched));
}

#[test]
fn durations_read_as_minutes_under_an_hour_and_hours_after() {
    assert_eq!(duration_label(Some(59)).as_deref(), Some("00:59"));
    assert_eq!(duration_label(Some(3599)).as_deref(), Some("59:59"));
    assert_eq!(duration_label(Some(3626)).as_deref(), Some("1:00:26"));
    assert_eq!(duration_label(None), None);
    assert_eq!(published_line(Some("2026-10-30"), Some(3626)).as_deref(), Some("发布 2026-10-30 · 1:00:26"));
    assert_eq!(published_line(Some("2026-10-30"), None).as_deref(), Some("发布 2026-10-30"));
    assert_eq!(published_line(None, Some(59)).as_deref(), Some("00:59"));
    assert_eq!(published_line(None, None), None);
}

#[test]
fn publication_day_and_duration_are_optional_on_every_list_and_metadata_reply() {
    // Older agents omit both fields; newer ones send values or explicit nulls.
    let list: Response = serde_json::from_str(r#"{"ok":true,"library_items":[
        {"item_id":"a","revision_id":"r","title":"Old","item_type":"webpage"},
        {"item_id":"b","revision_id":"r","title":"Null","item_type":"webpage","published_at":null,"duration_seconds":null},
        {"item_id":"c","revision_id":"r","title":"Episode","item_type":"webpage","published_at":"2026-11-06","duration_seconds":3723,"unknown_extra":1}]}"#).unwrap();
    let items = list.library_items.unwrap();
    assert_eq!(items.iter().map(|i| (i.published_at.as_deref(), i.duration_seconds)).collect::<Vec<_>>(),
        vec![(None, None), (None, None), (Some("2026-11-06"), Some(3723))]);
    let query: QueryItem = serde_json::from_str(r#"{"item_id":"a","revision_id":"r","title":"T","item_type":"webpage"}"#).unwrap();
    assert_eq!((query.published_at, query.duration_seconds), (None, None));
    let query: QueryItem = serde_json::from_str(r#"{"item_id":"a","revision_id":"r","title":"T","item_type":"webpage","published_at":"2026-10-30","duration_seconds":3626}"#).unwrap();
    assert_eq!((query.published_at.as_deref(), query.duration_seconds), (Some("2026-10-30"), Some(3626)));
    let read = |extra: &str| -> Bibliographic {
        let body = format!(r#"{{"metadata_schema_version":2,"item":{{"item_id":"i","revision_id":"r","item_type":"webpage","metadata":{{}}}},"published_date":{{"value":"2026-10-30","precision":"day","raw":null}}{extra}}}"#);
        serde_json::from_str::<biblio::MetadataV2Read>(&body).unwrap().into()
    };
    assert_eq!(read("").duration_seconds, None);
    assert_eq!(read(r#","duration_seconds":null"#).duration_seconds, None);
    let b = read(r#","duration_seconds":3626"#);
    assert_eq!((b.duration_seconds, b.published_date.map(|d| d.display())), (Some(3626), Some("2026-10-30".into())));
}

#[test]
fn the_fixture_lists_only_day_precision_publication_and_its_durations() {
    let client = Client::new(FixtureBackend::new(40));
    let w = client.library_query(&QuerySpec::default(), Sensitivity::Restricted, 0, 40, None).unwrap();
    let find = |id: &str| w.items.iter().find(|i| i.item_id == id).unwrap();
    let podcast = find(crate::fixture::PODCAST_ITEM);
    assert_eq!((podcast.published_at.as_deref(), podcast.duration_seconds), (Some("2026-10-30"), Some(3626)));
    // A year-only publication is not a day: null, never the capture date.
    let year_only = find("item_fixture_000005");
    assert_eq!(year_only.bibliographic.as_ref().and_then(|b| b.published_date.as_ref()).map(|d| d.precision), Some(DatePrecision::Year));
    assert_eq!(year_only.published_at, None);
    assert_eq!(find("item_fixture_000004").duration_seconds, Some(59));
    let page = client.library_page(None).unwrap();
    let legacy = page.items.iter().find(|i| i.item_id == crate::fixture::PODCAST_ITEM).unwrap();
    assert_eq!((legacy.published_at.as_deref(), legacy.duration_seconds), (Some("2026-10-30"), Some(3626)));
    // The metadata read carries the duration; a book answers without the field.
    let b = client.bibliographic(crate::fixture::PODCAST_ITEM, &podcast.revision_id, Sensitivity::Restricted).unwrap();
    assert_eq!(b.duration_seconds, Some(3626));
    let book = w.items.iter().find(|i| i.item_type == "书籍").unwrap();
    assert_eq!(client.bibliographic(&book.item_id, &book.revision_id, Sensitivity::Restricted).unwrap().duration_seconds, None);
}


#[test]
fn newer_agent_diagnostics_are_read_and_older_agents_omit_them() {
    // started_at in RFC 3339 (Z, offset, fraction); unreadable is absent.
    let p: TranslationProgress = serde_json::from_str(r#"{"done_batches":1,"total_batches":2,"started_at":"2026-11-09T09:34:12.123456+00:00"}"#).unwrap();
    assert_eq!(p.started_at, crate::dto::parse_rfc3339("2026-11-09T17:34:12+08:00"));
    assert_eq!(crate::dto::parse_rfc3339("1970-01-01T00:01:00Z"), Some(60));
    let bad: TranslationProgress = serde_json::from_str(r#"{"done_batches":1,"total_batches":2,"started_at":"yesterday"}"#).unwrap();
    assert_eq!(bad.started_at, None);
    let back: TranslationProgress = serde_json::from_value(serde_json::to_value(p).unwrap()).unwrap();
    assert_eq!(back, p);
    // 翻译中 1/2 段 · 已用 3 分钟; an older agent keeps 翻译中 1/2.
    let start = p.started_at.unwrap();
    assert!(matches!(translating_label_at(Some(&p), start + 200).as_str(), "翻译中 1/2 段 · 已用 3 分钟" | "Translating 1/2 parts · 3 min so far"));
    assert!(matches!(translating_label_at(Some(&p), start + 10).as_str(), "翻译中 1/2 段 · 刚开始" | "Translating 1/2 parts · just started"));
    let old = TranslationProgress { started_at: None, ..p };
    assert!(matches!(translating_label_at(Some(&old), start + 200).as_str(), "翻译中 1/2" | "Translating 1/2"));
    // failure_detail in plain words; an unknown code falls back to the reason.
    assert!(matches!(failure_detail_label("batch_3_of_5:count_mismatch").as_deref(), Some("第 3 段：模型返回块数不对" | "Part 3: the model returned the wrong number of paragraphs")));
    assert!(matches!(failure_detail_label("title:cjk_ratio").as_deref(), Some("标题：译文里中文太少" | "Title: the translation had too little Chinese")));
    assert_eq!(failure_detail_label("batch_2:something_new"), None);
    assert_eq!(failure_detail_label("nonsense"), None);
    let s: TranslationStatus = serde_json::from_str(r#"{"state":"failed","reason":"retry_exhausted","failure_detail":"batch_2:json_parse","failure_context":{"expected_blocks":4}}"#).unwrap();
    assert_eq!(s.failure_detail.as_deref(), Some("batch_2:json_parse"));
    let s: TranslationStatus = serde_json::from_str(r#"{"state":"succeeded","untranslated_blocks":3}"#).unwrap();
    assert_eq!(s.untranslated_blocks, Some(3));
    let s: TranslationStatus = serde_json::from_str(r#"{"state":"failed"}"#).unwrap();
    assert_eq!((s.failure_detail, s.untranslated_blocks), (None, None));
}

#[test]
fn folder_transfer_requires_explicit_capability_and_preserves_frozen_wire_identity() {
    use std::collections::BTreeMap;
    let op = GroupEditOp::TransferFolders { source_ids: vec!["grp_c_sleep".into()], target_parent_id: "grp_u_mine".into(),
        expected_versions: BTreeMap::from([("grp_z_psych".into(), "v1".into()), ("grp_c_sleep".into(), "v1".into())]), moving: false };
    let wire = serde_json::to_value(Command::GroupEdit { max_sensitivity: Sensitivity::Restricted, request_id: "frozen".into(), expected_version: Some("v1".into()), ops: vec![op.clone()] }).unwrap();
    assert_eq!(wire, serde_json::json!({"command":"group_edit","max_sensitivity":"restricted","request_id":"frozen","expected_version":"v1","ops":[{
        "op":"transfer_folders","source_ids":["grp_c_sleep"],"target_parent_id":"grp_u_mine","expected_versions":{"grp_z_psych":"v1","grp_c_sleep":"v1"},"moving":false}]}));
    let fixture = FixtureBackend::new(40);
    let client = Client::new(fixture.clone());
    assert!(!client.capabilities().unwrap().group_transfer_folders);
    assert_eq!(client.group_edit("frozen", Some("v1"), op.clone()), Err(ClientError::Unsupported));
    assert_eq!(client.groups().unwrap().len(), 15);
    fixture.set_controls(FixtureControls { transfer_folders_op: true, lose_next_folder_reply: true, ..fixture.controls() });
    client.forget_capabilities();
    assert!(client.capabilities().unwrap().group_transfer_folders);
    assert_eq!(client.group_edit("frozen", Some("v1"), op.clone()), Err(ClientError::Timeout));
    assert_eq!(client.groups().unwrap().len(), 16);
    let receipt = client.group_edit_receipt("frozen", Some("v1"), op.clone()).unwrap().unwrap();
    assert_eq!((receipt.group_id.as_str(), receipt.version.as_str()), ("grp_u_mine", "v2"));
    client.group_edit("rename-target", Some("v2"), GroupEditOp::Rename { id: "grp_u_mine".into(), name: "renamed".into() }).unwrap();
    assert_eq!(client.group_edit_receipt("frozen", Some("v1"), op).unwrap(), Some(receipt), "historical receipt, not the current target version");
    assert_eq!(client.groups().unwrap().len(), 16);
}

#[test]
fn folder_transfer_accepts_only_a_bound_receipt_without_requiring_the_tree() {
    struct Reply(FixtureBackend, serde_json::Value);
    impl Backend for Reply {
        fn send(&self, command: &Command) -> Result<Response, ClientError> {
            if matches!(command, Command::GroupEdit { .. }) { serde_json::from_value(self.1.clone()).map_err(|_| ClientError::InvalidFrame) }
            else { self.0.send(command) }
        }
        fn mode(&self) -> BackendMode { BackendMode::SyntheticFixture }
    }
    for (receipt, accepted) in [
        (serde_json::json!({"group_id":"grp_u_mine","version":"v2"}), true),
        (serde_json::Value::Null, false),
        (serde_json::json!({"group_id":"grp_c_sleep","version":"v2"}), false),
        (serde_json::json!({"group_id":"grp_u_mine","version":""}), false),
    ] {
        let fixture = FixtureBackend::new(10);
        fixture.set_controls(FixtureControls { transfer_folders_op: true, ..fixture.controls() });
        let client = Client::new(Reply(fixture, serde_json::json!({"ok":true,"group_edit":receipt})));
        let result = client.group_edit_receipt("receipt", Some("v1"), GroupEditOp::TransferFolders { source_ids: vec!["grp_c_sleep".into()],
            target_parent_id: "grp_u_mine".into(), expected_versions: Default::default(), moving: false });
        if accepted { assert_eq!(result.unwrap().unwrap().version, "v2"); }
        else { assert_eq!(result, Err(ClientError::InvalidFrame)); }
    }
}

#[test]
fn folder_tree_size_retries_shrink_at_the_same_offset_and_keep_the_frozen_view() {
    struct LargeTree {
        sent: std::sync::Arc<std::sync::Mutex<Vec<(usize, usize, Option<String>)>>>,
        always_large: bool,
    }
    impl Backend for LargeTree {
        fn send(&self, command: &Command) -> Result<Response, ClientError> {
            let Command::GroupTreeRead { offset, limit, expected_view, .. } = command else {
                assert!(matches!(command, Command::Capabilities), "this flow sends no writes");
                let mut reply = FixtureBackend::new(1).send(command)?;
                reply.capabilities.as_mut().unwrap().groups_v1.as_mut().unwrap().tree_page_limit = 500;
                return Ok(reply);
            };
            self.sent.lock().unwrap().push((*offset, *limit, expected_view.clone()));
            let end = (offset + limit).min(283);
            let nodes: Vec<_> = (*offset..end).map(|i| serde_json::json!({"id":format!("group_{i}"),"name":"n".repeat(1024),"kind":"collection",
                "parent_id":if i == 0 { None } else { Some("group_0") },"root_group_id":"group_0","item_count":0,"subtree_item_count":0,"version":"v1"})).collect();
            let reply = serde_json::json!({"ok":true,"group_tree":{"schema_version":1,"view":"frozen","total":283,"offset":offset,"next_offset":if end < 283 { Some(end) } else { None },"nodes":nodes}});
            // A later page has a smaller bound too, exercising shrink after a
            // view is already installed rather than only at the initial page.
            let max = if *offset == 0 { 256 * 1024 } else { 100 * 1024 };
            if self.always_large || serde_json::to_vec(&reply).unwrap().len() > max { return Err(ClientError::Daemon("result_too_large".into())); }
            serde_json::from_value(reply).map_err(|_| ClientError::InvalidFrame)
        }
        fn mode(&self) -> BackendMode { BackendMode::SyntheticFixture }
    }
    for always_large in [false, true] {
        let sent = std::sync::Arc::new(std::sync::Mutex::new(vec![]));
        let client = Client::new(LargeTree { sent: sent.clone(), always_large });
        let result = client.groups();
        let sent = sent.lock().unwrap();
        assert_eq!(&sent[..3], &[(0, 500, None), (0, 250, None), (0, 125, None)]);
        if always_large {
            assert_eq!(result, Err(ClientError::Daemon("result_too_large".into())));
            assert!(sent.len() <= 10 && sent.last().unwrap().1 == 1);
        } else {
            let nodes = result.unwrap();
            assert_eq!(nodes.len(), 283);
            for (i, node) in nodes.iter().enumerate() { assert_eq!(node.id, format!("group_{i}")); }
            assert_eq!(&sent[3..5], &[(125, 125, Some("frozen".into())), (125, 62, Some("frozen".into()))]);
            assert!(sent.iter().skip(3).all(|(_, _, view)| view.as_deref() == Some("frozen")));
        }
    }
}
