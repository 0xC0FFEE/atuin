use std::path::Path;

use async_trait::async_trait;
use atuin_client::{
    database::{Database, all_with_count_rusqlite},
    history::History,
    settings::{FilterMode, Settings},
};
use eyre::Result;
use fuzzy_matcher::{FuzzyMatcher, skim::SkimMatcherV2};
use itertools::Itertools;
use time::OffsetDateTime;
use tokio::task::JoinHandle;
use tokio::task::yield_now;
use tracing::{Level, info, instrument, warn};
use uuid;

use super::{SearchEngine, SearchState};

pub struct Search {
    all_history: Vec<(History, i32)>,
    preload: Option<JoinHandle<Vec<(History, i32)>>>,
    preload_db_path: String,
    preload_timeout_secs: f64,
    engine: SkimMatcherV2,
}

impl Search {
    pub fn new(settings: &Settings) -> Self {
        Search {
            all_history: vec![],
            preload: None,
            preload_db_path: settings.db_path.clone(),
            preload_timeout_secs: settings.local_timeout,
            engine: SkimMatcherV2::default(),
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
            info!(target: "atuin::search_perf", event = "engine_preload_started", engine = "skim");
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
                engine = "skim",
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

    #[instrument(skip_all, level = Level::TRACE, name = "skim_search", fields(query = %state.input.as_str()))]
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

        Ok(fuzzy_search(&self.engine, state, &self.all_history).await)
    }

    #[instrument(skip_all, level = Level::TRACE, name = "skim_highlight")]
    fn get_highlight_indices(&self, command: &str, search_input: &str) -> Vec<usize> {
        let (_, indices) = self
            .engine
            .fuzzy_indices(command, search_input)
            .unwrap_or_default();
        indices
    }

    fn is_loading(&self) -> bool {
        self.preload.is_some()
    }
}

#[allow(clippy::too_many_lines)]
#[instrument(skip_all, level = Level::TRACE, name = "fuzzy_match", fields(history_count = all_history.len()))]
async fn fuzzy_search(
    engine: &SkimMatcherV2,
    state: &SearchState,
    all_history: &[(History, i32)],
) -> Vec<History> {
    let mut set = Vec::new();
    let mut ranks = Vec::new();
    let query = state.input.as_str();
    let now = OffsetDateTime::now_utc();

    for (i, (history, count)) in all_history.iter().enumerate() {
        if i % 256 == 0 {
            yield_now().await;
        }
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
        #[allow(clippy::cast_lossless, clippy::cast_precision_loss)]
        if let Some((score, indices)) = engine.fuzzy_indices(&history.command, query) {
            let begin = indices.first().copied().unwrap_or_default();

            let mut duration = (now - history.timestamp).as_seconds_f64().log2();
            if !duration.is_finite() || duration <= 1.0 {
                duration = 1.0;
            }
            // these + X.0 just make the log result a bit smoother.
            // log is very spiky towards 1-4, but I want a gradual decay.
            // eg:
            // log2(4) = 2, log2(5) = 2.3 (16% increase)
            // log2(8) = 3, log2(9) = 3.16 (5% increase)
            // log2(16) = 4, log2(17) = 4.08 (2% increase)
            let count = (*count as f64 + 8.0).log2();
            let begin = (begin as f64 + 16.0).log2();
            let path = path_dist(history.cwd.as_ref(), state.context.cwd.as_ref());
            let path = (path as f64 + 8.0).log2();

            // reduce longer durations, raise higher counts, raise matches close to the start
            let score = (-score as f64) * count / path / duration / begin;

            'insert: {
                // algorithm:
                // 1. find either the position that this command ranks
                // 2. find the same command positioned better than our rank.
                for i in 0..set.len() {
                    // do we out score the current position?
                    if ranks[i] > score {
                        ranks.insert(i, score);
                        set.insert(i, history.clone());
                        let mut j = i + 1;
                        while j < set.len() {
                            // remove duplicates that have a worse score
                            if set[j].command == history.command {
                                ranks.remove(j);
                                set.remove(j);

                                // break this while loop because there won't be any other
                                // duplicates.
                                break;
                            }
                            j += 1;
                        }

                        break 'insert;
                    }
                    // don't continue if this command has a better score already
                    if set[i].command == history.command {
                        break 'insert;
                    }
                }
                ranks.push(score);
                set.push(history.clone());
            }
        }
    }

    set
}

fn path_dist(a: &Path, b: &Path) -> usize {
    let mut a: Vec<_> = a.components().collect();
    let b: Vec<_> = b.components().collect();

    let mut dist = 0;

    // pop a until there's a common ancestor
    while !b.starts_with(&a) {
        dist += 1;
        a.pop();
    }

    b.len() - a.len() + dist
}
