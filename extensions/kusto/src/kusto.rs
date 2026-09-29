use std::{env, fs};
use zed_extension_api::{self as zed, settings::LspSettings};

const SERVER_NAME: &str = "kusto-lsp";

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
                env: binary.env.unwrap_or_default().into_iter().collect(),
            });
        }

        if let Some(path) = worktree.which(SERVER_NAME) {
            return Ok(zed::Command {
                command: path,
                args: Vec::new(),
                env: worktree.shell_env(),
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
                env: worktree.shell_env(),
            });
        }

        Err("Kusto language server not found. Run extensions/kusto/install-server.sh or set lsp.kusto-lsp.binary.path in Zed settings.".into())
    }
}

zed::register_extension!(KustoExtension);
