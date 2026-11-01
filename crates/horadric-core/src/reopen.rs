//! The sessions the human closed, newest first, so one closed by accident
//! comes back with its conversation in one action, as a browser reopens a
//! closed tab.
//!
//! Only a session with a conversation to carry on is kept: one closed
//! before its first prompt has nothing to come back to. Kept short, since
//! the quest log holds the rest.

use serde::{Deserialize, Serialize};

use crate::saved::SavedSession;

/// How many closed sessions are kept.
pub const KEEP: usize = 10;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Closed {
    /// The session as it was, paused, with whether it was a chat.
    pub session: SavedSession,
    /// The project key its cluster had, for the project menu's line.
    pub project: String,
    /// When it was closed, in seconds since the epoch.
    pub at: u64,
}

impl Closed {
    /// The conversation it carries on. Always Some in the list.
    pub fn conversation(&self) -> Option<&str> {
        (self.session.claude_session_id.as_deref()).filter(|_| self.session.prompted)
    }
}

/// Puts a closed session at the head of the list, or leaves the list as
/// it is when there is nothing to reopen. The same conversation closed
/// again is kept once, as its newest closing.
pub fn remember(list: &mut Vec<Closed>, mut closed: Closed) {
    if closed.session.shell || closed.session.ssh.is_some() || closed.conversation().is_none() {
        return;
    }
    closed.session.running = false;
    let conversation = closed.conversation().map(str::to_string);
    list.retain(|c| c.conversation().map(str::to_string) != conversation);
    list.insert(0, closed);
    list.truncate(KEEP);
}

/// The newest closed session, in the project with key `project` when one
/// is named, or the one carrying on `conversation` when that is named.
/// Taken off the list, so a second press reopens the next one and never
/// the same twice.
pub fn take(
    list: &mut Vec<Closed>,
    project: Option<&str>,
    conversation: Option<&str>,
) -> Option<Closed> {
    let i = list.iter().position(|c| {
        project.is_none_or(|p| c.project == p)
            && conversation.is_none_or(|v| c.conversation() == Some(v))
    })?;
    Some(list.remove(i))
}

/// The closed sessions of the project with key `project`, newest first.
pub fn of<'a>(list: &'a [Closed], project: &'a str) -> impl Iterator<Item = &'a Closed> + 'a {
    list.iter().filter(move |c| c.project == project)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::Agent;

    fn closed(project: &str, conversation: Option<&str>, prompted: bool) -> Closed {
        Closed {
            session: SavedSession {
                id: format!("tile-{}", conversation.unwrap_or("none")),
                name: "s".into(),
                title: None,
                renamed: false,
                cwd: format!("C:\\{project}"),
                agent: Agent::Claude,
                args: Vec::new(),
                claude_session_id: conversation.map(String::from),
                prompted,
                last_line: String::new(),
                running: true,
                shell: false,
                ssh: None,
                worktree: None,
                loot: Default::default(),
                runeword: None,
                chat: true,
            },
            project: project.into(),
            at: 0,
        }
    }

    #[test]
    fn only_a_conversation_is_kept_newest_first_and_once() {
        let mut list = Vec::new();
        remember(&mut list, closed("a", Some("1"), true));
        remember(&mut list, closed("a", Some("2"), false));
        remember(&mut list, closed("a", None, true));
        let mut shell = closed("a", Some("3"), true);
        shell.session.shell = true;
        remember(&mut list, shell);
        remember(&mut list, closed("b", Some("4"), true));
        remember(&mut list, closed("a", Some("1"), true));
        let kept: Vec<_> = list.iter().filter_map(|c| c.conversation()).collect();
        assert_eq!(kept, ["1", "4"]);
        assert!(!list[0].session.running);
    }

    #[test]
    fn the_list_stays_short() {
        let mut list = Vec::new();
        for i in 0..KEEP + 3 {
            remember(&mut list, closed("a", Some(&i.to_string()), true));
        }
        assert_eq!(list.len(), KEEP);
        assert_eq!(
            list[0].conversation(),
            Some((KEEP + 2).to_string().as_str())
        );
    }

    #[test]
    fn taking_reopens_each_once_newest_first_and_by_project() {
        let mut list = Vec::new();
        remember(&mut list, closed("a", Some("1"), true));
        remember(&mut list, closed("b", Some("2"), true));
        remember(&mut list, closed("a", Some("3"), true));
        assert_eq!(of(&list, "a").count(), 2);
        let take_one = |list: &mut Vec<Closed>, p, v| {
            take(list, p, v).map(|c| c.session.claude_session_id.unwrap())
        };
        assert_eq!(take_one(&mut list, Some("b"), None).as_deref(), Some("2"));
        assert_eq!(take_one(&mut list, Some("b"), None), None);
        assert_eq!(take_one(&mut list, None, Some("1")).as_deref(), Some("1"));
        assert_eq!(take_one(&mut list, None, None).as_deref(), Some("3"));
        assert_eq!(take_one(&mut list, None, None), None);
    }
}
