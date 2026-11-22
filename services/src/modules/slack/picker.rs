use std::{collections::BTreeSet, path::PathBuf, sync::Arc};

use pinyin::ToPinyinMulti;
use serde::Serialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::{disk::DiskCache, mailbox::UserProfile, service::SlackService};

#[derive(Serialize)]
pub(super) struct PickerItem {
    #[serde(flatten)]
    summary: Value,
    participants: Vec<Participant>,
    search_terms: BTreeSet<String>,
}

#[derive(Serialize)]
struct Participant {
    id: String,
    name: String,
    username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    avatar_path: Option<PathBuf>,
}

impl PickerItem {
    fn from_summary(mut summary: Value, users: &[UserProfile], disk: Option<&DiskCache>) -> Self {
        let find_user = |id: &str| {
            users
                .binary_search_by(|user| user.id.as_str().cmp(id))
                .ok()
                .map(|index| &users[index])
        };
        let mut search_terms = BTreeSet::new();
        let participants: Vec<_> = summary["participant_ids"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(|id| {
                let user = find_user(id);
                let name = user
                    .map(|user| user.name.as_str())
                    .unwrap_or_else(|| id.split_once('.').map_or(id, |(_, id)| id));
                add_terms(&mut search_terms, name);
                if let Some(user) = user {
                    for alias in [user.display_name.as_deref(), user.username.as_deref()]
                        .into_iter()
                        .flatten()
                    {
                        add_terms(&mut search_terms, alias);
                    }
                }
                Participant {
                    id: id.to_owned(),
                    name: name.to_owned(),
                    username: user.and_then(|user| user.username.clone()),
                    avatar_path: user.and_then(|user| disk?.cached_avatar(user.slack_id())),
                }
            })
            .collect();
        if summary["kind"] == "im"
            && let Some(user) = participants.first()
            && find_user(&user.id).is_some()
        {
            summary["title"] = json!(user.name);
        }
        if let Some(title) = summary["title"].as_str() {
            add_terms(&mut search_terms, title.trim_start_matches('#'));
        }
        if let Some(last) = summary["last_message"].as_object_mut() {
            let author = last.remove("author").unwrap_or_default();
            let name = author["id"]
                .as_str()
                .and_then(find_user)
                .map(|user| user.name.as_str())
                .or_else(|| author["name"].as_str());
            last.insert("author_name".into(), json!(name));
        }
        Self {
            summary,
            participants,
            search_terms,
        }
    }
}

fn add_terms(terms: &mut BTreeSet<String>, text: &str) {
    let text = text.trim().to_lowercase();
    if text.is_empty() {
        return;
    }
    let mut full = vec![String::new()];
    let mut initials = vec![String::new()];
    let mut has_pinyin = false;
    for character in text.chars().filter(|c| !c.is_whitespace()) {
        if let Some(readings) = character.to_pinyin_multi() {
            has_pinyin = true;
            let readings: Vec<_> = readings.into_iter().map(|pinyin| pinyin.plain()).collect();
            full = full
                .iter()
                .flat_map(|prefix| readings.iter().map(move |sound| format!("{prefix}{sound}")))
                .take(64)
                .collect();
            let initials_for_character: BTreeSet<_> = readings
                .iter()
                .filter_map(|sound| sound.chars().next())
                .collect();
            initials = initials
                .iter()
                .flat_map(|prefix| {
                    initials_for_character
                        .iter()
                        .map(move |first| format!("{prefix}{first}"))
                })
                .take(64)
                .collect();
        } else {
            for alias in full.iter_mut().chain(initials.iter_mut()) {
                alias.push(character);
            }
        }
    }
    terms.insert(text);
    if has_pinyin {
        terms.extend(full);
        terms.extend(initials);
    }
}

impl SlackService {
    pub(super) async fn refreshing_picker(&self) {
        loop {
            self.picker_changed.notified().await;
            let mut state = self.state.lock().await;
            let Some(session) = state.cache.session_id else {
                continue;
            };
            let Ok(account) = state.account(session) else {
                continue;
            };
            let users = state
                .user_directory
                .as_ref()
                .map_or(&[][..], |directory| &directory.users);
            let items: Arc<[PickerItem]> = state
                .cache
                .conversations
                .values()
                .map(|room| {
                    PickerItem::from_summary(
                        self.conversation_summary(account, room).2,
                        users,
                        state.cache.disk.as_ref(),
                    )
                })
                .collect();
            state.picker_items = items;
        }
    }

    pub(super) async fn picker_failure(&self, session: Uuid) {
        let mut state = self.state.lock().await;
        if state.account(session).is_ok() {
            state.picker_failed = true;
        }
    }

    pub(super) async fn picker_workspace(
        &self,
        id: &str,
        fallback_name: &str,
    ) -> (Value, Arc<[PickerItem]>) {
        let state = self.state.lock().await;
        let account = state
            .cache
            .session_id
            .and_then(|session| state.account(session).ok());
        let name = account.map_or(fallback_name, |account| &account.identity.team_name);
        let ready = state.cache.directory_loaded
            && state
                .user_directory
                .as_ref()
                .is_some_and(|directory| directory.complete())
            && state.picker_items.len() == state.cache.conversations.len()
            && state
                .picker_items
                .iter()
                .all(|item| item.summary["history_loaded"] == true);
        let status = if account.is_some() && ready {
            "ready"
        } else if (account.is_none() && state.issue.is_some())
            || state.picker_failed
            || state.retired
            || state.secrets.disabled_token.is_some()
        {
            "unavailable"
        } else {
            "warming_up"
        };
        let items = if account.is_some() {
            state.picker_items.clone()
        } else {
            Arc::default()
        };
        (json!({"team_id":id,"team_name":name,"state":status}), items)
    }
}
