use std::collections::{HashMap, HashSet};
use std::ops::Range;

use crate::assistant::repository::{self, CreateMessageParams};
use crate::assistant::token_estimate::message_tokens;
use crate::assistant::types::{AssistantCompaction, AssistantMessage, ContentPart, MessageRole};
use crate::db::DbPool;

pub const COMPACTION_METADATA_SOURCE: &str = "clai-compaction";

pub(crate) fn is_compaction_summary_message(message: &AssistantMessage) -> bool {
    message
        .provider_metadata
        .as_ref()
        .and_then(|metadata| metadata.get("source"))
        .and_then(|value| value.as_str())
        == Some(COMPACTION_METADATA_SOURCE)
}

/// The conversation one run sends to the provider, owned in memory for the
/// whole run: `[standing summary] + the messages it does not cover`, in
/// insertion order (`created_at`, then rowid), with tool results placed by
/// their issuing assistant. It is loaded once when the run starts; from then
/// on the run writes its rows through it and feeds it queue rows at request
/// boundaries, so history is never re-read while the run is active.
/// Nothing here is durable: the next run reconstructs the same view from
/// persistence.
pub struct RunConversation {
    messages: Vec<AssistantMessage>,
    /// `message_tokens` of every row in `messages`, by id. Tokenizing a full
    /// conversation costs tens of milliseconds and the trigger runs on every
    /// engine iteration, so counts are kept from the writes instead.
    tokens: HashMap<String, usize>,
    /// Queued user rows still pending delivery, reconciled against the queue
    /// at request boundaries.
    pending_ids: HashSet<String>,
}

impl RunConversation {
    /// The one history read of a run: every message plus the latest completed
    /// compaction, projected onto the provider view.
    pub async fn load(pool: &DbPool, session_id: &str) -> Result<Self, String> {
        let messages = repository::list_messages(pool, session_id).await?;
        let latest = repository::latest_completed_compaction(pool, session_id).await?;
        Ok(Self::from_history(&messages, latest.as_ref()))
    }

    pub(crate) fn from_history(
        messages: &[AssistantMessage],
        latest: Option<&AssistantCompaction>,
    ) -> Self {
        let messages = provider_view(join_tool_groups(messages.to_vec()), latest);
        Self {
            tokens: messages
                .iter()
                .map(|message| (message.id.clone(), message_tokens(message)))
                .collect(),
            messages,
            pending_ids: HashSet::new(),
        }
    }

    #[cfg(test)]
    pub(crate) fn from_provider_view_for_test(messages: Vec<AssistantMessage>) -> Self {
        Self {
            tokens: messages
                .iter()
                .map(|message| (message.id.clone(), message_tokens(message)))
                .collect(),
            messages,
            pending_ids: HashSet::new(),
        }
    }

    #[cfg(test)]
    pub(crate) fn token_cache_for_test(&self) -> Vec<(&str, usize)> {
        self.tokens
            .iter()
            .map(|(id, tokens)| (id.as_str(), *tokens))
            .collect()
    }

    pub(crate) fn cached_message_tokens(&self, message: &AssistantMessage) -> usize {
        self.tokens
            .get(&message.id)
            .copied()
            .unwrap_or_else(|| message_tokens(message))
    }

    pub(crate) fn is_pending(&self, message_id: &str) -> bool {
        self.pending_ids.contains(message_id)
    }

    pub fn messages(&self) -> &[AssistantMessage] {
        &self.messages
    }

    /// Estimated tokens of the whole conversation as the provider receives it.
    pub(crate) fn estimated_tokens(&self) -> usize {
        self.tokens.values().sum()
    }

    /// The standing summary; `provider_view` places at most one, first.
    pub fn summary(&self) -> Option<&AssistantMessage> {
        self.messages
            .first()
            .filter(|message| is_compaction_summary_message(message))
    }

    /// Messages after the standing summary.
    pub fn tail(&self) -> &[AssistantMessage] {
        &self.messages[self.summary_len()..]
    }

    pub fn latest_user_message(&self) -> Option<&AssistantMessage> {
        self.messages
            .iter()
            .rev()
            .find(|message| message.role == MessageRole::User)
    }

    /// Persist a new row and record it. Every conversation write a run makes
    /// goes through this, the two methods below, or `upsert` right after the
    /// repository call, so a row cannot reach the database without reaching
    /// the view the provider sees.
    pub async fn create_message(
        &mut self,
        pool: &DbPool,
        params: CreateMessageParams,
    ) -> Result<AssistantMessage, String> {
        let message = repository::create_message(pool, params).await?;
        self.upsert(message.clone());
        Ok(message)
    }

    pub async fn update_message_content(
        &mut self,
        pool: &DbPool,
        message_id: &str,
        content: &[ContentPart],
    ) -> Result<AssistantMessage, String> {
        let message = repository::update_message_content(pool, message_id, content).await?;
        self.upsert(message.clone());
        Ok(message)
    }

    pub async fn delete_message(&mut self, pool: &DbPool, message_id: &str) -> Result<(), String> {
        repository::delete_message(pool, message_id).await?;
        self.remove(message_id);
        Ok(())
    }

    /// Record a persisted row: replace an existing row with the same id
    /// (assistant placeholders are finalized in place), otherwise insert it
    /// after every row created at the same time or earlier, which is the
    /// insertion order `list_messages` returns, and re-join tool groups the
    /// same way `from_history` does.
    pub fn upsert(&mut self, message: AssistantMessage) {
        self.tokens
            .insert(message.id.clone(), message_tokens(&message));
        if let Some(existing) = self.messages.iter_mut().find(|m| m.id == message.id) {
            *existing = message;
            return;
        }
        let start = self.summary_len();
        let offset = self.messages[start..]
            .iter()
            .rposition(|m| m.created_at <= message.created_at)
            .map_or(0, |index| index + 1);
        self.messages.insert(start + offset, message);
        self.messages = join_tool_groups(std::mem::take(&mut self.messages));
    }

    fn remove(&mut self, message_id: &str) {
        self.messages.retain(|message| message.id != message_id);
        self.tokens.remove(message_id);
        self.pending_ids.remove(message_id);
    }

    /// Reconcile with the queue's current pending rows: edits replace the
    /// row by id, newcomers are inserted, and a previously pending row that
    /// is neither pending nor delivered by this run was deleted by the user.
    pub fn refresh_pending(&mut self, pending: Vec<AssistantMessage>) {
        let fresh: HashSet<String> = pending.iter().map(|message| message.id.clone()).collect();
        let deleted: Vec<String> = self
            .messages
            .iter()
            .filter(|message| {
                self.pending_ids.contains(&message.id) && !fresh.contains(&message.id)
            })
            .map(|message| message.id.clone())
            .collect();
        for id in &deleted {
            self.remove(id);
        }
        for message in pending {
            self.upsert(message);
        }
        self.pending_ids = fresh;
    }

    /// Pending queued rows in this conversation, in view order.
    pub fn pending_ids(&self) -> Vec<String> {
        self.messages
            .iter()
            .filter(|message| self.pending_ids.contains(&message.id))
            .map(|message| message.id.clone())
            .collect()
    }

    pub fn mark_delivered(&mut self, message_ids: &[String]) {
        for id in message_ids {
            self.pending_ids.remove(id);
        }
    }

    fn summary_len(&self) -> usize {
        usize::from(self.summary().is_some())
    }

    pub(crate) fn apply_compaction(&mut self, consumed: usize, summary_message: AssistantMessage) {
        for message in self.messages.drain(..consumed) {
            self.tokens.remove(&message.id);
        }
        self.tokens
            .insert(summary_message.id.clone(), message_tokens(&summary_message));
        self.messages.insert(0, summary_message);
    }
}

/// `[latest valid summary] + non-summary messages after its boundary`, or
/// every non-summary message when the compaction row cannot be placed
/// (referenced rows deleted or belonging to another session). `messages`
/// must be in view order (see [`join_tool_groups`]): the boundary is the
/// last message a compaction consumed from that order.
fn provider_view(
    messages: Vec<AssistantMessage>,
    latest: Option<&AssistantCompaction>,
) -> Vec<AssistantMessage> {
    let summary_and_boundary = latest.and_then(|compaction| {
        let summary = messages.iter().find(|message| {
            Some(message.id.as_str()) == compaction.summary_message_id.as_deref()
        })?;
        let boundary = messages.iter().position(|message| {
            Some(message.id.as_str()) == compaction.source_to_message_id.as_deref()
        })?;
        Some((summary.clone(), boundary + 1))
    });
    let (summary, boundary) = summary_and_boundary.unzip();
    summary
        .into_iter()
        .chain(
            messages
                .into_iter()
                .skip(boundary.unwrap_or(0))
                .filter(|message| !is_compaction_summary_message(message)),
        )
        .collect()
}

/// Move tool results behind the assistant that issued each call. A later
/// assistant can appear before a result, so call ownership must survive
/// intervening messages. Idempotent on an already joined sequence.
pub(crate) fn join_tool_groups(messages: Vec<AssistantMessage>) -> Vec<AssistantMessage> {
    let mut groups: Vec<Vec<AssistantMessage>> = Vec::with_capacity(messages.len());
    let mut issuers: HashMap<String, usize> = HashMap::new();
    for message in messages {
        if message.role == MessageRole::Tool {
            let results = tool_result_ids(&message);
            if let Some(&group) = results.first().and_then(|id| issuers.get(*id)) {
                if results.iter().all(|id| issuers.get(*id) == Some(&group)) {
                    groups[group].push(message);
                    continue;
                }
            }
        }
        let group = groups.len();
        for id in tool_use_ids(&message) {
            issuers.insert(id.to_string(), group);
        }
        groups.push(vec![message]);
    }
    groups.into_iter().flatten().collect()
}

/// Partition messages into the groups a compaction cut must not split: an
/// assistant message together with the tool messages answering the calls it
/// issued. Everything else is its own group. [`join_tool_groups`] places
/// results right behind their call, so each group is a contiguous range.
pub(crate) fn tool_groups(messages: &[AssistantMessage]) -> Vec<Range<usize>> {
    let mut groups = Vec::new();
    let mut start = 0usize;
    while start < messages.len() {
        let mut end = start + 1;
        let issued = tool_use_ids(&messages[start]);
        if !issued.is_empty() {
            while end < messages.len() && messages[end].role == MessageRole::Tool {
                let answered = tool_result_ids(&messages[end]);
                if answered.is_empty() || !answered.iter().all(|id| issued.contains(id)) {
                    break;
                }
                end += 1;
            }
        }
        groups.push(start..end);
        start = end;
    }
    groups
}

fn tool_use_ids(message: &AssistantMessage) -> HashSet<&str> {
    message
        .content
        .iter()
        .filter_map(|part| match part {
            ContentPart::ToolUse { tool_call_id, .. } => Some(tool_call_id.as_str()),
            _ => None,
        })
        .collect()
}

fn tool_result_ids(message: &AssistantMessage) -> Vec<&str> {
    message
        .content
        .iter()
        .filter_map(|part| match part {
            ContentPart::ToolResult { tool_call_id, .. } => Some(tool_call_id.as_str()),
            _ => None,
        })
        .collect()
}

pub fn content_text(content: &[ContentPart]) -> String {
    content
        .iter()
        .filter_map(|part| match part {
            ContentPart::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}
