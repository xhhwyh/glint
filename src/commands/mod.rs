#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlashCommand {
    pub name: &'static str,
    pub description: &'static str,
    pub kind: SlashCommandKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlashCommandKind {
    Compact,
    Model,
    Resume,
}

pub const SLASH_COMMANDS: [SlashCommand; 3] = [
    SlashCommand {
        name: "/compact",
        description: "Summarize earlier conversation and continue compacted",
        kind: SlashCommandKind::Compact,
    },
    SlashCommand {
        name: "/model",
        description: "Switch provider and model",
        kind: SlashCommandKind::Model,
    },
    SlashCommand {
        name: "/resume",
        description: "Resume a saved session",
        kind: SlashCommandKind::Resume,
    },
];

pub fn matching_slash_commands(query: &str) -> Vec<SlashCommand> {
    SLASH_COMMANDS
        .into_iter()
        .filter(|command| command.name[1..].starts_with(query))
        .take(5)
        .collect()
}
