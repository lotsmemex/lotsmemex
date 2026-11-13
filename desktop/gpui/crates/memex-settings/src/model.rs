//! Settings state for the model page. The agent owns configuration, credentials
//! and discovery (`memex.model_settings.md`); this keeps only the draft being
//! edited, what the agent last reported, and presentation facts. No key is held
//! here: credentials appear only as the agent's state and masked suffix.

use memex_client::{tx, txf, ClientError, CredentialState, ModelCard, ModelConfiguration, ModelSettings, Provider, ProviderCredential, ProviderModels, Purpose, Selection};
use std::collections::BTreeMap;

/// One row of the approved s2 layout. `purpose` is `None` where the backend has no
/// route yet; such rows are shown disabled as "待后端支持". Labels and hints are
/// Chinese keys; the view shows them through `tx`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row {
    pub group: Group,
    pub label: &'static str,
    pub hint: &'static str,
    pub purpose: Option<Purpose>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    Understanding,
    Translation,
    Decision,
}

impl Group {
    pub fn title(self) -> (&'static str, &'static str) {
        let (title, desc) = match self {
            Group::Understanding => ("对话与理解", "用于对话小结、核心内容提炼和上下文对话。"),
            Group::Translation => ("翻译", "用于全文与段落翻译。"),
            Group::Decision => ("决策与分类", "用于海量条目快速分类整理、快速判断。"),
        };
        (tx(title), tx(desc))
    }
}

pub const ROWS: [Row; 6] = [
    Row { group: Group::Understanding, label: "上下文对话", hint: "", purpose: Some(Purpose::Chat) },
    Row { group: Group::Understanding, label: "对话小结", hint: "", purpose: Some(Purpose::Summary) },
    Row { group: Group::Understanding, label: "核心内容提炼", hint: "", purpose: None },
    Row { group: Group::Understanding, label: "建议提问", hint: "仅生成追问建议", purpose: Some(Purpose::Suggestions) },
    // Saving this route does not take over the existing translation worker.
    Row { group: Group::Translation, label: "翻译", hint: "现有翻译任务暂按原配置执行", purpose: Some(Purpose::Translation) },
    Row { group: Group::Decision, label: "批量分类", hint: "适合轻量快速的判断模型", purpose: Some(Purpose::Classification) },
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Loading,
    Ready,
    Saving,
    /// The agent rejected the save because settings changed elsewhere; the draft is kept.
    Conflict,
    /// Old agent or no socket: settings cannot be read or written.
    Unavailable(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelSource {
    Discovered,
    ManualUnverified,
}

#[derive(Debug, Default)]
pub struct SettingsModel {
    snapshot: Option<ModelSettings>,
    draft: ModelConfiguration,
    status: Option<Status>,
    message: Option<String>,
    credentials: BTreeMap<String, ProviderCredential>,
    discovered: BTreeMap<String, ProviderModels>,
}

impl SettingsModel {
    pub fn status(&self) -> Status {
        self.status.clone().unwrap_or(Status::Loading)
    }
    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }
    pub fn set_message(&mut self, m: Option<String>) {
        self.message = m;
    }
    pub fn revision(&self) -> Option<u64> {
        self.snapshot.as_ref().map(|s| s.revision)
    }
    pub fn draft(&self) -> &ModelConfiguration {
        &self.draft
    }
    pub fn is_dirty(&self) -> bool {
        self.snapshot.as_ref().is_some_and(|s| s.configuration != self.draft)
    }
    /// Credential and discovery commands bind the saved revision, so they wait for a save.
    pub fn provider_is_saved(&self, provider_id: &str) -> bool {
        self.snapshot.as_ref().is_some_and(|s| s.configuration.providers.iter().any(|p| p.id == provider_id))
            && !self.is_dirty()
    }

    pub fn loaded(&mut self, result: Result<ModelSettings, ClientError>) {
        match result {
            Ok(s) => {
                // A reload resolves a save conflict; other notices stay until dismissed.
                if self.status == Some(Status::Conflict) {
                    self.message = None;
                }
                self.draft = s.configuration.clone();
                // Facts bound to an older revision are no longer valid.
                self.discovered.retain(|_, d| d.settings_revision == s.revision);
                self.credentials.retain(|_, c| c.settings_revision == s.revision);
                self.snapshot = Some(s);
                self.status = Some(Status::Ready);
            }
            Err(ClientError::Unsupported) => self.status = Some(Status::Unavailable(tx("当前代理未提供模型设置（可能是旧版本）").into())),
            Err(e) => self.status = Some(Status::Unavailable(e.message())),
        }
    }

    pub fn selection(&self, purpose: Purpose) -> Option<&ModelCard> {
        let id = &self.draft.selections.iter().find(|s| s.purpose == purpose)?.model;
        self.draft.models.iter().find(|m| &m.id == id)
    }

    /// Changing the model keeps the purpose's effort; clearing the route drops both.
    pub fn select(&mut self, purpose: Purpose, model: Option<String>) {
        let effort = self.reasoning_effort(purpose).map(str::to_string);
        self.draft.selections.retain(|s| s.purpose != purpose);
        if let Some(model) = model {
            self.draft.selections.push(Selection { purpose, model, reasoning_effort: effort });
            self.draft.selections.sort_by_key(|s| s.purpose);
        }
    }

    /// The saved value verbatim, including one this version does not know.
    pub fn reasoning_effort(&self, purpose: Purpose) -> Option<&str> {
        self.draft.selections.iter().find(|s| s.purpose == purpose)?.reasoning_effort.as_deref()
    }

    /// Effort belongs to a route; without a selected model there is nothing to set.
    pub fn set_reasoning_effort(&mut self, purpose: Purpose, effort: Option<&str>) {
        if let Some(s) = self.draft.selections.iter_mut().find(|s| s.purpose == purpose) {
            s.reasoning_effort = effort.map(str::to_string);
        }
    }

    pub fn add_provider(&mut self, label: &str, api_base: &str) -> Result<(), String> {
        let label = label.trim();
        let mut base = api_base.trim().to_string();
        if label.is_empty() || base.is_empty() {
            return Err(tx("请填写名称和地址").into());
        }
        if !base.ends_with('/') {
            base.push('/');
        }
        let id = unique_id(&slug(label, "provider"), self.draft.providers.iter().map(|p| p.id.as_str()));
        self.draft.providers.push(Provider { id, label: label.into(), api_base: base });
        Ok(())
    }

    /// Removing a provider leaves its Keychain entry; ask for an explicit clear first.
    pub fn remove_provider(&mut self, provider_id: &str) -> Result<(), String> {
        if self.credentials.get(provider_id).is_some_and(|c| c.state != CredentialState::Missing) {
            return Err(tx("删除前请先清除该服务方的密钥，否则密钥会留在钥匙串中").into());
        }
        let models: Vec<String> = self.draft.models.iter().filter(|m| m.provider_id == provider_id).map(|m| m.id.clone()).collect();
        for m in models {
            self.remove_model(&m);
        }
        self.draft.providers.retain(|p| p.id != provider_id);
        Ok(())
    }

    pub fn set_provider_base(&mut self, provider_id: &str, api_base: &str) {
        if let Some(p) = self.draft.providers.iter_mut().find(|p| p.id == provider_id) {
            p.api_base = api_base.trim().to_string();
        }
    }

    /// A manually entered upstream ID; never presented as available.
    pub fn add_model(&mut self, provider_id: &str, label: &str, model_id: &str) -> Result<String, String> {
        let model_id = model_id.trim();
        if model_id.is_empty() || model_id.chars().any(char::is_whitespace) {
            return Err(tx("模型 ID 不能为空，也不能含空格").into());
        }
        if self.draft.models.iter().any(|m| m.provider_id == provider_id && m.model_id == model_id) {
            return Err(tx("该模型已在列表中").into());
        }
        let label = if label.trim().is_empty() { model_id } else { label.trim() };
        let id = unique_id(&slug(model_id, "model"), self.draft.models.iter().map(|m| m.id.as_str()));
        self.draft.models.push(ModelCard { id: id.clone(), label: label.into(), provider_id: provider_id.into(), model_id: model_id.into() });
        Ok(id)
    }

    /// The agent refuses removal of a routed model, so its routes are cleared in the same change.
    pub fn remove_model(&mut self, card_id: &str) -> Vec<Purpose> {
        let cleared: Vec<Purpose> = self.draft.selections.iter().filter(|s| s.model == card_id).map(|s| s.purpose).collect();
        self.draft.selections.retain(|s| s.model != card_id);
        self.draft.models.retain(|m| m.id != card_id);
        cleared
    }

    pub fn discard(&mut self) {
        if let Some(s) = &self.snapshot {
            self.draft = s.configuration.clone();
        }
        self.status = Some(Status::Ready);
        self.message = None;
    }

    pub fn start_save(&mut self) -> Option<(u64, ModelConfiguration)> {
        if !self.is_dirty() || self.status() == Status::Saving {
            return None;
        }
        self.status = Some(Status::Saving);
        Some((self.revision()?, self.draft.clone()))
    }

    /// Unknown transport outcomes are not retried: the caller reloads instead.
    pub fn finish_save(&mut self, result: Result<ModelSettings, ClientError>) {
        match result {
            Ok(s) => {
                self.message = None;
                self.loaded(Ok(s));
            }
            Err(e) if e == ClientError::Daemon("stale_model_settings".into()) => {
                self.status = Some(Status::Conflict);
                self.message = Some(e.message());
            }
            Err(e) => {
                self.status = Some(Status::Ready);
                self.message = Some(e.message());
            }
        }
    }

    pub fn credential(&self, provider_id: &str) -> Option<&ProviderCredential> {
        self.credentials.get(provider_id)
    }

    /// Accepts a credential answer only for the revision currently shown.
    pub fn credential_loaded(&mut self, provider_id: &str, result: Result<ProviderCredential, ClientError>) {
        match result {
            Ok(c) if Some(c.settings_revision) == self.revision() => {
                // Generations only grow; an older answer never replaces a newer one.
                if self.credentials.get(provider_id).is_some_and(|known| known.generation > c.generation) {
                    return;
                }
                self.credentials.insert(provider_id.into(), c);
                if !self.discovery_fresh(provider_id) {
                    self.discovered.remove(provider_id);
                }
            }
            Ok(_) => {}
            Err(e) => self.message = Some(e.message()),
        }
    }

    pub fn credential_label(&self, provider_id: &str) -> String {
        match self.credentials.get(provider_id) {
            None => tx("密钥：读取中…").into(),
            Some(c) => match c.state {
                CredentialState::Missing => tx("密钥：未保存").into(),
                CredentialState::Saved => txf("密钥：已保存 {0}", &[&c.masked_key.as_deref().unwrap_or("***")]),
                CredentialState::ScopeChanged => tx("密钥：地址已变更，需要重新输入").into(),
            },
        }
    }

    /// Late discovery for another revision or credential generation is dropped.
    pub fn discovery_loaded(&mut self, provider_id: &str, result: Result<ProviderModels, ClientError>) {
        match result {
            Ok(m) => {
                let previous = self.discovered.insert(provider_id.into(), m);
                // A late answer for an older scope is dropped and the prior result kept.
                if !self.discovery_fresh(provider_id) {
                    match previous {
                        Some(p) => {
                            self.discovered.insert(provider_id.into(), p);
                        }
                        None => {
                            self.discovered.remove(provider_id);
                        }
                    }
                }
            }
            Err(e) => self.message = Some(e.message()),
        }
    }

    /// A discovery result still describes the current scope: same settings revision,
    /// same endpoint as the draft, and — when made with the saved key — that exact
    /// key generation still saved. A credentialless result used no key, so it is
    /// bound to the endpoint and revision only.
    fn discovery_fresh(&self, provider_id: &str) -> bool {
        let Some(d) = self.discovered.get(provider_id) else { return false };
        let base_ok = self.draft.providers.iter().any(|p| p.id == provider_id && p.api_base == d.api_base);
        let key_ok = match d.credential_generation {
            None => true,
            Some(g) => self.credentials.get(provider_id).is_some_and(|c| c.state == CredentialState::Saved && c.generation == g),
        };
        Some(d.settings_revision) == self.revision() && base_ok && key_ok
    }

    pub fn discovered(&self, provider_id: &str) -> Option<&ProviderModels> {
        self.discovered.get(provider_id).filter(|_| self.discovery_fresh(provider_id))
    }

    pub fn source(&self, card: &ModelCard) -> ModelSource {
        match self.discovered(&card.provider_id) {
            Some(d) if d.model_ids.iter().any(|id| id == &card.model_id) => ModelSource::Discovered,
            _ => ModelSource::ManualUnverified,
        }
    }
}

fn slug(text: &str, fallback: &str) -> String {
    let s: String = text.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c.to_ascii_lowercase() } else { '-' }).collect();
    let s = s.trim_matches('-').chars().take(60).collect::<String>();
    if s.is_empty() { fallback.into() } else { s }
}

fn unique_id<'a>(base: &str, taken: impl Iterator<Item = &'a str>) -> String {
    let taken: Vec<&str> = taken.collect();
    if !taken.contains(&base) {
        return base.into();
    }
    (2..).map(|n| format!("{base}-{n}")).find(|c| !taken.contains(&c.as_str())).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use memex_client::{fixture::FixtureBackend, Client, DiscoverAuthorization, Secret};

    fn ready(client: &Client) -> SettingsModel {
        let mut m = SettingsModel::default();
        m.loaded(client.model_settings());
        m
    }

    fn save(client: &Client, m: &mut SettingsModel) {
        let (rev, cfg) = m.start_save().expect("dirty");
        m.finish_save(client.replace_model_settings(rev, cfg));
    }

    #[test]
    fn save_is_cas_and_a_conflict_keeps_the_draft() {
        let client = Client::new(FixtureBackend::new(1));
        let mut a = ready(&client);
        let mut b = ready(&client);
        a.add_provider("合成代理", "http://127.0.0.1:18317/v1").unwrap();
        save(&client, &mut a);
        assert_eq!(a.revision(), Some(1));
        assert_eq!(a.draft().providers[0].api_base, "http://127.0.0.1:18317/v1/");
        b.add_provider("另一处", "https://example.invalid/v1/").unwrap();
        save(&client, &mut b);
        assert_eq!(b.status(), Status::Conflict);
        assert_eq!(b.draft().providers[0].label, "另一处", "draft must survive a conflict");
        assert_eq!(client.model_settings().unwrap().configuration.providers[0].label, "合成代理");
        b.loaded(client.model_settings());
        assert_eq!((b.status(), b.message()), (Status::Ready, None), "reload resolves the conflict");
    }

    #[test]
    fn removing_a_routed_model_clears_its_route_in_the_same_change() {
        let client = Client::new(FixtureBackend::new(1));
        let mut m = ready(&client);
        m.add_provider("合成代理", "http://127.0.0.1:18317/v1/").unwrap();
        let p = m.draft().providers[0].id.clone();
        let card = m.add_model(&p, "Astra", "gpt-6-astra").unwrap();
        m.select(Purpose::Chat, Some(card.clone()));
        save(&client, &mut m);
        assert_eq!(m.remove_model(&card), vec![Purpose::Chat]);
        save(&client, &mut m);
        assert_eq!(m.status(), Status::Ready, "{:?}", m.message());
        assert!(m.selection(Purpose::Chat).is_none());
    }

    #[test]
    fn discovery_marks_sources_and_late_results_are_dropped() {
        let client = Client::new(FixtureBackend::new(1));
        let mut m = ready(&client);
        m.add_provider("合成代理", "http://127.0.0.1:18317/v1/").unwrap();
        let p = m.draft().providers[0].id.clone();
        let listed = m.add_model(&p, "Alpha", "fixture-model-alpha").unwrap();
        m.add_model(&p, "手填", "gpt-6.1-terra").unwrap();
        save(&client, &mut m);
        let rev = m.revision().unwrap();
        m.credential_loaded(&p, client.provider_credential(rev, &p));
        let key = client.replace_provider_credential(rev, &p, 0, Secret::new("sk-synthetic-xyz".into())).unwrap();
        assert_eq!(key.masked_key.as_deref(), Some("***xyz"));
        m.credential_loaded(&p, Ok(key.clone()));
        let found = client.discover_models(rev, &p, DiscoverAuthorization::SavedKey { expected_generation: key.generation });
        // A newer revision lands before the discovery answer: it must be ignored.
        let mut late = SettingsModel::default();
        late.loaded(client.model_settings());
        late.select(Purpose::Chat, Some(listed.clone()));
        let (r, c) = late.start_save().unwrap();
        late.finish_save(client.replace_model_settings(r, c));
        late.discovery_loaded(&p, found.clone());
        assert!(late.discovered(&p).is_none());
        // On the matching revision it is kept and only advertised IDs count as listed.
        m.discovery_loaded(&p, found);
        let cards = m.draft().models.clone();
        assert_eq!(m.source(&cards[0]), ModelSource::Discovered);
        assert_eq!(m.source(&cards[1]), ModelSource::ManualUnverified);
    }

    #[test]
    fn changing_the_address_invalidates_the_saved_key_and_removal_needs_a_clear() {
        let client = Client::new(FixtureBackend::new(1));
        let mut m = ready(&client);
        m.add_provider("合成代理", "http://127.0.0.1:18317/v1/").unwrap();
        let p = m.draft().providers[0].id.clone();
        save(&client, &mut m);
        let key = client.replace_provider_credential(1, &p, 0, Secret::new("ab".into())).unwrap();
        assert_eq!(key.masked_key.as_deref(), Some("***"), "short keys are fully masked");
        m.credential_loaded(&p, Ok(key));
        assert!(m.remove_provider(&p).is_err());
        m.set_provider_base(&p, "http://127.0.0.1:18318/v1/");
        save(&client, &mut m);
        m.credential_loaded(&p, client.provider_credential(m.revision().unwrap(), &p));
        assert_eq!(m.credential_label(&p), "密钥：地址已变更，需要重新输入");
        assert!(format!("{:?}", Secret::new("sk-secret".into())).contains("redacted"));
    }

    #[test]
    fn discovery_is_bound_to_the_credential_generation_it_used() {
        let client = Client::new(FixtureBackend::new(1));
        let mut m = ready(&client);
        m.add_provider("合成代理", "http://127.0.0.1:18317/v1/").unwrap();
        let p = m.draft().providers[0].id.clone();
        m.add_model(&p, "Alpha", "fixture-model-alpha").unwrap();
        m.select(Purpose::Chat, Some(m.draft().models[0].id.clone()));
        save(&client, &mut m);
        let rev = m.revision().unwrap();
        let g1 = client.replace_provider_credential(rev, &p, 0, Secret::new("key-one-aaa".into())).unwrap();
        m.credential_loaded(&p, Ok(g1.clone()));
        let found_g1 = client.discover_models(rev, &p, DiscoverAuthorization::SavedKey { expected_generation: g1.generation });
        m.discovery_loaded(&p, found_g1.clone());
        let card = m.draft().models[0].clone();
        assert_eq!(m.source(&card), ModelSource::Discovered);

        // Replacing the key (same settings revision) invalidates the g1 discovery.
        let g2 = client.replace_provider_credential(rev, &p, g1.generation, Secret::new("key-two-bbb".into())).unwrap();
        m.credential_loaded(&p, Ok(g2.clone()));
        assert!(m.discovered(&p).is_none(), "discovery made with generation 1 must not survive generation 2");
        assert_eq!(m.source(&card), ModelSource::ManualUnverified);
        // A g1 discovery answer arriving late is dropped too.
        m.discovery_loaded(&p, found_g1.clone());
        assert!(m.discovered(&p).is_none());
        // An older credential answer must not overwrite the newer generation.
        m.credential_loaded(&p, Ok(g1.clone()));
        assert_eq!(m.credential(&p).map(|c| c.generation), Some(g2.generation));
        // Card and route are kept; nothing was fetched automatically.
        assert_eq!(m.draft().models.len(), 1);
        assert!(m.selection(Purpose::Chat).is_some());

        // Clearing the key also invalidates a saved-key discovery.
        let found_g2 = client.discover_models(rev, &p, DiscoverAuthorization::SavedKey { expected_generation: g2.generation });
        m.discovery_loaded(&p, found_g2);
        assert!(m.discovered(&p).is_some());
        let g3 = client.clear_provider_credential(rev, &p, g2.generation).unwrap();
        m.credential_loaded(&p, Ok(g3.clone()));
        assert!(m.discovered(&p).is_none());

        // Credentialless discovery used no key: it is bound to the endpoint and
        // settings revision, so a later key change does not void it, but an
        // address change (new revision / api_base) does.
        let open = client.discover_models(rev, &p, DiscoverAuthorization::Credentialless);
        m.discovery_loaded(&p, open);
        assert!(m.discovered(&p).is_some());
        let g4 = client.replace_provider_credential(rev, &p, g3.generation, Secret::new("key-four-ddd".into())).unwrap();
        m.credential_loaded(&p, Ok(g4));
        assert!(m.discovered(&p).is_some(), "credentialless result does not depend on the key");
        m.set_provider_base(&p, "http://127.0.0.1:18318/v1/");
        assert!(m.discovered(&p).is_none(), "an edited address no longer matches the discovered endpoint");
    }

    #[test]
    fn effort_follows_its_route_and_round_trips_through_a_save() {
        let client = Client::new(FixtureBackend::new(1));
        let mut m = ready(&client);
        m.add_provider("合成代理", "http://127.0.0.1:18317/v1/").unwrap();
        let p = m.draft().providers[0].id.clone();
        let astra = m.add_model(&p, "Astra", "gpt-6-astra").unwrap();
        let terra = m.add_model(&p, "Terra", "gpt-6.1-terra").unwrap();
        // No route yet: effort has nowhere to live.
        m.set_reasoning_effort(Purpose::Translation, Some("low"));
        assert_eq!(m.reasoning_effort(Purpose::Translation), None);
        m.select(Purpose::Translation, Some(astra.clone()));
        m.set_reasoning_effort(Purpose::Translation, Some("low"));
        m.select(Purpose::Classification, Some(terra.clone()));
        save(&client, &mut m);
        assert_eq!(m.status(), Status::Ready, "{:?}", m.message());
        let saved = client.model_settings().unwrap().configuration.selections;
        assert_eq!(saved.iter().map(|s| (s.purpose, s.reasoning_effort.as_deref())).collect::<Vec<_>>(), vec![(Purpose::Translation, Some("low")), (Purpose::Classification, None)]);
        // Switching the model keeps the purpose's effort; clearing the route drops it.
        m.select(Purpose::Translation, Some(terra));
        assert_eq!(m.reasoning_effort(Purpose::Translation), Some("low"));
        m.select(Purpose::Translation, None);
        m.select(Purpose::Translation, Some(astra));
        assert_eq!(m.reasoning_effort(Purpose::Translation), None);
    }

    #[test]
    fn an_effort_from_a_newer_agent_survives_an_unrelated_edit() {
        let mut m = SettingsModel::default();
        let configuration = ModelConfiguration {
            providers: vec![Provider { id: "p".into(), label: "P".into(), api_base: "http://127.0.0.1:1/v1/".into() }],
            models: vec![ModelCard { id: "a".into(), label: "A".into(), provider_id: "p".into(), model_id: "a".into() }],
            selections: vec![Selection { purpose: Purpose::Chat, model: "a".into(), reasoning_effort: Some("ultra".into()) }],
        };
        m.loaded(Ok(ModelSettings { schema_version: 1, revision: 3, configuration }));
        m.select(Purpose::Summary, Some("a".into()));
        let (_, cfg) = m.start_save().unwrap();
        assert_eq!(cfg.selections[0].reasoning_effort.as_deref(), Some("ultra"));
    }

    #[test]
    fn old_agent_reports_unavailable() {
        let fixture = FixtureBackend::new(1);
        fixture.set_controls(memex_client::fixture::FixtureControls { legacy_agent: true, ..Default::default() });
        let mut m = SettingsModel::default();
        m.loaded(Client::new(fixture).model_settings());
        assert!(matches!(m.status(), Status::Unavailable(ref s) if s.contains("旧版本")));
    }
}
