use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use release_channel::ReleaseChannel;

use crate::{RemoteOs, RemotePlatform};

/// Identifies the set of remote servers shipped with this ZedTracer app.
pub const BUNDLED_SERVER_BUILD_ID: Option<&str> = option_env!("ZEDTRACER_REMOTE_SERVER_BUILD_ID");

pub(crate) fn binary_name(
    release_channel: ReleaseChannel,
    version: &str,
    platform: RemotePlatform,
) -> String {
    let identifier = match BUNDLED_SERVER_BUILD_ID {
        Some(build_id) => format!("bundle-{build_id}"),
        None => format!("{}-{version}", release_channel.dev_name()),
    };
    format!(
        "zedtracer-remote-server-{identifier}{}",
        if platform.os.is_windows() { ".exe" } else { "" }
    )
}

pub(crate) fn version_matches(output: &str) -> bool {
    matches_build_id(output, BUNDLED_SERVER_BUILD_ID)
}

fn matches_build_id(output: &str, build_id: Option<&str>) -> bool {
    build_id.is_none_or(|build_id| {
        output.trim().lines().next_back() == Some(format!("zedtracer-{build_id}").as_str())
    })
}

pub(crate) async fn bundled_archive(platform: RemotePlatform) -> Result<Option<PathBuf>> {
    let Some(build_id) = BUNDLED_SERVER_BUILD_ID else {
        return Ok(None);
    };
    let executable = std::env::current_exe().context("locating the ZedTracer application")?;
    archive_for_build(&executable, build_id, platform)
        .await
        .map(Some)
}

async fn archive_for_build(
    executable: &Path,
    build_id: &str,
    platform: RemotePlatform,
) -> Result<PathBuf> {
    let archive = archive_path(executable, build_id, platform)?;
    let metadata = smol::fs::metadata(&archive).await.with_context(|| {
        format!(
            "This ZedTracer app is missing its bundled remote server for {} {} (build {build_id}): {}. \
             Reinstall a complete ZedTracer app; upstream Zed servers cannot be used.",
            platform.os,
            platform.arch,
            archive.display()
        )
    })?;
    anyhow::ensure!(
        metadata.is_file() && metadata.len() > 0,
        "The bundled ZedTracer remote server is empty or not a file: {}",
        archive.display()
    );
    Ok(archive)
}

fn archive_path(executable: &Path, build_id: &str, platform: RemotePlatform) -> Result<PathBuf> {
    anyhow::ensure!(
        !build_id.is_empty()
            && build_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-'),
        "Invalid ZedTracer remote server build ID: {build_id:?}"
    );
    anyhow::ensure!(
        platform.os != RemoteOs::Windows,
        "This ZedTracer app does not bundle Windows remote servers"
    );
    let executable_dir = executable
        .parent()
        .filter(|directory| directory.file_name().is_some_and(|name| name == "MacOS"))
        .context("Bundled ZedTracer remote servers require launching the installed macOS app")?;
    let contents_dir = executable_dir
        .parent()
        .context("locating the ZedTracer app's Contents directory")?;
    Ok(contents_dir
        .join("Resources/remote_servers")
        .join(build_id)
        .join(format!(
            "zedtracer-remote-server-{}-{}.gz",
            platform.os, platform.arch
        )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RemoteArch;

    #[test]
    fn bundled_archives_follow_the_remote_platform_not_the_client() -> Result<()> {
        let executable = Path::new("/Applications/zedTracer.app/Contents/MacOS/zed");
        for os in [RemoteOs::Linux, RemoteOs::MacOs] {
            for arch in [RemoteArch::X86_64, RemoteArch::Aarch64] {
                let archive = archive_path(executable, "build-123", RemotePlatform { os, arch })?;
                assert_eq!(
                    archive,
                    PathBuf::from(format!(
                        "/Applications/zedTracer.app/Contents/Resources/remote_servers/\
                         build-123/zedtracer-remote-server-{os}-{arch}.gz"
                    ))
                );
                assert_ne!(
                    archive,
                    archive_path(executable, "build-456", RemotePlatform { os, arch })?
                );
            }
        }
        Ok(())
    }

    #[test]
    fn bundled_server_rejects_another_build_or_an_upstream_version() {
        assert!(matches_build_id("zedtracer-build-123\n", Some("build-123")));
        assert!(matches_build_id(
            "Shell startup message\nzedtracer-build-123\n",
            Some("build-123")
        ));
        assert!(!matches_build_id(
            "zedtracer-build-456\n",
            Some("build-123")
        ));
        assert!(!matches_build_id("1.30.0\n", Some("build-123")));
        assert!(!matches_build_id("", Some("build-123")));
        assert!(matches_build_id("development-commit\n", None));
    }

    #[test]
    fn missing_or_empty_bundle_is_an_error_not_an_upstream_fallback() -> Result<()> {
        smol::block_on(async {
            let directory = tempfile::tempdir()?;
            let executable = directory.path().join("zedTracer.app/Contents/MacOS/zed");
            let platform = RemotePlatform {
                os: RemoteOs::Linux,
                arch: RemoteArch::X86_64,
            };
            let other_archive = archive_path(&executable, "other-build", platform)?;
            std::fs::create_dir_all(other_archive.parent().context("archive has no parent")?)?;
            std::fs::write(&other_archive, b"other build")?;
            assert!(
                archive_for_build(&executable, "current-build", platform)
                    .await
                    .is_err()
            );
            let archive = archive_path(&executable, "current-build", platform)?;
            std::fs::create_dir_all(archive.parent().context("archive has no parent")?)?;
            std::fs::write(&archive, b"")?;
            assert!(
                archive_for_build(&executable, "current-build", platform)
                    .await
                    .is_err()
            );
            std::fs::write(&archive, b"current build")?;
            assert_eq!(
                archive_for_build(&executable, "current-build", platform).await?,
                archive
            );
            Ok(())
        })
    }

    #[test]
    fn unsupported_bundle_layouts_and_build_ids_are_errors() {
        let platform = RemotePlatform {
            os: RemoteOs::Linux,
            arch: RemoteArch::X86_64,
        };
        assert!(
            archive_path(Path::new("/repo/target/release/zed"), "build-123", platform).is_err()
        );
        for build_id in [
            "",
            "../other-build",
            "/tmp/server",
            "build/123",
            "build\n123",
        ] {
            assert!(
                archive_path(
                    Path::new("/Applications/zedTracer.app/Contents/MacOS/zed"),
                    build_id,
                    platform,
                )
                .is_err()
            );
        }
        assert!(
            archive_path(
                Path::new("/Applications/zedTracer.app/Contents/MacOS/zed"),
                "build-123",
                RemotePlatform {
                    os: RemoteOs::Windows,
                    ..platform
                },
            )
            .is_err()
        );
    }
}
