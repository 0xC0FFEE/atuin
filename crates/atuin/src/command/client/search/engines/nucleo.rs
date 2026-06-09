use std::{cmp::Ordering, collections::HashMap, sync::Mutex};

use async_trait::async_trait;
use atuin_client::{
    database::{Context, Database, all_with_count_rusqlite},
    history::History,
    settings::{FilterMode, Settings},
};
use atuin_nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use atuin_nucleo_matcher::{Config, Matcher, Utf32Str};
use eyre::Result;
use itertools::Itertools;
use time::OffsetDateTime;
use tokio::task::JoinHandle;
use tracing::{Level, info, instrument, warn};
use uuid;

use super::{SearchEngine, SearchState};

// One boundary bonus in fzf/nucleo is 8 points. Grouping by that amount keeps
// clear fuzzy wins ahead while letting history signals order near-equivalent matches.
const FUZZY_SCORE_BUCKET_SIZE: u32 = 8;

pub struct Search {
    all_history: Vec<(History, i32)>,
    preload: Option<JoinHandle<Vec<(History, i32)>>>,
    preload_db_path: String,
    preload_timeout_secs: f64,
    query_matcher: Matcher,
    highlight_matcher: Mutex<Matcher>,
}

impl Search {
    pub fn new(settings: &Settings) -> Self {
        let matcher = Matcher::new(Config::DEFAULT);
        Search {
            all_history: vec![],
            preload: None,
            preload_db_path: settings.db_path.clone(),
            preload_timeout_secs: settings.local_timeout,
            query_matcher: matcher.clone(),
            highlight_matcher: Mutex::new(matcher),
        }
    }

    fn start_preload(&mut self, db: &dyn Database) {
        if !self.all_history.is_empty() || self.preload.is_some() {
            return;
        }

        let db = db.clone_boxed();
        let db_path = self.preload_db_path.clone();
        let timeout_secs = self.preload_timeout_secs;
        self.preload = Some(tokio::task::spawn_blocking(move || {
            let started = std::time::Instant::now();
            info!(
                target: "atuin::search_perf",
                event = "engine_preload_started",
                engine = "nucleo"
            );
            let all_history =
                match all_with_count_rusqlite(std::path::Path::new(&db_path), timeout_secs) {
                    Ok(rows) => rows,
                    Err(_) => {
                        let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                        else {
                            return Vec::new();
                        };

                        runtime
                            .block_on(async move { db.all_with_count().await.unwrap_or_default() })
                    }
                };
            info!(
                target: "atuin::search_perf",
                event = "engine_preload_finished",
                engine = "nucleo",
                elapsed_ms = started.elapsed().as_millis() as u64,
                entry_count = all_history.len() as u64
            );
            all_history
        }));
    }

    async fn harvest_preload_if_ready(&mut self) -> bool {
        let Some(preload) = self.preload.as_ref() else {
            return false;
        };

        if !preload.is_finished() {
            return false;
        }

        let Some(preload) = self.preload.take() else {
            return false;
        };

        if let Ok(all_history) = preload.await {
            self.all_history = all_history;
            return true;
        }

        false
    }
}

#[async_trait]
impl SearchEngine for Search {
    async fn preload(&mut self, db: &dyn Database) -> Result<bool> {
        self.start_preload(db);
        Ok(self.harvest_preload_if_ready().await)
    }

    #[instrument(skip_all, level = Level::TRACE, name = "nucleo_search", fields(query = %state.input.as_str()))]
    async fn full_query(
        &mut self,
        state: &SearchState,
        db: &mut dyn Database,
    ) -> Result<Vec<History>> {
        self.harvest_preload_if_ready().await;
        if self.all_history.is_empty() {
            self.start_preload(db);
            return Ok(Vec::new());
        }

        Ok(fuzzy_search(
            &mut self.query_matcher,
            state,
            &self.all_history,
        ))
    }

    #[instrument(skip_all, level = Level::TRACE, name = "nucleo_highlight")]
    fn get_highlight_indices(&self, command: &str, search_input: &str) -> Vec<usize> {
        let mut indices = Vec::new();
        let pattern = Pattern::parse(search_input, CaseMatching::Smart, Normalization::Smart);
        let mut command_buf = Vec::new();
        let command = Utf32Str::new(command, &mut command_buf);

        let Ok(mut matcher) = self.highlight_matcher.lock() else {
            return Vec::new();
        };
        let Some(_) = pattern.indices(command, &mut matcher, &mut indices) else {
            return Vec::new();
        };
        indices.sort_unstable();
        indices.dedup();
        indices
            .into_iter()
            .filter_map(|index| usize::try_from(index).ok())
            .collect()
    }

    fn is_loading(&self) -> bool {
        self.preload.is_some()
    }
}

#[allow(clippy::too_many_lines)]
#[instrument(skip_all, level = Level::TRACE, name = "fuzzy_match", fields(history_count = all_history.len()))]
fn fuzzy_search(
    matcher: &mut Matcher,
    state: &SearchState,
    all_history: &[(History, i32)],
) -> Vec<History> {
    let mut matches = HashMap::new();
    let query = state.input.as_str();
    let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
    let now = OffsetDateTime::now_utc();

    let mut command_buf = Vec::new();
    let mut match_indices = Vec::new();

    for (history, count) in all_history {
        command_buf.clear();
        match_indices.clear();

        let context = &state.context;
        let git_root = context
            .git_root
            .as_ref()
            .and_then(|git_root| git_root.to_str())
            .unwrap_or(&context.cwd);
        match state.filter_mode {
            FilterMode::Global => {}
            // we aggregate host by ',' separating them
            FilterMode::Host
                if history
                    .hostname
                    .split(',')
                    .contains(&context.hostname.as_str()) => {}
            // we aggregate session by concattenating them.
            // sessions are 32 byte simple uuid formats
            FilterMode::Session
                if history
                    .session
                    .as_bytes()
                    .chunks(32)
                    .contains(&context.session.as_bytes()) => {}
            // SessionPreload: include current session + global history from before session start
            FilterMode::SessionPreload => {
                let is_current_session = {
                    history
                        .session
                        .as_bytes()
                        .chunks(32)
                        .any(|chunk| chunk == context.session.as_bytes())
                };

                if !is_current_session {
                    let Ok(uuid) = uuid::Uuid::parse_str(&context.session) else {
                        warn!("failed to parse session id '{}'", context.session);
                        continue;
                    };
                    let Some(timestamp) = uuid.get_timestamp() else {
                        warn!(
                            "failed to get timestamp from uuid '{}'",
                            uuid.as_hyphenated()
                        );
                        continue;
                    };
                    let (seconds, nanos) = timestamp.to_unix();
                    let Ok(session_start) = time::OffsetDateTime::from_unix_timestamp_nanos(
                        i128::from(seconds) * 1_000_000_000 + i128::from(nanos),
                    ) else {
                        warn!(
                            "failed to create OffsetDateTime from second: {seconds}, nanosecond: {nanos}"
                        );
                        continue;
                    };

                    if history.timestamp >= session_start {
                        continue;
                    }
                }
            }
            // we aggregate directory by ':' separating them
            FilterMode::Directory if history.cwd.split(':').contains(&context.cwd.as_str()) => {}
            FilterMode::Workspace if history.cwd.split(':').contains(&git_root) => {}
            _ => continue,
        }

        let command = Utf32Str::new(&history.command, &mut command_buf);
        #[allow(clippy::cast_lossless, clippy::cast_precision_loss)]
        if let Some(score) = pattern.indices(command, matcher, &mut match_indices) {
            match_indices.sort_unstable();
            match_indices.dedup();
            let scored = ScoredHistory::new(
                history.clone(),
                *count,
                score,
                now,
                locality_score(&history.cwd, &state.context),
            );
            matches
                .entry(history.command.clone())
                .and_modify(|best: &mut ScoredHistory| {
                    if compare_scored(&scored, best).is_lt() {
                        *best = scored.clone();
                    }
                })
                .or_insert(scored);
        }
    }

    let mut scored: Vec<_> = matches.into_values().collect();
    scored.sort_by(compare_scored);
    scored.into_iter().map(|scored| scored.history).collect()
}

#[derive(Clone)]
struct ScoredHistory {
    fuzzy_bucket: u32,
    fuzzy_score: u32,
    context_score: f64,
    count: i32,
    history: History,
}

impl ScoredHistory {
    fn new(
        history: History,
        count: i32,
        fuzzy_score: u32,
        now: OffsetDateTime,
        locality: f64,
    ) -> Self {
        let age_secs = (now - history.timestamp).as_seconds_f64();
        let age_secs = if age_secs.is_finite() && age_secs > 1.0 {
            age_secs
        } else {
            1.0
        };

        let recency = 6.0 / age_secs.log2().max(1.0);
        let frequency = (f64::from(count.max(0)) + 1.0).log2().min(8.0) * 0.35;
        Self {
            fuzzy_bucket: fuzzy_score / FUZZY_SCORE_BUCKET_SIZE,
            fuzzy_score,
            context_score: recency + frequency + locality,
            count,
            history,
        }
    }
}

fn locality_score(history_cwds: &str, context: &Context) -> f64 {
    let git_root = context.git_root.as_ref().and_then(|path| path.to_str());
    let mut in_workspace = false;

    for cwd in history_cwds.split(':') {
        if cwd == context.cwd {
            return 1.5;
        }

        if let Some(git_root) = git_root
            && (cwd == git_root
                || cwd
                    .strip_prefix(git_root)
                    .is_some_and(|suffix| suffix.starts_with('/')))
        {
            in_workspace = true;
        }
    }

    if in_workspace { 0.75 } else { 0.0 }
}

fn compare_scored(left: &ScoredHistory, right: &ScoredHistory) -> Ordering {
    right
        .fuzzy_bucket
        .cmp(&left.fuzzy_bucket)
        .then_with(|| right.context_score.total_cmp(&left.context_score))
        .then_with(|| right.fuzzy_score.cmp(&left.fuzzy_score))
        .then_with(|| right.history.timestamp.cmp(&left.history.timestamp))
        .then_with(|| right.count.cmp(&left.count))
        .then_with(|| left.history.command.len().cmp(&right.history.command.len()))
        .then_with(|| left.history.command.cmp(&right.history.command))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::client::search::cursor::Cursor;
    use atuin_client::database::Context;
    use std::path::PathBuf;
    use time::Duration;

    fn state(query: &str) -> SearchState {
        SearchState {
            input: Cursor::from(query.to_owned()),
            filter_mode: FilterMode::Global,
            context: Context {
                session: "session".to_owned(),
                cwd: "/work/project".to_owned(),
                hostname: "host".to_owned(),
                git_root: None,
            },
            custom_context: None,
        }
    }

    fn history(command: &str, timestamp: OffsetDateTime, cwd: &str) -> History {
        History {
            id: command.to_owned().into(),
            timestamp,
            duration: 0,
            exit: 0,
            command: command.to_owned(),
            cwd: cwd.to_owned(),
            session: "session".to_owned(),
            hostname: "host:user".to_owned(),
            author: "user".to_owned(),
            intent: None,
            deleted_at: None,
        }
    }

    #[test]
    fn locality_score_is_binary_for_cwd_and_workspace() {
        let context = Context {
            session: "session".to_owned(),
            cwd: "/work/project".to_owned(),
            hostname: "host".to_owned(),
            git_root: Some(PathBuf::from("/work/project")),
        };

        assert_eq!(locality_score("/tmp:/work/project", &context), 1.5);
        assert_eq!(locality_score("/work/project/crates/atuin", &context), 0.75);
        assert_eq!(locality_score("/work/projectile", &context), 0.0);
    }

    #[test]
    fn fuzzy_quality_wins_over_history_metadata() {
        let now = OffsetDateTime::now_utc();
        let all_history = vec![
            (
                history("c x a x r x g x o", now - Duration::SECOND, "/work/project"),
                10_000,
            ),
            (
                history("cargo test", now - Duration::days(90), "/unrelated/project"),
                1,
            ),
        ];

        let mut matcher = Matcher::new(Config::DEFAULT);
        let results = fuzzy_search(&mut matcher, &state("cargo"), &all_history);

        assert_eq!(results[0].command, "cargo test");
    }

    #[test]
    fn close_matches_prefer_recent_history() {
        let now = OffsetDateTime::now_utc();
        let all_history = vec![
            (
                history("git checkout", now - Duration::days(30), "/work/project"),
                1,
            ),
            (
                history("git commit", now - Duration::SECOND, "/work/project"),
                1,
            ),
        ];

        let mut matcher = Matcher::new(Config::DEFAULT);
        let results = fuzzy_search(&mut matcher, &state("git c"), &all_history);

        assert_eq!(results[0].command, "git commit");
    }
}
