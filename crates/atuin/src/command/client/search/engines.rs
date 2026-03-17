use async_trait::async_trait;
use atuin_client::{
    database::{Context, Database},
    history::{History, HistoryId},
    settings::{FilterMode, SearchMode, Settings},
};
use eyre::Result;

use super::cursor::Cursor;

pub mod db;
pub mod nucleo;
pub mod skim;

pub fn engine(search_mode: SearchMode, settings: &Settings) -> Box<dyn SearchEngine> {
    match search_mode {
        SearchMode::Skim => Box::new(skim::Search::new(settings)) as Box<_>,
        SearchMode::Nucleo => Box::new(nucleo::Search::new(settings)) as Box<_>,
        mode => Box::new(db::Search(mode)) as Box<_>,
    }
}

pub struct SearchState {
    pub input: Cursor,
    pub filter_mode: FilterMode,
    pub context: Context,
    pub custom_context: Option<HistoryId>,
}

impl SearchState {
    pub(crate) fn rotate_filter_mode(&mut self, settings: &Settings, offset: isize) {
        let mut i = settings
            .search
            .filters
            .iter()
            .position(|&m| m == self.filter_mode)
            .unwrap_or_default();
        for _ in 0..settings.search.filters.len() {
            i = (i.wrapping_add_signed(offset)) % settings.search.filters.len();
            let mode = settings.search.filters[i];
            if self.filter_mode_available(mode, settings) {
                self.filter_mode = mode;
                break;
            }
        }
    }

    fn filter_mode_available(&self, mode: FilterMode, settings: &Settings) -> bool {
        match mode {
            FilterMode::Global | FilterMode::SessionPreload => self.custom_context.is_none(),
            FilterMode::Workspace => settings.workspaces && self.context.git_root.is_some(),
            _ => true,
        }
    }
}

#[async_trait]
pub trait SearchEngine: Send + Sync + 'static {
    async fn preload(&mut self, _db: &dyn Database) -> Result<bool> {
        Ok(false)
    }

    async fn full_query(
        &mut self,
        state: &SearchState,
        db: &mut dyn Database,
    ) -> Result<Vec<History>>;

    fn is_loading(&self) -> bool {
        false
    }

    async fn query(&mut self, state: &SearchState, db: &mut dyn Database) -> Result<Vec<History>> {
        if state.input.as_str().is_empty() {
            Ok(db
                .list(&[state.filter_mode], &state.context, Some(200), true, false)
                .await?
                .into_iter()
                .collect::<Vec<_>>())
        } else {
            self.full_query(state, db).await
        }
    }
    fn get_highlight_indices(&self, command: &str, search_input: &str) -> Vec<usize>;
}
