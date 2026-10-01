use std::{env, fs};
use zed_extension_api::{self as zed, settings::LspSettings};

const SERVER_NAME: &str = "kusto-lsp";

/// Where the server finds the run log Zed writes for the code lenses.
const DATA_DIR_VARIABLE: &str = "KUSTO_ZED_DATA_DIR";

/// Zed's data directory. The extension's working directory is `<data dir>/extensions/work/kusto`,
/// and the extension API does not say where the data directory is any other way.
fn zed_data_dir() -> Option<String> {
    let work_dir = env::current_dir().ok()?;
    let data_dir = work_dir.ancestors().nth(3)?;
    Some(data_dir.to_string_lossy().into_owned())
}

fn with_data_dir(mut environment: Vec<(String, String)>) -> Vec<(String, String)> {
    if !environment.iter().any(|(name, _)| name == DATA_DIR_VARIABLE)
        && let Some(data_dir) = zed_data_dir()
    {
        environment.push((DATA_DIR_VARIABLE.to_string(), data_dir));
    }
    environment
}

struct KustoExtension;

impl zed::Extension for KustoExtension {
    fn new() -> Self {
        Self
    }

    fn language_server_command(
        &mut self,
        _language_server_id: &zed::LanguageServerId,
        worktree: &zed::Worktree,
    ) -> zed::Result<zed::Command> {
        let settings = LspSettings::for_worktree(SERVER_NAME, worktree)?;
        if let Some(binary) = settings.binary
            && let Some(path) = binary.path
        {
            return Ok(zed::Command {
                command: path,
                args: binary.arguments.unwrap_or_default(),
                env: with_data_dir(binary.env.unwrap_or_default().into_iter().collect()),
            });
        }

        if let Some(path) = worktree.which(SERVER_NAME) {
            return Ok(zed::Command {
                command: path,
                args: Vec::new(),
                env: with_data_dir(worktree.shell_env()),
            });
        }

        let path = env::current_dir()
            .map_err(|error| error.to_string())?
            .join("server")
            .join(SERVER_NAME);
        if fs::metadata(&path).is_ok_and(|metadata| metadata.is_file()) {
            return Ok(zed::Command {
                command: path.to_string_lossy().into_owned(),
                args: Vec::new(),
                env: with_data_dir(worktree.shell_env()),
            });
        }

        Err("Kusto language server not found. Run extensions/kusto/install-server.sh or set lsp.kusto-lsp.binary.path in Zed settings.".into())
    }
}

zed::register_extension!(KustoExtension);
