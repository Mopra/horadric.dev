use horadric_core::event::HookEvent;
use horadric_core::session::{Phase, WaitReason};

use super::*;

fn entry(n: usize) -> Entry {
    Entry {
        at: clock(9, 5, n as u16),
        mark: Mark::Ok,
        what: format!("click {n}"),
        outcome: String::new(),
        unasked: false,
    }
}

#[test]
fn control_starts_with_an_allowed_call_and_lasts_after_the_last() {
    let t0 = Instant::now();
    assert_eq!(extend(None, t0, false), None);
    let until = extend(None, t0, true);
    assert_eq!(until, Some(t0 + ACTIVE_FOR));
    let later = t0 + Duration::from_secs(15);
    assert_eq!(extend(until, later, false), Some(later + ACTIVE_FOR));
    // Over: a refusal or a done does not bring it back.
    let after = t0 + Duration::from_secs(21);
    assert_eq!(extend(until, after, false), None);
    assert_eq!(extend(until, after, true), Some(after + ACTIVE_FOR));
}

#[test]
fn the_ghost_glides_softly_from_start_to_point() {
    let (a, b) = ((0.0, 100.0), (200.0, -100.0));
    for takes in [GLIDE, GLIDE_FAST, GLIDE_SLOW] {
        assert_eq!(glide_at(a, b, Duration::ZERO, takes), a);
        assert_eq!(glide_at(a, b, takes, takes), b);
        assert_eq!(glide_at(a, b, takes * 3, takes), b);
        let half = glide_at(a, b, takes / 2, takes);
        assert!((half.0 - 100.0).abs() < 0.01 && half.1.abs() < 0.01);
        // Slow at the start: a tenth of the time is far less than a
        // tenth of the way.
        assert!(glide_at(a, b, takes / 10, takes).0 < 10.0);
    }
    // A hold goes nowhere.
    assert_eq!(glide_at(a, a, GLIDE / 2, GLIDE), a);
}

#[test]
fn the_glide_counts_down_its_bar() {
    for takes in [GLIDE, GLIDE_FAST] {
        assert_eq!(left_of_glide(Duration::ZERO, takes), 1.0);
        assert!((left_of_glide(takes / 4, takes) - 0.75).abs() < 1e-4);
        assert_eq!(left_of_glide(takes * 2, takes), 0.0);
    }
}

#[test]
fn the_slider_runs_from_slow_on_the_left_to_fast_on_the_right() {
    assert_eq!(glide_of_slider(0.0), GLIDE_SLOW);
    assert_eq!(glide_of_slider(1.0), GLIDE_FAST);
    assert_eq!(glide_of_slider(-3.0), GLIDE_SLOW);
    assert_eq!(glide_of_slider(7.0), GLIDE_FAST);
    assert_eq!(glide_of_slider(f32::NAN), GLIDE_SLOW);
    // Whole tenths only, as the label shows.
    for i in 0..=100 {
        let d = glide_of_slider(i as f32 / 100.0);
        assert_eq!(d.as_millis() % 100, 0, "{d:?}");
        assert!((GLIDE_FAST..=GLIDE_SLOW).contains(&d));
    }
    // And back: a glide's place gives the same glide.
    for ms in [300, 1000, 2000, 3700, 4000] {
        let d = Duration::from_millis(ms);
        assert_eq!(glide_of_slider(slider_of_glide(d)), d);
    }
    assert_eq!(slider_of_glide(Duration::from_secs(60)), 0.0);
    assert_eq!(slider_of_glide(Duration::ZERO), 1.0);
}

#[test]
fn the_slider_takes_its_place_from_the_mouse() {
    assert_eq!(slider_at(SLIDER_X), 0.0);
    assert_eq!(slider_at(SLIDER_X + SLIDER_W), 1.0);
    assert_eq!(slider_at(SLIDER_X + SLIDER_W / 2.0), 0.5);
    assert_eq!(slider_at(0.0), 0.0);
    assert_eq!(slider_at(BOX_W), 1.0);
    assert_eq!(speed_label(GLIDE), "2.0 s");
    assert_eq!(speed_label(GLIDE_FAST), "0.3 s");
}

#[test]
fn the_speed_is_kept_only_when_moved_from_the_default() {
    assert_eq!(glide_to_saved(GLIDE), None);
    assert_eq!(glide_from_saved(None), GLIDE);
    let d = Duration::from_millis(700);
    assert_eq!(glide_from_saved(glide_to_saved(d)), d);
    // A file edited by hand stays between the ends.
    assert_eq!(glide_from_saved(Some(10)), GLIDE_FAST);
    assert_eq!(glide_from_saved(Some(60_000)), GLIDE_SLOW);
}

#[test]
fn the_mouse_finds_the_controls_and_nothing_else() {
    let mid = CONTROLS_Y + CONTROLS_H / 2.0;
    assert_eq!(control_at(SLIDER_X + 10.0, mid, None), Some(Control::Speed));
    assert_eq!(control_at(SLIDER_X - 4.0, mid, None), Some(Control::Speed));
    assert_eq!(control_at(BYPASS_X + 5.0, mid, None), Some(Control::Bypass));
    assert_eq!(
        control_at(BOX_W - PAD - 1.0, mid, None),
        Some(Control::Bypass)
    );
    // The title, the label, the gap and the history are not controls.
    assert_eq!(control_at(SLIDER_X + 10.0, 20.0, None), None);
    assert_eq!(control_at(PAD + 2.0, mid, None), None);
    assert_eq!(control_at(BYPASS_X - 4.0, mid, None), None);
    assert_eq!(
        control_at(BYPASS_X + 5.0, CONTROLS_Y + CONTROLS_H, None),
        None
    );
    assert_eq!(control_at(BOX_W - 2.0, mid, None), None);
}

#[test]
fn the_guard_tells_the_agent_what_opened() {
    assert_eq!(
        guard_error("Synchronization", guard::Kind::Dialog, false),
        "stopped: the window \"Synchronization\" opened while the ghost was on its way; \
         look again before clicking"
    );
    assert_eq!(
        guard_error(" ", guard::Kind::Front, true),
        "stopped: a window with no title came to the front while Horadric held the \
         action; look again before acting"
    );
    assert_eq!(
        guard_outcome("Save As", guard::Kind::Shown),
        "stopped: the window \"Save As\" opened"
    );
}

#[test]
fn a_glide_that_arrived_waits_while_the_human_has_esc_down() {
    assert!(!may_land(false, false));
    assert!(may_land(true, false));
    // The hold is not long enough yet: it must not lose to the landing.
    assert!(!may_land(true, true));
    // The fastest glide is over before the hold counts, so this is what
    // lets the human stop it at all.
    assert!(GLIDE_FAST < ESC_HOLD);
}

#[test]
fn esc_halts_what_glides_and_ends_bypass_even_with_nothing_on_its_way() {
    assert_eq!(esc_does(true, false), (true, false));
    assert_eq!(esc_does(true, true), (true, true));
    assert_eq!(esc_does(false, true), (false, true));
    assert_eq!(esc_does(false, false), (false, false));
}

#[test]
fn only_what_the_human_allowed_goes_without_esc() {
    assert!(may_glide(false, false));
    assert!(may_glide(false, true));
    assert!(may_glide(true, true));
    assert!(!may_glide(true, false));
}

#[test]
fn a_trip_stops_only_the_glide_it_came_for() {
    assert!(trip_stops(4, Some(4)));
    assert!(!trip_stops(3, Some(4)));
    assert!(!trip_stops(4, None));
}

#[test]
fn now_says_when_the_session_works_elsewhere() {
    let studio = "Acme Studio";
    let click = Some("mcp__horadric__takeover_click");
    assert_eq!(away_of(click, Turn::Working, false, studio), None);
    assert_eq!(away_of(None, Turn::Working, false, studio), None);
    assert_eq!(
        away_of(Some("Bash"), Turn::Working, false, studio),
        Some("Working in the shell (Bash), not in Acme Studio.".into())
    );
    assert_eq!(
        away_of(
            Some("mcp__horadric__browser_open"),
            Turn::Working,
            false,
            ""
        ),
        Some("Working in the browser (browser_open), not in the program.".into())
    );
    assert!(away_of(click, Turn::Resting, false, studio)
        .unwrap()
        .starts_with("Finished its turn"));
    assert_eq!(
        away_of(None, Turn::Gone, false, studio),
        Some("The session has ended.".into())
    );
    // Stuck on a prompt of its agent's, it does nothing in the program.
    assert!(away_of(click, Turn::Working, true, studio)
        .unwrap()
        .starts_with("Waiting for you"));
}

#[test]
fn tools_are_placed_in_words() {
    // Both families of names: the tools were first shipped as desktop_.
    assert!(is_takeover_tool("takeover_type"));
    assert!(is_takeover_tool("mcp__horadric__takeover_click"));
    assert!(is_takeover_tool("desktop_type"));
    assert!(is_takeover_tool("mcp__horadric__desktop_text"));
    assert!(!is_takeover_tool("mcp__horadric__browser_click"));
    assert!(!is_takeover_tool("mcp__horadric__desk_click"));
    assert!(!is_takeover_tool("Bash"));
    assert_eq!(tool_place("PowerShell"), "in the shell");
    assert_eq!(tool_place("Edit"), "on files");
    assert_eq!(
        tool_place("mcp__acme__studio_project_build"),
        "with studio_project_build"
    );
}

#[test]
fn a_session_works_until_its_turn_is_over() {
    let at = |p: Phase| turn_of(Some(&p));
    assert_eq!(at(Phase::Working), Turn::Working);
    assert_eq!(at(Phase::Waiting(WaitReason::Permission)), Turn::Working);
    assert_eq!(at(Phase::Waiting(WaitReason::Dialog)), Turn::Working);
    assert_eq!(at(Phase::Done), Turn::Resting);
    assert_eq!(at(Phase::Idle), Turn::Resting);
    assert_eq!(at(Phase::Waiting(WaitReason::Input)), Turn::Resting);
    assert_eq!(
        at(Phase::Waiting(WaitReason::Error("x".into()))),
        Turn::Resting
    );
    assert_eq!(at(Phase::Ended), Turn::Gone);
    assert_eq!(at(Phase::Paused), Turn::Gone);
    assert_eq!(turn_of(None), Turn::Gone);
}

#[test]
fn a_session_on_a_prompt_of_its_agents_waits_for_the_human() {
    let asks = |p: Phase| asks_human(Some(&p));
    assert!(asks(Phase::Waiting(WaitReason::Permission)));
    assert!(asks(Phase::Waiting(WaitReason::Dialog)));
    assert!(!asks(Phase::Working));
    assert!(!asks(Phase::Waiting(WaitReason::Input)));
    assert!(!asks_human(None));
}

#[test]
fn the_history_says_when_bypass_began_and_how_it_ended() {
    let line = |end| {
        let (what, outcome) = bypass_words(end);
        entry_line(&Entry {
            at: String::new(),
            mark: Mark::Ok,
            what: what.into(),
            outcome: outcome.into(),
            unasked: false,
        })
    };
    assert_eq!(line(None), "Bypass on");
    assert_eq!(line(Some(BypassEnd::Esc)), "Bypass off: Esc held");
    assert_eq!(
        line(Some(BypassEnd::Ceiling)),
        "Bypass off: on for 30 minutes, its limit"
    );
    assert_eq!(line(Some(BypassEnd::Switched)), "Bypass off: switched off");
    assert_eq!(
        line(Some(BypassEnd::Done)),
        "Bypass off: the work it was on for is done"
    );
}

#[test]
fn bypass_ends_when_the_work_it_was_on_for_is_done() {
    let now = Instant::now();
    let mut held = HashMap::new();
    // On for nobody, it has no work to end with and is over at once.
    assert!(bypass_over(&held, false));
    assert!(!bypass_over(&held, true));
    held.insert("a".to_string(), Holder::default());
    held.get_mut("a")
        .unwrap()
        .saw(Turn::Working, false, false, now);
    assert!(!bypass_over(&held, false));
    // Done, the Stop hook's phase, after working.
    held.get_mut("a")
        .unwrap()
        .saw(Turn::Resting, false, false, now);
    assert!(bypass_over(&held, false));
    // Not while the last action is still on its way.
    assert!(!bypass_over(&held, true));
    // A second session still at work keeps it on.
    held.insert("b".to_string(), Holder::default());
    held.get_mut("b").unwrap().called();
    assert!(!bypass_over(&held, false));
    // Its Stop, StopFailure or SessionEnd ends its share.
    held.get_mut("b").unwrap().ended = true;
    assert!(bypass_over(&held, false));
}

#[test]
fn a_resting_session_gets_time_to_start_working() {
    let now = Instant::now();
    let mut h = Holder::default();
    h.saw(Turn::Resting, false, false, now);
    assert!(!h.ended, "turned on before its next prompt");
    h.saw(Turn::Resting, false, true, now);
    assert!(h.ended, "rested the whole wait");
    let mut gone = Holder::default();
    gone.saw(Turn::Gone, false, false, now);
    assert!(gone.ended);
    // Operating again while bypass is on starts its share over.
    gone.called();
    assert_eq!(
        gone,
        Holder {
            worked: true,
            ended: false,
            asking: None,
        }
    );
    // Working after its turn ended does not undo the end the hook told.
    gone.ended = true;
    gone.saw(Turn::Working, false, false, now);
    assert!(gone.ended);
}

#[test]
fn a_session_waiting_for_the_human_does_not_keep_bypass() {
    let start = Instant::now();
    let mut h = Holder::default();
    h.called();
    // A permission prompt mid turn: the human may be right there.
    h.saw(Turn::Working, true, false, start);
    assert!(!h.ended);
    h.saw(Turn::Working, true, false, start + BYPASS_IDLE / 2);
    assert!(!h.ended);
    // Answered, it works on and the wait starts over next time.
    h.saw(Turn::Working, false, false, start + BYPASS_IDLE / 2);
    h.saw(Turn::Working, true, false, start + BYPASS_IDLE);
    h.saw(
        Turn::Working,
        true,
        false,
        start + BYPASS_IDLE + BYPASS_IDLE / 2,
    );
    assert!(!h.ended);
    // Nobody came for the whole wait: its work is not going on.
    h.saw(Turn::Working, true, false, start + 2 * BYPASS_IDLE);
    assert!(h.ended);
}

#[test]
fn only_operating_keeps_bypass_and_never_past_its_limit() {
    // A click has a point; what only bypass let through was held.
    assert!(keeps_bypass(true, false));
    assert!(keeps_bypass(false, true));
    // A look: a screenshot, a snapshot, a wait.
    assert!(!keeps_bypass(false, false));
    assert!(!bypass_spent(BYPASS_MAX - Duration::from_secs(1)));
    assert!(bypass_spent(BYPASS_MAX));
    assert_eq!(BYPASS_MAX, Duration::from_secs(30 * 60));
}

#[test]
fn what_bypass_let_through_is_marked_once() {
    let mut list = VecDeque::new();
    note_unasked(&mut list, "type \"x\"");
    note_unasked(&mut list, "click OK");
    assert!(take_unasked(&mut list, "click OK"));
    assert!(!take_unasked(&mut list, "click OK"));
    assert!(take_unasked(&mut list, "type \"x\""));
    for n in 0..HISTORY + 4 {
        note_unasked(&mut list, &format!("click {n}"));
    }
    assert_eq!(list.len(), HISTORY);
    assert!(!take_unasked(&mut list, "click 0"));
}

#[test]
fn the_breath_of_light_swells_as_it_comes_and_fades_behind() {
    // At its head it is full, a swell ahead not there yet.
    assert!((frame::breath(0.0) - 1.0).abs() < 1e-4);
    assert!(frame::breath(frame::SWELL) < 1e-4);
    assert_eq!(frame::breath(frame::SWELL * 3.0), 0.0);
    // Brighter the nearer it comes, dimmer the longer it is gone.
    assert!(frame::breath(40.0) > frame::breath(200.0));
    assert!(frame::breath(-40.0) > frame::breath(-300.0));
    assert_eq!(frame::breath(-100_000.0), 0.0);
    // No jump where coming turns to going.
    assert!((frame::breath(0.5) - frame::breath(-0.5)).abs() < 0.02);
}

#[test]
fn a_wave_lies_a_whole_number_of_times_along_its_edge() {
    for long in [1440.0, 2560.0, 3440.0, 300.0] {
        for about in [520.0, 190.0] {
            let wave = frame::fitted(about, long);
            let times = long / wave;
            assert!((times - times.round()).abs() < 1e-3, "{long} {about}");
            assert!(times >= 1.0);
        }
    }
}

#[test]
fn a_light_is_placed_on_each_edge_from_where_it_is_on_the_ring() {
    let (w, h) = (3440.0, 1440.0);
    let len = 2.0 * (w + h);
    // Halfway along the top, it is that far along the top.
    assert_eq!(frame::lap_from(1720.0, 0.0, len), 1720.0);
    // Seen from the right edge, which starts where the top ends, it has
    // most of a lap to go, and so is far before that edge.
    let right = frame::lap_from(1720.0, w, len);
    assert!((right - (1720.0 - w + len)).abs() < 1e-2 || (right - (1720.0 - w)).abs() < 1e-2);
    // Its front reaches an edge one swell before its head does.
    assert_eq!(frame::lap_from(w - 100.0, w, len), -100.0);
    // Always within one lap, starting a swell before the edge.
    for head in (0..20000).step_by(137) {
        for start in [0.0, w, w + h, 2.0 * w + h] {
            let at = frame::lap_from(head as f32, start, len);
            assert!(
                (-frame::SWELL..len - frame::SWELL).contains(&at),
                "{head} {start}: {at}"
            );
        }
    }
}

#[test]
fn the_ghost_stays_then_fades() {
    assert_eq!(ghost_alpha(Duration::ZERO), 1.0);
    assert_eq!(ghost_alpha(GHOST_STAYS), 1.0);
    assert!((ghost_alpha(GHOST_STAYS + GHOST_FADE / 2) - 0.5).abs() < 1e-4);
    assert_eq!(ghost_alpha(GHOST_STAYS + GHOST_FADE), 0.0);
}

#[test]
fn the_plate_sits_in_the_top_right_of_the_work_area() {
    assert_eq!(box_place([0, 0, 1920, 1040], (380, 200), 18), (1522, 18));
    assert_eq!(box_place([-2560, 40, 0, 1440], (380, 200), 18), (-398, 58));
    // Narrower than the plate: it keeps its left edge on the screen.
    assert_eq!(box_place([100, 0, 300, 600], (380, 200), 18), (100, 18));
}

#[test]
fn the_plate_grows_with_its_reason_and_history() {
    let bare = box_height(0.0, 0, false, 0.0);
    // Room for the controls under the title.
    assert!(bare >= CONTROLS_Y + CONTROLS_STEP + 22.0 + ROW);
    assert!(box_height(17.0, 0, false, 0.0) > bare);
    assert_eq!(
        box_height(500.0, 0, false, 0.0),
        box_height(REASON_LINES * REASON_LINE, 0, false, 0.0)
    );
    let one = box_height(0.0, 1, false, 0.0);
    assert!(one > bare);
    assert_eq!(box_height(0.0, 3, false, 0.0) - one, 2.0 * ROW);
    // And for the bypass band while it is on.
    assert_eq!(box_height(0.0, 1, true, 0.0) - one, BANNER);
}

#[test]
fn history_keeps_the_newest_on_top() {
    let mut h = VecDeque::new();
    for n in 0..HISTORY + 3 {
        remember(&mut h, entry(n));
    }
    assert_eq!(h.len(), HISTORY);
    assert_eq!(h.front(), Some(&entry(HISTORY + 2)));
    assert_eq!(h.back(), Some(&entry(3)));
}

#[test]
fn history_lines_read_what_then_how_it_went() {
    let mut e = entry(1);
    assert_eq!(entry_line(&e), "click 1");
    e.outcome = "dialog opened".into();
    assert_eq!(entry_line(&e), "click 1: dialog opened");
    e.what = " ".into();
    assert_eq!(entry_line(&e), "dialog opened");
    e.unasked = true;
    assert_eq!(entry_line(&e), "unasked: dialog opened");
    e.outcome.clear();
    assert_eq!(entry_line(&e), "unasked");
    assert_eq!(clock(7, 3, 9), "07:03:09");
}

#[test]
fn each_mark_has_its_own_glyph() {
    let marks = [Mark::Ok, Mark::Failed, Mark::Refused, Mark::Stopped];
    let glyphs: std::collections::HashSet<char> = marks.iter().map(|m| m.glyph()).collect();
    assert_eq!(glyphs.len(), marks.len());
}

fn hook(name: &str, tool: &str, id: &str) -> HookEvent {
    HookEvent {
        tool_name: Some(tool.into()),
        tool_use_id: Some(id.into()),
        tool_input: Some(json!({"command": "dir C:\\", "description": "List the drive"})),
        ..HookEvent::synthetic(name)
    }
}

fn shell(id: &str, tool: &str, cmd: &str) -> Shell {
    Shell {
        id: id.into(),
        tool: tool.into(),
        command: cmd.into(),
        description: String::new(),
        agent: false,
        at: "10:00:00".into(),
        state: ShellState::Running,
        // Each its own, as the hook reader numbers them.
        serial: SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
    }
}

static SERIAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// What a run of control keeps of the shells: the plate's lists and the
/// log's ledger.
#[derive(Default)]
struct Heard {
    lists: HashMap<String, VecDeque<Shell>>,
    ledger: Ledger,
}

impl Heard {
    fn hear(&mut self, note: ShellNote) -> Vec<Shell> {
        hear_shell(&mut self.lists, &mut self.ledger, note, true)
    }
}

fn began(id: &str, cmd: &str) -> ShellNote {
    ShellNote::Began {
        session: "s".into(),
        shell: shell(id, "PowerShell", cmd),
    }
}

fn ended(id: &str, tool: &str, state: ShellState) -> ShellNote {
    ShellNote::Ended {
        session: "s".into(),
        id: id.into(),
        tool: tool.into(),
        command: String::new(),
        state,
    }
}

#[test]
fn only_shell_tools_are_listed() {
    assert!(is_shell_tool("Bash"));
    assert!(is_shell_tool("PowerShell"));
    assert!(is_shell_tool("mcp__x__Bash"));
    assert!(!is_shell_tool("BashOutput"));
    assert!(!is_shell_tool("Read"));
    assert!(shell_note("s", &hook("PreToolUse", "Read", "t"), "10:00:00").is_none());
    assert!(shell_note("s", &hook("Notification", "Bash", "t"), "10:00:00").is_none());
}

#[test]
fn the_main_loops_turn_edges_end_what_still_runs() {
    for name in ["Stop", "StopFailure", "SessionEnd", "UserPromptSubmit"] {
        assert_eq!(
            shell_note("s", &HookEvent::synthetic(name), "t"),
            Some(ShellNote::TurnEnded {
                session: "s".into()
            })
        );
    }
    let sub = HookEvent {
        agent_id: Some("a".into()),
        ..HookEvent::synthetic("Stop")
    };
    assert!(shell_note("s", &sub, "t").is_none());
}

#[test]
fn a_shell_command_begins_with_what_it_runs_and_why() {
    let mut e = hook("PreToolUse", "PowerShell", "t1");
    e.agent_id = Some("a1".into());
    let Some(ShellNote::Began { session, shell }) = shell_note("tile", &e, "10:00:00") else {
        panic!("not a start");
    };
    assert_eq!(session, "tile");
    assert_eq!(shell.id, "t1");
    assert_eq!(shell.tool, "PowerShell");
    assert_eq!(shell.command, "dir C:\\");
    assert_eq!(shell.title(), "List the drive");
    assert!(shell.agent);
    assert_eq!(shell.state, ShellState::Running);
    let bare = Shell {
        description: " ".into(),
        ..shell
    };
    assert_eq!(bare.title(), "PowerShell command");
}

#[test]
fn a_shell_command_ends_as_its_result_says() {
    let input = json!({"command": "x"});
    let ok = json!({"stdout": "hi", "stderr": "", "interrupted": false});
    assert_eq!(
        shell_ended("Bash", &input, &ok, None, false),
        ShellState::Done
    );
    assert_eq!(
        shell_ended("Bash", &input, &json!("plain text"), None, false),
        ShellState::Done
    );
    let cut = json!({"stdout": "", "interrupted": true});
    assert_eq!(
        shell_ended("Bash", &input, &cut, None, false),
        ShellState::Interrupted
    );
    let late = json!({"stdout": "", "backgroundTaskId": "b1", "timedOutAfterMs": 120000});
    assert_eq!(
        shell_ended("Bash", &input, &late, None, false),
        ShellState::TimedOut
    );
    let sent = json!({"stdout": "", "backgroundTaskId": "b1"});
    assert_eq!(
        shell_ended("Bash", &input, &sent, None, false),
        ShellState::Background
    );
    let null = serde_json::Value::Null;
    assert_eq!(
        shell_ended(
            "PowerShell",
            &input,
            &null,
            Some("Exit code 3\nnope"),
            false
        ),
        ShellState::Failed(Some(3))
    );
    assert_eq!(
        shell_ended("PowerShell", &input, &null, Some("it broke"), false),
        ShellState::Failed(None)
    );
    assert_eq!(
        shell_ended("PowerShell", &input, &null, Some("Exit code 1"), true),
        ShellState::Interrupted
    );
}

#[test]
fn a_failure_hook_carries_its_exit_code() {
    let mut e = hook("PostToolUseFailure", "Bash", "t1");
    e.error = Some(json!("Exit code 127\ncommand not found"));
    let Some(ShellNote::Ended {
        id, tool, state, ..
    }) = shell_note("s", &e, "10:00:01")
    else {
        panic!("not an end");
    };
    assert_eq!(id, "t1");
    assert_eq!(tool, "Bash");
    assert_eq!(state, ShellState::Failed(Some(127)));
    let mut stop = hook("PostToolUseFailure", "Bash", "t2");
    stop.is_interrupt = Some(json!(true));
    assert!(matches!(
        shell_note("s", &stop, "10:00:01"),
        Some(ShellNote::Ended {
            state: ShellState::Interrupted,
            ..
        })
    ));
}

#[test]
fn the_list_keeps_the_newest_on_top_and_ends_only_what_it_saw_begin() {
    let mut h = Heard::default();
    for i in 0..SHELLS + 2 {
        h.hear(began(&format!("t{i}"), &format!("cmd {i}")));
    }
    let list = &h.lists["s"];
    assert_eq!(list.len(), SHELLS);
    assert_eq!(list[0].command, format!("cmd {}", SHELLS + 1));
    let done = h.hear(ended("t9", "PowerShell", ShellState::Done));
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].command, "cmd 9");
    assert_eq!(h.lists["s"][0].state, ShellState::Done);
    // Ended twice, or begun before control: nothing to end.
    let again = ended("t9", "PowerShell", ShellState::Failed(Some(1)));
    assert!(h.hear(again).is_empty());
    assert!(h
        .hear(ended("never", "PowerShell", ShellState::Done))
        .is_empty());
    let other = ShellNote::Ended {
        session: "x".into(),
        id: "t8".into(),
        tool: "PowerShell".into(),
        command: "cmd 8".into(),
        state: ShellState::Done,
    };
    assert!(h.hear(other).is_empty());
    assert_eq!(h.lists["s"][1].state, ShellState::Running);
}

#[test]
fn a_command_pushed_off_the_list_still_gets_its_end_in_the_log() {
    let mut h = Heard::default();
    for i in 0..SHELLS + 2 {
        h.hear(began(&format!("t{i}"), &format!("cmd {i}")));
    }
    // The plate lists eight; the first two are off it and still owed.
    assert!(h.lists["s"].iter().all(|s| s.command != "cmd 0"));
    let done = h.hear(ended("t0", "PowerShell", ShellState::Failed(Some(2))));
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].command, "cmd 0");
    assert_eq!(done[0].state, ShellState::Failed(Some(2)));
    // A turn's end stops every one still owed, listed or not.
    let stopped = h.hear(ShellNote::TurnEnded {
        session: "s".into(),
    });
    assert_eq!(stopped.len(), SHELLS + 1);
    assert!(h.ledger.is_empty());
}

#[test]
fn a_command_still_running_when_control_rests_gets_its_end_and_nothing_new_begins() {
    let mut h = Heard::default();
    h.hear(began("a", "long build"));
    // Control rests: the plate's lists go, the ledger stays.
    h.lists.clear();
    let trail = |h: &mut Heard, note| hear_shell(&mut h.lists, &mut h.ledger, note, false);
    assert!(trail(&mut h, began("b", "begun after control")).is_empty());
    assert!(
        h.lists.is_empty(),
        "nothing is listed for a run that is over"
    );
    let done = trail(&mut h, ended("a", "PowerShell", ShellState::Done));
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].command, "long build");
    assert!(shell_log_line("t", "s", &done[0]).contains("shell command done"));
    assert!(h.ledger.is_empty());
    assert!(trail(&mut h, ended("b", "PowerShell", ShellState::Done)).is_empty());
}

#[test]
fn the_ledger_gives_up_the_oldest_past_its_limit() {
    let mut h = Heard::default();
    for i in 0..LEDGER + 3 {
        h.hear(began(&format!("t{i}"), "x"));
    }
    assert_eq!(h.ledger["s"].len(), LEDGER);
    assert!(h
        .hear(ended("t0", "PowerShell", ShellState::Done))
        .is_empty());
}

#[test]
fn a_denied_command_ends_as_denied_and_not_as_stopped() {
    let denied = shell_note("s", &hook("PermissionDenied", "Bash", "t1"), "10:00:00");
    assert!(matches!(
        denied,
        Some(ShellNote::Ended {
            ref id,
            state: ShellState::Denied,
            ..
        }) if id == "t1"
    ));
    let mut h = Heard::default();
    h.hear(began("t1", "rm -rf build"));
    let ended = h.hear(denied.unwrap());
    assert_eq!(ended[0].state, ShellState::Denied);
    assert_eq!(ShellState::Denied.word(), "denied, not run");
    assert!(shell_log_line("t", "s", &ended[0]).contains("shell command denied, not run"));
    // Its turn's end has nothing left of it to call stopped.
    assert!(h
        .hear(ShellNote::TurnEnded {
            session: "s".into()
        })
        .is_empty());
}

#[test]
fn an_end_without_an_id_ends_the_oldest_of_its_shell_still_running() {
    let mut h = Heard::default();
    let begin = |h: &mut Heard, tool: &str, cmd: &str| {
        h.hear(ShellNote::Began {
            session: "s".into(),
            shell: shell("", tool, cmd),
        })
    };
    begin(&mut h, "Bash", "first");
    begin(&mut h, "PowerShell", "second");
    begin(&mut h, "Bash", "third");
    let done = h.hear(ended("", "Bash", ShellState::Done));
    assert_eq!(done[0].command, "first");
    let done = h.hear(ended("", "Bash", ShellState::Done));
    assert_eq!(done[0].command, "third");
    assert!(h.hear(ended("", "Bash", ShellState::Done)).is_empty());
    assert_eq!(h.lists["s"][1].state, ShellState::Running);

    // Its command, when it says one, picks among them.
    begin(&mut h, "Bash", "older");
    begin(&mut h, "Bash", "mine");
    let by_command = ShellNote::Ended {
        session: "s".into(),
        id: String::new(),
        tool: "Bash".into(),
        command: "mine".into(),
        state: ShellState::Done,
    };
    assert_eq!(h.hear(by_command)[0].command, "mine");
    assert_eq!(h.lists["s"][1].state, ShellState::Running);
}

#[test]
fn a_turns_end_stops_what_never_got_its_end() {
    let mut h = Heard::default();
    h.hear(began("a", "x"));
    h.hear(began("b", "y"));
    h.hear(ended("a", "PowerShell", ShellState::Done));
    let turn = |session: &str| ShellNote::TurnEnded {
        session: session.into(),
    };
    assert!(h.hear(turn("other")).is_empty());
    let stopped = h.hear(turn("s"));
    assert_eq!(stopped.len(), 1);
    assert_eq!(stopped[0].command, "y");
    assert_eq!(h.lists["s"][0].state, ShellState::Stopped);
    assert_eq!(h.lists["s"][1].state, ShellState::Done);
    assert!(h.hear(turn("s")).is_empty());
}

#[test]
fn the_fold_counts_what_runs() {
    let mut h = Heard::default();
    assert_eq!(shells_count(&VecDeque::new()), "none yet");
    h.hear(began("a", "x"));
    h.hear(began("b", "y"));
    assert_eq!(shells_count(&h.lists["s"]), "2, 2 running");
    h.lists.get_mut("s").unwrap()[0].state = ShellState::Done;
    h.lists.get_mut("s").unwrap()[1].state = ShellState::Done;
    assert_eq!(shells_count(&h.lists["s"]), "2");
}

#[test]
fn each_shell_state_reads_and_shows_its_own_way() {
    assert_eq!(ShellState::Failed(Some(2)).word(), "exit 2");
    assert_eq!(ShellState::Failed(None).word(), "failed");
    assert_eq!(ShellState::TimedOut.word(), "timed out, in background");
    assert_eq!(ShellState::Stopped.word(), "stopped");
    let states = [
        ShellState::Running,
        ShellState::Done,
        ShellState::Failed(None),
        ShellState::Interrupted,
        ShellState::TimedOut,
        ShellState::Stopped,
    ];
    let glyphs: std::collections::HashSet<char> = states.iter().map(|s| s.glyph()).collect();
    assert_eq!(glyphs.len(), states.len());
}

#[test]
fn a_command_counts_its_lines_past_what_is_laid_out() {
    assert_eq!(command_lines(3, "a\nb\nc"), 3);
    assert_eq!(command_lines(0, ""), 1);
    let long = format!("{}\nmore\nand more", "x".repeat(COMMAND_CHARS));
    assert_eq!(command_lines(40, &long), 42);
    assert_eq!(more_lines(0), None);
    assert_eq!(more_lines(1).as_deref(), Some("+1 line"));
    assert_eq!(more_lines(12).as_deref(), Some("+12 lines"));
    let wide = "é".repeat(COMMAND_CHARS + 10);
    assert_eq!(command_shown(&wide).chars().count(), COMMAND_CHARS);
    assert_eq!(command_shown("ls"), "ls");
}

#[test]
fn the_shell_section_sits_under_the_rest_and_grows_with_its_commands() {
    let body = body_height(0.0, 2, false);
    let fit = Fit {
        shown: vec![1, 2],
        more: 0,
        note: false,
    };
    let folded = shells_height(false, &fit, 14.0);
    assert_eq!(folded, SHELLS_GAP + FOLD_H);
    let open = shells_height(true, &fit, 14.0);
    assert_eq!(open - folded, shell_row(1, 14.0) + shell_row(2, 14.0));
    let noted = Fit { note: true, ..fit };
    assert_eq!(shells_height(true, &noted, 14.0), open + ROW);
    assert_eq!(box_height(0.0, 2, false, open), (body + open + PAD).ceil());
    assert_eq!(box_height(0.0, 2, false, 0.0), (body + PAD).ceil());
}

#[test]
fn with_room_every_command_shows_as_much_as_its_state_allows() {
    let rows = [(20, ShellState::Running), (5, ShellState::Done)];
    let fit = fit_shells(&rows, 14.0, 10_000.0);
    assert_eq!(fit.shown, vec![RUNNING_LINES, DONE_LINES]);
    assert_eq!((fit.more, fit.note), (0, false));
    let none = fit_shells(&[], 14.0, 10_000.0);
    assert!(none.shown.is_empty() && none.note);
    assert!(!fit_shells(&[], 14.0, SHELLS_GAP + FOLD_H).note);
}

#[test]
fn a_tall_running_command_gives_up_lines_before_anything_is_left_out() {
    let head = SHELLS_GAP + FOLD_H;
    let rows = [
        (20, ShellState::Running),
        (1, ShellState::Done),
        (1, ShellState::Done),
    ];
    // Room for all three at two lines and one line more.
    let room = head + shell_row(3, 14.0) + 2.0 * shell_row(1, 14.0);
    let fit = fit_shells(&rows, 14.0, room);
    assert_eq!(fit.shown, vec![3, 1, 1]);
    assert_eq!((fit.more, fit.note), (0, false));
    assert!(shells_height(true, &fit, 14.0) <= room);
}

#[test]
fn what_cannot_fit_is_left_out_and_counted() {
    let head = SHELLS_GAP + FOLD_H;
    let rows = [
        (20, ShellState::Running),
        (5, ShellState::Done),
        (5, ShellState::Done),
    ];
    let room = head + shell_row(2, 14.0) + ROW + 10.0;
    let fit = fit_shells(&rows, 14.0, room);
    assert_eq!(fit.shown, vec![2]);
    assert_eq!((fit.more, fit.note), (2, true));
    assert!(shells_height(true, &fit, 14.0) <= room);
    assert_eq!(shells_note(&fit), "2 more not shown, no room on the screen");
    let tight = fit_shells(&rows, 14.0, head + ROW);
    assert!(tight.shown.is_empty());
    assert_eq!((tight.more, tight.note), (3, true));
    let none = fit_shells(&rows, 14.0, 0.0);
    assert!(none.shown.is_empty() && !none.note);
    assert_eq!(
        shells_note(&Fit::default()),
        "No shell command since control began"
    );
}

#[test]
fn the_fold_header_is_a_control_and_the_rest_of_the_section_is_not() {
    let top = 300.0;
    let header = top + SHELLS_GAP + 4.0;
    assert_eq!(
        control_at(PAD + 2.0, header, Some(top)),
        Some(Control::Shells)
    );
    assert_eq!(
        control_at(BOX_W - PAD - 1.0, header, Some(top)),
        Some(Control::Shells)
    );
    assert_eq!(control_at(PAD + 2.0, header, None), None);
    assert_eq!(control_at(2.0, header, Some(top)), None);
    assert_eq!(control_at(PAD + 2.0, top + 2.0, Some(top)), None);
    assert_eq!(
        control_at(PAD + 2.0, top + SHELLS_GAP + FOLD_H + 2.0, Some(top)),
        None
    );
    let mid = CONTROLS_Y + CONTROLS_H / 2.0;
    assert_eq!(
        control_at(BYPASS_X + 5.0, mid, Some(top)),
        Some(Control::Bypass)
    );
}

#[test]
fn the_log_gives_the_command_at_its_start_and_only_a_glimpse_at_its_end() {
    let mut sh = Shell {
        description: "List processes".into(),
        ..shell("t", "PowerShell", "Get-Process |\n  Select -First 3")
    };
    assert_eq!(
        shell_log_line("2026-11-26T10:00:00Z", "fix plc", &sh),
        "2026-11-26T10:00:00Z  fix plc  shell command started  PowerShell  \
         (Get-Process | Select -First 3)  [List processes]\n"
    );
    sh.state = ShellState::Failed(Some(1));
    sh.agent = true;
    sh.description.clear();
    assert_eq!(
        shell_log_line("t", "s", &sh),
        "t  s  shell command exit 1 by a subagent  PowerShell  \
         (Get-Process | Select -First 3)\n"
    );
    sh.command = "y".repeat(COMMAND_CHARS * 2);
    let end = shell_log_line("t", "s", &sh);
    assert!(end.contains(&format!("({}...)", "y".repeat(LOG_END_CHARS))));
    sh.state = ShellState::Running;
    let start = shell_log_line("t", "s", &sh);
    assert!(start.contains(&format!("({})", "y".repeat(COMMAND_CHARS))));
}

#[test]
fn only_the_humans_own_press_on_the_slider_moves_it() {
    // A real press on the slider, held: it follows, wherever the pointer
    // goes from there.
    assert!(slider_follows(Some(Control::Speed), true));
    // A posted message, which the hook never saw, or one it saw let go.
    assert!(!slider_follows(None, true));
    assert!(!slider_follows(Some(Control::Speed), false));
    assert!(!slider_follows(None, false));
    // A press the human made elsewhere does not lend itself to the slider.
    assert!(!slider_follows(Some(Control::Bypass), true));
    assert!(!slider_follows(Some(Control::Shells), true));
}
