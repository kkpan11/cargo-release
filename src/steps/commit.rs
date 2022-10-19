use crate::config;
use crate::error::CliError;
use crate::ops::git;
use crate::ops::replace::{Template, NOW};
use crate::steps::plan;

/// Owner the specified packages
///
/// Will automatically skip published versions
#[derive(Debug, Clone, clap::Args)]
pub struct CommitStep {
    #[command(flatten)]
    manifest: clap_cargo::Manifest,

    /// Custom config file
    #[arg(short, long = "config")]
    custom_config: Option<String>,

    /// Ignore implicit configuration files.
    #[arg(long)]
    isolated: bool,

    /// Comma-separated globs of branch names a release can happen from
    #[arg(long, value_delimiter = ',')]
    allow_branch: Option<Vec<String>>,

    /// Actually perform a release. Dry-run mode is the default
    #[arg(short = 'x', long)]
    execute: bool,

    /// Skip release confirmation and version preview
    #[arg(long)]
    no_confirm: bool,

    #[command(flatten)]
    commit: crate::config::CommitArgs,
}

impl CommitStep {
    pub fn run(&self) -> Result<(), CliError> {
        git::git_version()?;

        let ws_meta = self
            .manifest
            .metadata()
            // When evaluating dependency ordering, we need to consider optional dependencies
            .features(cargo_metadata::CargoOpt::AllFeatures)
            .exec()?;
        let config = self.to_config();
        let ws_config = crate::config::load_workspace_config(&config, &ws_meta)?;
        let pkgs = plan::load(&config, &ws_meta)?;

        let pkgs = plan::plan(pkgs)?;

        let (selected_pkgs, _excluded_pkgs): (Vec<_>, Vec<_>) = pkgs
            .into_iter()
            .map(|(_, pkg)| pkg)
            .partition(|p| p.config.release());
        if crate::ops::git::is_dirty(ws_meta.workspace_root.as_std_path())?.is_none() {
            log::info!("Nothing to commit.");
            return Err(2.into());
        }

        let dry_run = !self.execute;
        let mut failed = false;

        // STEP 0: Help the user make the right decisions.
        failed |= !super::verify_git_branch(
            ws_meta.workspace_root.as_std_path(),
            &ws_config,
            dry_run,
            log::Level::Warn,
        )?;

        failed |= !super::verify_if_behind(
            ws_meta.workspace_root.as_std_path(),
            &ws_config,
            dry_run,
            log::Level::Warn,
        )?;

        // STEP 1: Release Confirmation
        super::confirm("Commit", &selected_pkgs, self.no_confirm, dry_run)?;

        super::commit::workspace_commit(&ws_meta, &ws_config, &selected_pkgs, dry_run)?;

        super::finish(failed, dry_run)
    }

    fn to_config(&self) -> crate::config::ConfigArgs {
        crate::config::ConfigArgs {
            custom_config: self.custom_config.clone(),
            isolated: self.isolated,
            allow_branch: self.allow_branch.clone(),
            commit: self.commit.clone(),
            ..Default::default()
        }
    }
}

pub fn pkg_commit(pkg: &plan::PackageRelease, dry_run: bool) -> Result<(), CliError> {
    let cwd = &pkg.package_root;
    let crate_name = pkg.meta.name.as_str();
    let version = pkg.planned_version.as_ref().unwrap_or(&pkg.initial_version);
    let prev_version_var = pkg.initial_version.bare_version_string.as_str();
    let prev_metadata_var = pkg.initial_version.full_version.build.as_str();
    let version_var = version.bare_version_string.as_str();
    let metadata_var = version.full_version.build.as_str();
    let template = Template {
        prev_version: Some(prev_version_var),
        prev_metadata: Some(prev_metadata_var),
        version: Some(version_var),
        metadata: Some(metadata_var),
        crate_name: Some(crate_name),
        date: Some(NOW.as_str()),
        ..Default::default()
    };
    let commit_msg = template.render(pkg.config.pre_release_commit_message());
    let sign = pkg.config.sign_commit();
    if !git::commit_all(cwd, &commit_msg, sign, dry_run)? {
        // commit failed, abort release
        return Err(101.into());
    }

    Ok(())
}

pub fn workspace_commit(
    ws_meta: &cargo_metadata::Metadata,
    ws_config: &config::Config,
    pkgs: &[plan::PackageRelease],
    dry_run: bool,
) -> Result<(), CliError> {
    let shared_version = super::find_shared_versions(pkgs)?;

    let shared_commit_msg = {
        let version_var = shared_version
            .as_ref()
            .map(|v| v.bare_version_string.as_str());
        let metadata_var = shared_version
            .as_ref()
            .map(|v| v.full_version.build.as_str());
        let template = Template {
            version: version_var,
            metadata: metadata_var,
            date: Some(NOW.as_str()),
            ..Default::default()
        };
        template.render(ws_config.pre_release_commit_message())
    };
    if !git::commit_all(
        ws_meta.workspace_root.as_std_path(),
        &shared_commit_msg,
        ws_config.sign_commit(),
        dry_run,
    )? {
        // commit failed, abort release
        return Err(101.into());
    }

    Ok(())
}