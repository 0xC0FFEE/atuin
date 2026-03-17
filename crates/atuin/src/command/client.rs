use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};

use clap::Subcommand;
use eyre::{Result, WrapErr};

use atuin_client::{database::Sqlite, settings::Settings, theme};
use atuin_common::utils::uuid_v7;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::{
    Layer, filter::EnvFilter, filter::LevelFilter, fmt, fmt::format::FmtSpan, prelude::*,
};

fn cleanup_old_logs(log_dir: &Path, prefix: &str, retention_days: u64) {
    let cutoff = std::time::SystemTime::now()
        - std::time::Duration::from_secs(retention_days * 24 * 60 * 60);

    let Ok(entries) = fs::read_dir(log_dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };

        if !name.starts_with(prefix) || name == prefix {
            continue;
        }

        if let Ok(metadata) = entry.metadata()
            && let Ok(modified) = metadata.modified()
            && modified < cutoff
        {
            let _ = fs::remove_file(&path);
        }
    }
}

mod default_config;
mod doctor;
mod history;
mod import;
mod init;
mod search;
mod stats;

#[derive(Subcommand, Debug)]
#[command(infer_subcommands = true)]
pub enum Cmd {
    /// Manipulate shell history
    #[command(subcommand)]
    History(history::Cmd),

    /// Import shell history from file
    #[command(subcommand)]
    Import(import::Cmd),

    /// Calculate statistics for your history
    Stats(stats::Cmd),

    /// Interactive history search
    Search(search::Cmd),

    /// Print Atuin's shell init script
    #[command()]
    Init(init::Cmd),

    /// Run the doctor to check for common issues
    #[command()]
    Doctor,

    /// Print a new session UUID (compat for shell init scripts)
    #[command(hide = true)]
    Uuid,

    /// Print the default atuin configuration (config.toml)
    #[command()]
    DefaultConfig,
}

impl Cmd {
    pub fn run(self) -> Result<()> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        let settings = Settings::new().wrap_err("could not load client settings")?;
        let theme_manager = theme::ThemeManager::new(settings.theme.debug, None);
        let res = runtime.block_on(self.run_inner(settings, theme_manager));

        runtime.shutdown_timeout(std::time::Duration::from_millis(50));

        res
    }

    async fn run_inner(
        self,
        mut settings: Settings,
        mut theme_manager: theme::ThemeManager,
    ) -> Result<()> {
        let env_log_set = std::env::var("ATUIN_LOG").is_ok();

        let base_filter =
            EnvFilter::from_env("ATUIN_LOG").add_directive("sqlx_sqlite::regexp=off".parse()?);

        let is_interactive_search = matches!(&self, Self::Search(cmd) if cmd.is_interactive());
        let use_search_logging = is_interactive_search && settings.logs.search_enabled();

        let span_path = std::env::var("ATUIN_SPAN").ok().map(|p| {
            if p.is_empty() {
                "atuin-spans.json".to_string()
            } else {
                p
            }
        });

        macro_rules! make_span_layer {
            ($path:expr) => {{
                let span_file = OpenOptions::new()
                    .create(true)
                    .truncate(true)
                    .write(true)
                    .open($path)?;
                Some(
                    fmt::layer()
                        .json()
                        .with_writer(span_file)
                        .with_span_events(FmtSpan::NEW | FmtSpan::CLOSE)
                        .with_filter(LevelFilter::TRACE),
                )
            }};
        }

        if use_search_logging {
            let search_filename = settings.logs.search.file.clone();
            let log_dir = PathBuf::from(&settings.logs.dir);
            fs::create_dir_all(&log_dir)?;

            cleanup_old_logs(&log_dir, &search_filename, settings.logs.search_retention());

            let file_appender =
                RollingFileAppender::new(Rotation::DAILY, &log_dir, &search_filename);

            let filter = if env_log_set {
                base_filter
            } else {
                EnvFilter::default()
                    .add_directive(settings.logs.search_level().as_directive().parse()?)
                    .add_directive("sqlx_sqlite::regexp=off".parse()?)
            };

            let base = tracing_subscriber::registry().with(
                fmt::layer()
                    .with_writer(file_appender)
                    .with_ansi(false)
                    .with_filter(filter),
            );

            match &span_path {
                Some(sp) => {
                    base.with(make_span_layer!(sp)).init();
                }
                None => {
                    base.init();
                }
            }
        }

        tracing::trace!(command = ?self, "client command");

        match self {
            Self::History(history) => return history.run(&settings).await,
            Self::Init(init) => {
                init.run(&settings);
                return Ok(());
            }
            Self::Doctor => return doctor::run(&settings).await,
            Self::Uuid => {
                println!("{}", uuid_v7().as_simple());
                return Ok(());
            }
            _ => {}
        }

        let db_path = PathBuf::from(settings.db_path.as_str());
        let db = Sqlite::new(db_path, settings.local_timeout).await?;

        let theme_name = settings.theme.name.clone();
        let theme = theme_manager.load_theme(theme_name.as_str(), settings.theme.max_depth);

        match self {
            Self::Import(import) => import.run(&db).await,
            Self::Stats(stats) => stats.run(&db, &settings, theme).await,
            Self::Search(search) => search.run(db, &mut settings, theme).await,
            Self::DefaultConfig => {
                default_config::run();
                Ok(())
            }
            Self::History(_) | Self::Init(_) | Self::Doctor | Self::Uuid => unreachable!(),
        }
    }
}
