use clap::Parser;

#[derive(Debug, Parser)]
#[command(version, about = "Chat with an OpenAI-compatible coding agent")]
pub struct Cli {}
