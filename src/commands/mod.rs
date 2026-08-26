use crate::plugins::PluginCommand;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlashCommand {
    pub name: String,
    pub description: String,
    pub kind: SlashCommandKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BuiltinSlashCommand {
    pub name: &'static str,
    pub description: &'static str,
    pub kind: SlashCommandKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlashCommandKind {
    New,
    Clear,
    Archive,
    Delete,
    Status,
    Compact,
    Model,
    Resume,
    Mcp,
    Plugins,
    PluginPrompt(usize),
    ReloadPlugins,
}

pub const SLASH_COMMANDS: [BuiltinSlashCommand; 11] = [
    BuiltinSlashCommand {
        name: "/archive",
        description: "Archive this session",
        kind: SlashCommandKind::Archive,
    },
    BuiltinSlashCommand {
        name: "/clear",
        description: "Clear this session's conversation context",
        kind: SlashCommandKind::Clear,
    },
    BuiltinSlashCommand {
        name: "/compact",
        description: "Summarize earlier conversation and continue compacted",
        kind: SlashCommandKind::Compact,
    },
    BuiltinSlashCommand {
        name: "/delete",
        description: "Permanently delete this session",
        kind: SlashCommandKind::Delete,
    },
    BuiltinSlashCommand {
        name: "/mcp",
        description: "Manage MCP servers and inspect capabilities",
        kind: SlashCommandKind::Mcp,
    },
    BuiltinSlashCommand {
        name: "/model",
        description: "Switch provider and model",
        kind: SlashCommandKind::Model,
    },
    BuiltinSlashCommand {
        name: "/new",
        description: "Start a fresh session",
        kind: SlashCommandKind::New,
    },
    BuiltinSlashCommand {
        name: "/plugins",
        description: "Browse installed plugins and marketplaces",
        kind: SlashCommandKind::Plugins,
    },
    BuiltinSlashCommand {
        name: "/reload-plugins",
        description: "Reload installed plugins and marketplaces",
        kind: SlashCommandKind::ReloadPlugins,
    },
    BuiltinSlashCommand {
        name: "/resume",
        description: "Resume a saved session",
        kind: SlashCommandKind::Resume,
    },
    BuiltinSlashCommand {
        name: "/status",
        description: "Open runtime, usage, and workspace statistics",
        kind: SlashCommandKind::Status,
    },
];

pub fn matching_slash_commands(query: &str, plugins: &[PluginCommand]) -> Vec<SlashCommand> {
    let mut commands = SLASH_COMMANDS
        .into_iter()
        .filter(|command| command.name[1..].starts_with(query))
        .map(SlashCommand::from)
        .collect::<Vec<_>>();
    commands.extend(
        plugins
            .iter()
            .enumerate()
            .filter(|(_, command)| command.name.starts_with(query))
            .map(|(index, command)| SlashCommand {
                name: format!("/{}", command.name),
                description: command.description.clone(),
                kind: SlashCommandKind::PluginPrompt(index),
            }),
    );
    commands.sort_by(|left, right| left.name.cmp(&right.name));
    commands
}

impl From<BuiltinSlashCommand> for SlashCommand {
    fn from(command: BuiltinSlashCommand) -> Self {
        Self {
            name: command.name.to_owned(),
            description: command.description.to_owned(),
            kind: command.kind,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slash_commands_are_sorted_by_name() {
        let names = SLASH_COMMANDS
            .iter()
            .map(|command| command.name)
            .collect::<Vec<_>>();
        let mut sorted = names.clone();
        sorted.sort_unstable();

        assert_eq!(names, sorted);
    }

    #[test]
    fn slash_commands_do_not_include_terminal() {
        assert!(
            !SLASH_COMMANDS
                .iter()
                .any(|command| command.name == "/terminal")
        );
    }

    #[test]
    fn plugin_commands_are_namespaced_and_sorted_with_builtins() {
        let commands = vec![PluginCommand {
            name: "demo:review".to_owned(),
            description: "Review code".to_owned(),
            prompt: "Review this project".to_owned(),
            plugin: "demo".to_owned(),
        }];
        let matches = matching_slash_commands("demo", &commands);
        assert_eq!(matches[0].name, "/demo:review");
        assert_eq!(matches[0].kind, SlashCommandKind::PluginPrompt(0));
    }
}
