use memex_models::*;
use rusqlite::Connection;

fn config() -> Configuration {
    Configuration {
        providers: vec![Provider {
            id: "local-proxy".into(),
            label: "本机代理（可能转发远端）".into(),
            api_base: "http://127.0.0.1:8317/v1/".into(),
        }],
        models: vec![
            Model {
                id: "astra".into(),
                label: "Astra".into(),
                provider_id: "local-proxy".into(),
                model_id: "gpt-6-astra".into(),
            },
            Model {
                id: "terra-manual".into(),
                label: "待验证 Terra".into(),
                provider_id: "local-proxy".into(),
                model_id: "gpt-6.1-terra".into(),
            },
        ],
        selections: vec![Selection {
            purpose: Purpose::Chat,
            model: "astra".into(),
            reasoning_effort: None,
        }],
    }
}

#[test]
fn persisted_selection_is_atomic_revisioned_and_not_an_execution_claim() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("settings.db");
    let mut a = Connection::open(&path).unwrap();
    let mut b = Connection::open(&path).unwrap();
    assert_eq!(read(&a).unwrap().revision, 0);
    let first = replace(&mut a, 0, config()).unwrap();
    let mut changed = first.configuration.clone();
    changed.selections[0].model = "terra-manual".into();
    changed.selections.push(Selection {
        purpose: Purpose::Suggestions,
        model: "astra".into(),
        reasoning_effort: None,
    });
    let second = replace(&mut b, first.revision, changed).unwrap();
    assert_eq!(
        replace(&mut a, first.revision, config())
            .unwrap_err()
            .to_string(),
        "stale_model_settings"
    );
    drop(a);
    let a = Connection::open(&path).unwrap();
    let saved = read(&a).unwrap();
    assert_eq!(saved, second);
    assert_eq!(
        saved
            .configuration
            .target(Purpose::Chat)
            .unwrap()
            .1
            .model_id,
        "gpt-6.1-terra"
    );
    assert_eq!(
        saved
            .configuration
            .target(Purpose::Suggestions)
            .unwrap()
            .1
            .model_id,
        "gpt-6-astra"
    );
    assert!(saved.configuration.target(Purpose::Translation).is_none());
    let mut broken = saved.configuration.clone();
    broken.models.clear();
    assert!(replace(&mut b, second.revision, broken).is_err());
    assert_eq!(read(&b).unwrap(), saved);
    let provider_only = Configuration {
        providers: saved.configuration.providers,
        ..Default::default()
    };
    assert_eq!(
        replace(&mut b, second.revision, provider_only)
            .unwrap()
            .configuration
            .models
            .len(),
        0
    );
}

#[test]
fn low_dial_persists_independently_with_cas_and_bounded_costs() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("route.db");
    let mut a = Connection::open(&path).unwrap();
    let original = replace(&mut a, 0, config()).unwrap();
    assert_eq!(low_dial::read(&a).unwrap(), None);
    let profile = low_dial::Profile { revision:0, model:"terra-manual".into(),
        reasoning_effort:None, max_input_bytes:8192, max_completion_tokens:512 };
    let saved = low_dial::replace(&mut a, 0, profile.clone()).unwrap();
    let mut b = Connection::open(&path).unwrap();
    assert_eq!(low_dial::read(&b).unwrap(), Some(saved.clone()));
    assert_eq!(read(&b).unwrap(), original);
    assert_eq!(low_dial::replace(&mut b, 0, profile.clone()).unwrap_err().to_string(), "stale_low_dial_profile");
    for bad in [low_dial::Profile { model:"missing".into(), ..profile.clone() },
        low_dial::Profile { max_input_bytes:262_145, ..profile.clone() },
        low_dial::Profile { max_completion_tokens:16_001, ..profile.clone() },
        low_dial::Profile { max_completion_tokens:0, ..profile.clone() },
        low_dial::Profile { reasoning_effort:Some(ReasoningEffort::High), ..profile.clone() }] {
        assert!(low_dial::replace(&mut b, saved.revision, bad).is_err());
        assert_eq!(low_dial::read(&a).unwrap(), Some(saved.clone()));
    }
    assert_eq!(read(&a).unwrap(), original);
    // Runaway guards, not cost caps: large mechanical windows are accepted.
    let wide = low_dial::Profile { max_input_bytes:131_072, max_completion_tokens:8192, ..profile.clone() };
    assert_eq!(low_dial::replace(&mut b, saved.revision, wide).unwrap().max_input_bytes, 131_072);
}

#[test]
fn purpose_effort_preferences_preserve_legacy_absence_and_survive_reopen() {
    let legacy = r#"{"purpose":"translation","model":"astra"}"#;
    let selection: Selection = serde_json::from_str(legacy).unwrap();
    assert_eq!(selection.reasoning_effort, None);
    assert_eq!(serde_json::to_string(&selection).unwrap(), legacy);
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("settings.db");
    let mut db = Connection::open(&path).unwrap();
    let mut configuration = config();
    configuration.selections = [
        (Purpose::Chat, Some(ReasoningEffort::Medium)),
        (Purpose::Suggestions, None),
        (Purpose::Summary, Some(ReasoningEffort::High)),
        (Purpose::Translation, Some(ReasoningEffort::Low)),
        (Purpose::Classification, Some(ReasoningEffort::None)),
    ]
    .into_iter()
    .map(|(purpose, reasoning_effort)| Selection {
        purpose,
        model: "astra".into(),
        reasoning_effort,
    })
    .collect();
    let saved = replace(&mut db, 0, configuration.clone()).unwrap();
    drop(db);
    assert_eq!(read(&Connection::open(&path).unwrap()).unwrap(), saved);
    let mut wire = serde_json::to_value(configuration).unwrap();
    assert_eq!(wire["selections"][3]["reasoning_effort"], "low");
    assert_eq!(wire["selections"][4]["reasoning_effort"], "none");
    wire["selections"][3]["reasoning_effort"] = "ultra".into();
    assert!(serde_json::from_value::<Configuration>(wire).is_err());
}

#[test]
fn invalid_endpoints_credentials_and_ambiguous_routes_are_rejected() {
    for endpoint in [
        "http://localhost:8317/v1/",
        "http://example.com/v1/",
        "https://u:secret@example.com/v1/",
        "https://example.com/v1/?api_key=secret",
        "https://example.com/v1/#secret",
        "file:///tmp/",
        "http://127.0.0.1:8317/v1",
        "http://127.0.0.1:0/v1/",
    ] {
        let mut c = config();
        c.providers[0].api_base = endpoint.into();
        assert_eq!(
            c.validate().unwrap_err().to_string(),
            "invalid_model_settings"
        );
    }
    for endpoint in [
        "https://api.example.com/v1/",
        "http://127.0.0.1:8317/v1/",
        "http://[::1]:8317/v1/",
    ] {
        let mut c = config();
        c.providers[0].api_base = endpoint.into();
        c.validate().unwrap();
    }
    let mut c = config();
    c.selections.push(c.selections[0].clone());
    assert!(c.validate().is_err());
    let mut c = config();
    c.models[1].id = c.models[0].id.clone();
    assert!(c.validate().is_err());
    let mut c = config();
    c.providers.clear();
    assert!(c.validate().is_err());
    let mut wire = serde_json::to_value(config()).unwrap();
    wire["providers"][0]["api_key"] = "synthetic-secret".into();
    assert!(serde_json::from_value::<Configuration>(wire).is_err());
}
