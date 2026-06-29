#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlashCommand {
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
    Terminal,
}

pub const SLASH_COMMANDS: [SlashCommand; 9] = [
    SlashCommand {
        name: "/archive",
        description: "Archive this session",
        kind: SlashCommandKind::Archive,
    },
    SlashCommand {
        name: "/clear",
        description: "Clear this session's conversation context",
        kind: SlashCommandKind::Clear,
    },
    SlashCommand {
        name: "/compact",
        description: "Summarize earlier conversation and continue compacted",
        kind: SlashCommandKind::Compact,
    },
    SlashCommand {
        name: "/delete",
        description: "Permanently delete this session",
        kind: SlashCommandKind::Delete,
    },
    SlashCommand {
        name: "/model",
        description: "Switch provider and model",
        kind: SlashCommandKind::Model,
    },
    SlashCommand {
        name: "/new",
        description: "Start a fresh session",
        kind: SlashCommandKind::New,
    },
    SlashCommand {
        name: "/resume",
        description: "Resume a saved session",
        kind: SlashCommandKind::Resume,
    },
    SlashCommand {
        name: "/status",
        description: "Open runtime, usage, and workspace statistics",
        kind: SlashCommandKind::Status,
    },
    SlashCommand {
        name: "/terminal",
        description: "Toggle the visible terminal and terminal-backed shell tool",
        kind: SlashCommandKind::Terminal,
    },
];

pub fn matching_slash_commands(query: &str) -> Vec<SlashCommand> {
    SLASH_COMMANDS
        .into_iter()
        .filter(|command| command.name[1..].starts_with(query))
        .collect()
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
}
