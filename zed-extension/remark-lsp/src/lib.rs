use zed_extension_api as zed;

struct RemarkExtension;

impl zed::Extension for RemarkExtension {
    fn new() -> Self {
        Self
    }

    fn language_server_command(
        &mut self,
        _language_server_id: &zed::LanguageServerId,
        worktree: &zed::Worktree,
    ) -> zed::Result<zed::Command> {
        let command = worktree
            .which("remark")
            .ok_or_else(|| "remark binary not found in PATH".to_string())?;

        let mut args = vec!["lsp".to_string()];
        if let Ok(extra) = std::env::var("REMARK_LSP_ARGS") {
            args.extend(extra.split_whitespace().map(|arg| arg.to_string()));
        }

        Ok(zed::Command {
            command,
            args,
            env: worktree.shell_env(),
        })
    }
}

zed::register_extension!(RemarkExtension);
