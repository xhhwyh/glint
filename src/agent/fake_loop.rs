use std::{sync::mpsc::Sender, thread, time::Duration};

use super::AgentEvent;

pub fn spawn_fake_loop(prompt: String, tx: Sender<AgentEvent>) {
    thread::spawn(move || {
        tx.send(AgentEvent::Started).ok();

        let response = format!(
            "I received your task: \"{prompt}\".\n\nThis is a minimal agent loop: prepare context, stream a model response, then return to idle. Tools can be added later as new agent events."
        );

        for word in response.split_inclusive(' ') {
            tx.send(AgentEvent::AssistantDelta(word.to_owned())).ok();
            thread::sleep(Duration::from_millis(35));
        }

        tx.send(AgentEvent::AssistantFinished).ok();
    });
}
