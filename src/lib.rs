use std::fs;

use zed_extension_api::{
    self as zed, LanguageServerId, LanguageServerInstallationStatus as Status, Result,
    set_language_server_installation_status as set_install_status, settings::LspSettings,
};

const ADAPTER_REPOSITORY: &str = "rusty-auth/rustowl-zed";
const RUSTOWL_REPOSITORY: &str = "cordx56/rustowl";

struct BinaryPaths {
    adapter: String,
    rustowl: String,
    rustowl_auto_setup: bool,
    adapter_args: Vec<String>,
}

#[derive(Default)]
struct RustOwlExtension {
    cached_adapter_path: Option<String>,
    cached_rustowl_path: Option<String>,
}

impl RustOwlExtension {
    fn binary_paths(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<BinaryPaths> {
        let settings = LspSettings::for_worktree(language_server_id.as_ref(), worktree).ok();
        let configured_binary = settings
            .as_ref()
            .and_then(|settings| settings.binary.as_ref());
        let adapter_args = configured_binary
            .and_then(|binary| binary.arguments.clone())
            .unwrap_or_default();

        let adapter = configured_binary
            .and_then(|binary| binary.path.clone())
            .or_else(|| worktree.which("rustowl-zed-adapter"))
            .map(Ok)
            .unwrap_or_else(|| self.managed_adapter_path(language_server_id))?;

        let (rustowl, rustowl_auto_setup) = if let Some(rustowl) = worktree.which("rustowl") {
            (rustowl, false)
        } else {
            (self.managed_rustowl_path(language_server_id)?, true)
        };

        Ok(BinaryPaths {
            adapter,
            rustowl,
            rustowl_auto_setup,
            adapter_args,
        })
    }

    fn managed_adapter_path(&mut self, language_server_id: &LanguageServerId) -> Result<String> {
        if let Some(path) = existing_file(&self.cached_adapter_path) {
            return Ok(path);
        }

        set_install_status(language_server_id, &Status::CheckingForUpdate);
        let release = zed::latest_github_release(
            ADAPTER_REPOSITORY,
            zed::GithubReleaseOptions {
                require_assets: true,
                pre_release: false,
            },
        )?;
        let (target, archive_suffix, file_type) = platform_artifact()?;
        let asset_name = format!("rustowl-zed-adapter-{target}.{archive_suffix}");
        let asset = release
            .assets
            .iter()
            .find(|asset| asset.name == asset_name)
            .ok_or_else(|| format!("RustOwl adapter release has no asset named {asset_name}"))?;

        let version_dir = format!("adapter-{}", release.version);
        let executable = executable_name("rustowl-zed-adapter");
        let binary_path = format!("{version_dir}/{executable}");
        if !is_file(&binary_path) {
            set_install_status(language_server_id, &Status::Downloading);
            zed::download_file(&asset.download_url, &version_dir, file_type)
                .map_err(|error| format!("failed to download RustOwl adapter: {error}"))?;
            make_executable(&binary_path)?;
            remove_old_installations("adapter-", &version_dir);
        }

        self.cached_adapter_path = Some(binary_path.clone());
        Ok(binary_path)
    }

    fn managed_rustowl_path(&mut self, language_server_id: &LanguageServerId) -> Result<String> {
        if let Some(path) = existing_file(&self.cached_rustowl_path) {
            return Ok(path);
        }

        set_install_status(language_server_id, &Status::CheckingForUpdate);
        let release = zed::latest_github_release(
            RUSTOWL_REPOSITORY,
            zed::GithubReleaseOptions {
                require_assets: true,
                pre_release: false,
            },
        )?;
        let (target, archive_suffix, file_type) = platform_artifact()?;
        let asset_name = format!("rustowl-{target}.{archive_suffix}");
        let asset = release
            .assets
            .iter()
            .find(|asset| asset.name == asset_name)
            .ok_or_else(|| format!("RustOwl release has no asset named {asset_name}"))?;

        let version_dir = format!("rustowl-{}", release.version);
        let executable = executable_name("rustowl");
        let binary_path = format!("{version_dir}/{executable}");
        if !is_file(&binary_path) {
            set_install_status(language_server_id, &Status::Downloading);
            zed::download_file(&asset.download_url, &version_dir, file_type)
                .map_err(|error| format!("failed to download RustOwl: {error}"))?;
            make_executable(&binary_path)?;
            remove_old_installations("rustowl-", &version_dir);
        }

        self.cached_rustowl_path = Some(binary_path.clone());
        Ok(binary_path)
    }
}

impl zed::Extension for RustOwlExtension {
    fn new() -> Self {
        Self::default()
    }

    fn language_server_command(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<zed::Command> {
        let binaries = self.binary_paths(language_server_id, worktree)?;
        Ok(zed::Command {
            command: binaries.adapter,
            args: binaries.adapter_args,
            env: vec![
                ("RUSTOWL_BINARY".into(), binaries.rustowl),
                (
                    "RUSTOWL_AUTO_SETUP".into(),
                    if binaries.rustowl_auto_setup {
                        "1"
                    } else {
                        "0"
                    }
                    .into(),
                ),
            ],
        })
    }

    fn language_server_initialization_options(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<Option<zed::serde_json::Value>> {
        Ok(
            LspSettings::for_worktree(language_server_id.as_ref(), worktree)
                .ok()
                .and_then(|settings| settings.initialization_options),
        )
    }

    fn language_server_workspace_configuration(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<Option<zed::serde_json::Value>> {
        Ok(
            LspSettings::for_worktree(language_server_id.as_ref(), worktree)
                .ok()
                .and_then(|settings| settings.settings),
        )
    }
}

fn existing_file(path: &Option<String>) -> Option<String> {
    path.as_ref()
        .filter(|path| is_file(path))
        .map(ToOwned::to_owned)
}

fn is_file(path: &str) -> bool {
    fs::metadata(path).is_ok_and(|metadata| metadata.is_file())
}

fn executable_name(stem: &str) -> String {
    let (os, _) = zed::current_platform();
    if os == zed::Os::Windows {
        format!("{stem}.exe")
    } else {
        stem.into()
    }
}

fn platform_artifact() -> Result<(String, &'static str, zed::DownloadedFileType)> {
    let (os, architecture) = zed::current_platform();
    let architecture = match architecture {
        zed::Architecture::Aarch64 => "aarch64",
        zed::Architecture::X8664 => "x86_64",
        zed::Architecture::X86 => {
            return Err("RustOwl does not publish 32-bit binaries".into());
        }
    };
    let platform = match os {
        zed::Os::Mac => "apple-darwin",
        zed::Os::Linux => "unknown-linux-gnu",
        zed::Os::Windows => "pc-windows-msvc",
    };
    let (suffix, file_type) = match os {
        zed::Os::Windows => ("zip", zed::DownloadedFileType::Zip),
        _ => ("tar.gz", zed::DownloadedFileType::GzipTar),
    };
    Ok((format!("{architecture}-{platform}"), suffix, file_type))
}

#[cfg(unix)]
fn make_executable(path: &str) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = fs::metadata(path)
        .map_err(|error| format!("failed to inspect {path}: {error}"))?
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)
        .map_err(|error| format!("failed to make {path} executable: {error}"))?;
    Ok(())
}

#[cfg(not(unix))]
fn make_executable(_path: &str) -> Result<()> {
    Ok(())
}

fn remove_old_installations(prefix: &str, current: &str) {
    let Ok(entries) = fs::read_dir(".") else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name.starts_with(prefix) && name != current {
            fs::remove_dir_all(entry.path()).ok();
        }
    }
}

zed::register_extension!(RustOwlExtension);
