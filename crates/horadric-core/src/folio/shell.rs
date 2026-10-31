//! `!` commands: the human runs a shell command from the composer, Horadric
//! runs it (not Claude), and what it printed goes in front of the next
//! prompt, tagged as Claude Code's own `!` mode tags it, so Claude reads
//! the command and its output as the human saw them.

/// One `!` command and what it printed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShellRun {
    pub command: String,
    /// Its output and errors as they came, at most
    /// [`super::OUTPUT_TAIL`] bytes of the end.
    pub output: String,
    /// Its exit code, None while it runs or when it never started.
    pub code: Option<i32>,
    pub running: bool,
    /// It went to Claude with a prompt.
    pub sent: bool,
}

const INPUT: (&str, &str) = ("<bash-input>", "</bash-input>");
const OUTPUT: (&str, &str) = ("<bash-stdout>", "</bash-stdout>");
const CODE: (&str, &str) = ("<bash-exit-code>", "</bash-exit-code>");

/// A run as it goes in front of a prompt.
pub fn shell_text(r: &ShellRun) -> String {
    let mut s = format!(
        "{}{}{}\n{}{}{}\n",
        INPUT.0,
        r.command,
        INPUT.1,
        OUTPUT.0,
        r.output.trim_end(),
        OUTPUT.1
    );
    if let Some(c) = r.code.filter(|c| *c != 0) {
        s.push_str(&format!("{}{c}{}\n", CODE.0, CODE.1));
    }
    s.push('\n');
    s
}

/// The runs in front of a prompt's text, and the text the human wrote.
pub fn split_shell(text: &str) -> (Vec<ShellRun>, &str) {
    let mut runs = Vec::new();
    let mut rest = text;
    while let Some(after) = rest.trim_start().strip_prefix(INPUT.0) {
        let Some((command, after)) = after.split_once(INPUT.1) else {
            break;
        };
        let after = after.trim_start();
        let Some(after) = after.strip_prefix(OUTPUT.0) else {
            break;
        };
        let Some((output, mut after)) = after.split_once(OUTPUT.1) else {
            break;
        };
        let mut code = Some(0);
        if let Some(c) = after.trim_start().strip_prefix(CODE.0) {
            if let Some((n, a)) = c.split_once(CODE.1) {
                code = n.trim().parse().ok();
                after = a;
            }
        }
        runs.push(ShellRun {
            command: command.to_string(),
            output: output.to_string(),
            code,
            running: false,
            sent: true,
        });
        rest = after;
    }
    if runs.is_empty() {
        return (runs, text);
    }
    (runs, rest.trim_start())
}

/// The command a composer's text runs, when it is a `!` command.
pub fn shell_command(text: &str) -> Option<&str> {
    let c = text.trim().strip_prefix('!')?.trim();
    (!c.is_empty()).then_some(c)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::folio::{Block, Folio};

    #[test]
    fn a_run_goes_before_the_prompt_and_comes_back_out() {
        let run = ShellRun {
            command: "git status".into(),
            output: "clean\n".into(),
            code: Some(1),
            ..ShellRun::default()
        };
        let text = format!("{}{}fix it", shell_text(&run), shell_text(&run));
        let (runs, rest) = split_shell(&text);
        assert_eq!(rest, "fix it");
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].command, "git status");
        assert_eq!(runs[0].output, "clean");
        assert_eq!(runs[0].code, Some(1));
        assert_eq!(split_shell("just words"), (vec![], "just words"));
    }

    #[test]
    fn a_run_shown_live_is_not_shown_again_with_its_prompt() {
        let mut f = Folio::new();
        let at = f.shell_started("ls");
        f.shell_done(at, "ls", "a\nb", Some(0));
        let line = format!("{}what now", f.shell_context());
        f.push_prompt(&line);
        assert!(matches!(&f.blocks[0], Block::Shell(r) if r.sent && r.output == "a\nb"));
        assert_eq!(f.blocks[1], Block::Prompt("what now".into()));
        assert_eq!(f.blocks.len(), 2);
        assert!(f.shell_context().is_empty());
        // A replay of the same prompt, with no run shown, shows the run.
        let mut g = Folio::new();
        g.push_prompt(&line);
        assert!(matches!(&g.blocks[0], Block::Shell(r) if r.command == "ls"));
    }

    #[test]
    fn a_bang_is_a_command_only_with_one() {
        assert_eq!(shell_command("!git log"), Some("git log"));
        assert_eq!(shell_command("! "), None);
        assert_eq!(shell_command("hi!"), None);
    }
}
