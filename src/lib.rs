use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use sha2::{Digest, Sha256};
use zed_extension_api::{
    self as zed, ContextServerId, LanguageServerId, LanguageServerInstallationStatus as Status,
    Result, set_language_server_installation_status as set_install_status,
    settings::{ContextServerSettings, LspSettings},
};

const RUNTIME_REPOSITORY: &str = "rusty-auth/rustowl-zed";

struct BinaryPaths {
    adapter: String,
    rustowl: String,
    rustowl_auto_setup: bool,
    adapter_args: Vec<String>,
    env: BTreeMap<String, String>,
}

#[derive(Clone)]
struct ManagedRuntime {
    adapter: String,
    rustowl: String,
    rustowlc: String,
    mcp: String,
    verified_install: bool,
}

struct RustOwlExtension {
    cached_runtime: Option<ManagedRuntime>,
    session_id: String,
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
        let env: BTreeMap<_, _> = configured_binary
            .and_then(|binary| binary.env.clone())
            .unwrap_or_default()
            .into_iter()
            .collect();

        let configured_adapter = configured_binary.and_then(|binary| binary.path.clone());
        let managed = if configured_adapter.is_none()
            || !env.contains_key("RUSTOWL_BINARY")
                && configured_adapter
                    .as_deref()
                    .and_then(|adapter| sibling_executable(adapter, "rustowl"))
                    .is_none()
        {
            Some(self.managed_runtime(Some(language_server_id))?)
        } else {
            None
        };
        let adapter = configured_adapter
            .clone()
            .or_else(|| managed.as_ref().map(|runtime| runtime.adapter.clone()))
            .ok_or_else(|| "RustOwl runtime omitted its adapter".to_owned())?;
        let configured_rustowl = env
            .get("RUSTOWL_BINARY")
            .cloned()
            .or_else(|| sibling_executable(&adapter, "rustowl"));
        let (rustowl, rustowl_auto_setup) = if let Some(rustowl) = configured_rustowl {
            (rustowl, false)
        } else {
            (
                managed
                    .as_ref()
                    .ok_or_else(|| "RustOwl runtime omitted its engine".to_owned())?
                    .rustowl
                    .clone(),
                true,
            )
        };
        if let (Some(mcp), Some(rustowlc)) = (
            sibling_executable(&adapter, "rustowl-mcp"),
            sibling_executable(&adapter, "rustowlc"),
        ) {
            self.cached_runtime = Some(ManagedRuntime {
                adapter: adapter.clone(),
                rustowl: rustowl.clone(),
                rustowlc,
                mcp,
                verified_install: false,
            });
        }

        Ok(BinaryPaths {
            adapter,
            rustowl,
            rustowl_auto_setup,
            adapter_args,
            env,
        })
    }

    fn managed_runtime(
        &mut self,
        language_server_id: Option<&LanguageServerId>,
    ) -> Result<ManagedRuntime> {
        if let Some(runtime) = self.cached_runtime.as_ref().filter(|runtime| {
            is_file(&runtime.adapter)
                && is_file(&runtime.rustowl)
                && is_file(&runtime.mcp)
                && is_file(&runtime.rustowlc)
                && (!runtime.verified_install || verify_runtime(runtime).is_ok())
        }) {
            return Ok(runtime.clone());
        }

        if let Some(language_server_id) = language_server_id {
            set_install_status(language_server_id, &Status::CheckingForUpdate);
        }
        let release = zed::latest_github_release(
            RUNTIME_REPOSITORY,
            zed::GithubReleaseOptions {
                require_assets: true,
                pre_release: false,
            },
        )?;
        let (target, archive_suffix, file_type) = platform_artifact()?;
        let asset_name = format!("rustowl-zed-runtime-{target}.{archive_suffix}");
        let asset = release
            .assets
            .iter()
            .find(|asset| asset.name == asset_name)
            .ok_or_else(|| format!("RustOwl release has no asset named {asset_name}"))?;

        let version_dir = format!("runtime-{}", release.version);
        let runtime = ManagedRuntime {
            adapter: format!("{version_dir}/{}", executable_name("rustowl-zed-adapter")),
            rustowl: format!("{version_dir}/{}", executable_name("rustowl")),
            rustowlc: format!("{version_dir}/{}", executable_name("rustowlc")),
            mcp: format!("{version_dir}/{}", executable_name("rustowl-mcp")),
            verified_install: true,
        };
        if verify_runtime(&runtime).is_err() {
            if let Some(language_server_id) = language_server_id {
                set_install_status(language_server_id, &Status::Downloading);
            }
            fs::remove_dir_all(&version_dir).ok();
            zed::download_file(&asset.download_url, &version_dir, file_type)
                .map_err(|error| format!("failed to download RustOwl runtime: {error}"))?;
            if let Err(error) = verify_runtime(&runtime) {
                fs::remove_dir_all(&version_dir).ok();
                return Err(format!(
                    "downloaded RustOwl runtime failed verification: {error}"
                ));
            }
            make_executable(&runtime.adapter)?;
            make_executable(&runtime.rustowl)?;
            make_executable(&runtime.rustowlc)?;
            make_executable(&runtime.mcp)?;
            remove_old_installations("runtime-", &version_dir);
        }

        self.cached_runtime = Some(runtime.clone());
        Ok(runtime)
    }
}

impl zed::Extension for RustOwlExtension {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        Self {
            cached_runtime: None,
            session_id: format!("{nonce:x}"),
        }
    }

    fn language_server_command(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<zed::Command> {
        let binaries = self.binary_paths(language_server_id, worktree)?;
        let mut env = binaries.env;
        env.insert("RUSTOWL_BINARY".into(), binaries.rustowl);
        env.insert(
            "RUSTOWL_AUTO_SETUP".into(),
            if binaries.rustowl_auto_setup {
                "1"
            } else {
                "0"
            }
            .into(),
        );
        env.insert("RUSTOWL_ZED_SESSION_ID".into(), self.session_id.clone());
        env.insert("RUSTOWL_ZED_WORKTREE_ID".into(), worktree.id().to_string());
        Ok(zed::Command {
            command: binaries.adapter,
            args: binaries.adapter_args,
            env: env.into_iter().collect(),
        })
    }

    fn context_server_command(
        &mut self,
        context_server_id: &ContextServerId,
        project: &zed::Project,
    ) -> Result<zed::Command> {
        if context_server_id.as_ref() != "rustowl-ownership" {
            return Err(format!(
                "unknown RustOwl context server {context_server_id}"
            ));
        }
        let settings = ContextServerSettings::for_project(context_server_id.as_ref(), project)
            .unwrap_or_default();
        if let Some(command) = settings.command {
            let path = command
                .path
                .ok_or_else(|| "RustOwl MCP command requires a path".to_owned())?;
            return Ok(zed::Command {
                command: path,
                args: command.arguments.unwrap_or_default(),
                env: command.env.unwrap_or_default().into_iter().collect(),
            });
        }

        let worktree_ids = project.worktree_ids();
        if worktree_ids.is_empty() {
            return Err("RustOwl MCP needs a project worktree".into());
        }
        let runtime = self.managed_runtime(None)?;
        let mut args = vec!["--zed-session".into(), self.session_id.clone()];
        for worktree_id in worktree_ids {
            args.push("--worktree-id".into());
            args.push(worktree_id.to_string());
        }
        Ok(zed::Command {
            command: runtime.mcp,
            args,
            env: Vec::new(),
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

fn is_file(path: &str) -> bool {
    fs::metadata(path).is_ok_and(|metadata| metadata.is_file())
}

fn verify_runtime(runtime: &ManagedRuntime) -> Result<()> {
    let directory = Path::new(&runtime.adapter)
        .parent()
        .ok_or_else(|| "RustOwl runtime has no installation directory".to_owned())?;
    let checksums = fs::read_to_string(directory.join("checksums.sha256"))
        .map_err(|error| format!("could not read runtime checksums: {error}"))?;
    if checksums.starts_with('\u{feff}') || checksums.contains('\r') {
        return Err("runtime checksums must be LF-only UTF-8 without a BOM".to_owned());
    }
    if !checksums.ends_with('\n') {
        return Err("runtime checksums are incomplete".to_owned());
    }

    let mut verified = BTreeSet::new();
    for line in checksums.lines() {
        let (expected, filename) = line
            .split_once("  ")
            .ok_or_else(|| "runtime checksum line is malformed".to_owned())?;
        if expected.len() != 64 || !expected.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(format!("invalid checksum for {filename}"));
        }
        let path = safe_runtime_file(directory, filename)?;
        let bytes = fs::read(&path)
            .map_err(|error| format!("could not read checksummed {filename}: {error}"))?;
        let actual = format!("{:x}", Sha256::digest(bytes));
        if !actual.eq_ignore_ascii_case(expected) {
            return Err(format!("checksum mismatch for {filename}"));
        }
        if !verified.insert(filename.to_owned()) {
            return Err(format!("duplicate checksum for {filename}"));
        }
    }

    let required_binaries = [
        runtime.adapter.as_str(),
        runtime.rustowl.as_str(),
        runtime.rustowlc.as_str(),
        runtime.mcp.as_str(),
    ];
    let required = required_binaries
        .iter()
        .map(|path| {
            Path::new(path)
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_owned)
                .ok_or_else(|| format!("runtime binary path is invalid: {path}"))
        })
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .chain(
            [
                "manifest.json",
                "sbom.cdx.json",
                "LICENSE-MIT",
                "LICENSE-MPL-2.0",
                "LICENSE-APACHE-2.0",
                "THIRD_PARTY_NOTICES.md",
                "ENGINE_THIRD_PARTY_NOTICES.md",
            ]
            .into_iter()
            .map(str::to_owned),
        );
    for required in required {
        if !verified.contains(&required) {
            return Err(format!("runtime checksums omit {required}"));
        }
    }

    let manifest: zed::serde_json::Value = serde_json_file(directory.join("manifest.json"))?;
    if manifest
        .get("formatVersion")
        .and_then(|value| value.as_u64())
        != Some(1)
    {
        return Err("runtime manifest format is incompatible".to_owned());
    }
    if manifest
        .get("extensionVersion")
        .and_then(|value| value.as_str())
        != Some(env!("CARGO_PKG_VERSION"))
    {
        return Err("runtime manifest version does not match this extension".to_owned());
    }
    if manifest
        .get("ownershipGraphSchemaVersion")
        .and_then(|value| value.as_u64())
        != Some(1)
    {
        return Err("runtime ownership graph schema is incompatible".to_owned());
    }
    let binaries = manifest
        .get("binaries")
        .and_then(|value| value.as_array())
        .ok_or_else(|| "runtime manifest omits its binary set".to_owned())?;
    for binary in ["rustowl-zed-adapter", "rustowl", "rustowlc", "rustowl-mcp"] {
        if !binaries.iter().any(|value| value.as_str() == Some(binary)) {
            return Err(format!("runtime manifest omits {binary}"));
        }
    }
    Ok(())
}

fn safe_runtime_file(directory: &Path, filename: &str) -> Result<PathBuf> {
    let path = Path::new(filename);
    if filename.is_empty()
        || path.is_absolute()
        || path.components().count() != 1
        || filename.contains(['/', '\\'])
    {
        return Err(format!("unsafe runtime checksum path {filename}"));
    }
    Ok(directory.join(path))
}

fn serde_json_file(path: PathBuf) -> Result<zed::serde_json::Value> {
    let bytes =
        fs::read(&path).map_err(|error| format!("could not read {}: {error}", path.display()))?;
    zed::serde_json::from_slice(&bytes)
        .map_err(|error| format!("could not parse {}: {error}", path.display()))
}

fn sibling_executable(executable: &str, sibling_stem: &str) -> Option<String> {
    let candidate = Path::new(executable)
        .parent()?
        .join(executable_name(sibling_stem));
    candidate
        .is_file()
        .then(|| candidate.to_string_lossy().into_owned())
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

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_runtime() -> (PathBuf, ManagedRuntime) {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("rustowl-runtime-verify-{nonce}"));
        fs::create_dir_all(&directory).unwrap();
        let binary_names = [
            "rustowl-zed-adapter".to_owned(),
            "rustowl".to_owned(),
            "rustowlc".to_owned(),
            "rustowl-mcp".to_owned(),
        ];
        let legal_names = [
            "LICENSE-MIT".to_owned(),
            "LICENSE-MPL-2.0".to_owned(),
            "LICENSE-APACHE-2.0".to_owned(),
            "THIRD_PARTY_NOTICES.md".to_owned(),
            "ENGINE_THIRD_PARTY_NOTICES.md".to_owned(),
        ];
        for name in binary_names.iter().chain(&legal_names) {
            fs::write(directory.join(name), format!("fixture {name}")).unwrap();
        }
        fs::write(
            directory.join("manifest.json"),
            format!(
                r#"{{"formatVersion":1,"extensionVersion":"{}","ownershipGraphSchemaVersion":1,"binaries":["rustowl-zed-adapter","rustowl","rustowlc","rustowl-mcp"]}}"#,
                env!("CARGO_PKG_VERSION")
            ),
        )
        .unwrap();
        fs::write(directory.join("sbom.cdx.json"), "{}").unwrap();

        let mut checksums = String::new();
        for name in binary_names
            .iter()
            .cloned()
            .chain(legal_names.iter().cloned())
            .chain(["manifest.json".to_owned(), "sbom.cdx.json".to_owned()])
        {
            let digest = Sha256::digest(fs::read(directory.join(&name)).unwrap());
            checksums.push_str(&format!("{digest:x}  {name}\n"));
        }
        fs::write(directory.join("checksums.sha256"), checksums).unwrap();
        let runtime = ManagedRuntime {
            adapter: directory
                .join(&binary_names[0])
                .to_string_lossy()
                .into_owned(),
            rustowl: directory
                .join(&binary_names[1])
                .to_string_lossy()
                .into_owned(),
            rustowlc: directory
                .join(&binary_names[2])
                .to_string_lossy()
                .into_owned(),
            mcp: directory
                .join(&binary_names[3])
                .to_string_lossy()
                .into_owned(),
            verified_install: true,
        };
        (directory, runtime)
    }

    #[test]
    fn verifies_every_managed_runtime_file_before_execution() {
        let (directory, runtime) = fixture_runtime();
        assert!(verify_runtime(&runtime).is_ok());

        fs::write(&runtime.rustowl, "tampered").unwrap();
        assert!(
            verify_runtime(&runtime)
                .unwrap_err()
                .contains("checksum mismatch")
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn checksum_paths_cannot_escape_the_runtime_directory() {
        let directory = Path::new("runtime");
        assert!(safe_runtime_file(directory, "rustowl").is_ok());
        assert!(safe_runtime_file(directory, "../rustowl").is_err());
        assert!(safe_runtime_file(directory, "/tmp/rustowl").is_err());
        assert!(safe_runtime_file(directory, "nested/rustowl").is_err());
    }
}
