//! The dialogs that stand in on the Mac for the Windows Folio's cards:
//! a question's options as menus, a line comment on an edit's rows, and
//! an MCP server's form or page. AppKit's own alerts, with fields laid in
//! as their accessory view, so nothing is drawn.

use horadric_core::folio::{Block, ElicitAction, ElicitField, Elicitation, FieldKind, ELICIT_SEP};
use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSAlertSecondButtonReturn, NSApplication, NSButton,
    NSControlStateValueOn, NSPopUpButton, NSTextField, NSView,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};
use serde_json::{Map, Value};

use super::folio::{with, FolioView};
use super::menu::{self, Item};

/// How wide a form's fields are.
const FIELD_W: f64 = 320.0;
const LABEL_H: f64 = 18.0;
const FIELD_H: f64 = 24.0;
const GAP: f64 = 8.0;

fn mtm() -> MainThreadMarker {
    MainThreadMarker::new().expect("dialogs run on the main thread")
}

fn alert(title: &str, text: &str) -> Retained<NSAlert> {
    let mtm = mtm();
    let a = NSAlert::new(mtm);
    a.setMessageText(&NSString::from_str(title));
    a.setInformativeText(&NSString::from_str(text));
    NSApplication::sharedApplication(mtm).activate();
    a
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}

fn field(initial: &str, y: f64) -> Retained<NSTextField> {
    let f = NSTextField::initWithFrame(NSTextField::alloc(mtm()), rect(0.0, y, FIELD_W, FIELD_H));
    f.setStringValue(&NSString::from_str(initial));
    f
}

fn label(text: &str, y: f64) -> Retained<NSTextField> {
    let l = NSTextField::labelWithString(&NSString::from_str(text), mtm());
    l.setFrame(rect(0.0, y, FIELD_W, LABEL_H));
    l
}

/// The options of each of a question ask's questions, picked from a menu
/// at the bar's button, one menu a question. None when one was dismissed.
pub(super) fn pick_answers(
    id: &str,
    request_id: &str,
    view: &FolioView,
) -> Option<Vec<Vec<String>>> {
    let questions = with(id, |c| {
        c.folio.blocks.iter().find_map(|b| match b {
            Block::Question {
                request_id: r,
                questions,
                ..
            } if r == request_id => Some(questions.clone()),
            _ => None,
        })
    })
    .flatten()?;
    let mtm = MainThreadMarker::from(view);
    let at = view.bar_point();
    let mut picks = Vec::new();
    for q in &questions {
        let mut items = vec![Item::disabled(q.question.clone()), Item::Separator];
        for (i, o) in q.options.iter().enumerate() {
            items.push(Item::pick(o.label.clone(), i + 1));
        }
        let picked = menu::popup(mtm, &items, at)?;
        let label = q.options.get(picked.checked_sub(1)?)?.label.clone();
        picks.push(vec![label]);
    }
    Some(picks)
}

/// `3` or `3-5`, rows as the transcript numbers them, as the indices of
/// the first and last, in order.
fn rows(typed: &str) -> Option<(usize, usize)> {
    let typed = typed.trim();
    let (a, b) = match typed.split_once(['-', ' ', ',']) {
        Some((a, b)) => (a.trim(), b.trim()),
        None => (typed, typed),
    };
    let (a, b): (usize, usize) = (a.parse().ok()?, b.parse().ok()?);
    let (a, b) = (a.min(b), a.max(b));
    Some((a.checked_sub(1)?, b.checked_sub(1)?))
}

/// A line comment: which rows of the diff, by the numbers the transcript
/// shows, and what to say of them. None for Cancel.
pub(super) fn comment() -> Option<(usize, usize, String)> {
    let a = alert(
        "Comment on the edit",
        "Name the rows by their numbers in the diff above, as 12 or 12-15.",
    );
    a.addButtonWithTitle(&NSString::from_str("Add Comment"));
    a.addButtonWithTitle(&NSString::from_str("Cancel"));
    let h = 2.0 * (LABEL_H + FIELD_H) + GAP;
    let pane = NSView::initWithFrame(NSView::alloc(mtm()), rect(0.0, 0.0, FIELD_W, h));
    let row_label = label("Rows", h - LABEL_H);
    let row_field = field("", h - LABEL_H - FIELD_H);
    let text_label = label("Comment", FIELD_H);
    let text_field = field("", 0.0);
    for v in [&row_label, &row_field, &text_label, &text_field] {
        pane.addSubview(v);
    }
    a.setAccessoryView(Some(&pane));
    a.window().setInitialFirstResponder(Some(&row_field));
    loop {
        if a.runModal() != NSAlertFirstButtonReturn {
            return None;
        }
        let said = text_field.stringValue().to_string();
        match rows(&row_field.stringValue().to_string()) {
            Some((from, to)) if !said.trim().is_empty() => return Some((from, to, said)),
            Some(_) => super::dialog::error("No comment", "Say what to change about those rows."),
            None => super::dialog::error("Which rows?", "Name them as 12, or 12-15."),
        }
    }
}

/// One field of a form as the dialog holds it.
enum Control {
    Text(Retained<NSTextField>),
    Check(Retained<NSButton>),
    /// With whether an empty first item was put in, for a field that may
    /// be left alone.
    Pick(Retained<NSPopUpButton>, bool),
    Checks(Vec<Retained<NSButton>>),
}

impl Control {
    /// What the human gave, as the text the shared model reads.
    fn raw(&self, f: &ElicitField) -> String {
        match (self, &f.kind) {
            (Control::Text(t), _) => t.stringValue().to_string(),
            (Control::Check(b), _) => (b.state() == NSControlStateValueOn).to_string(),
            (Control::Pick(p, blank), FieldKind::Choice(options)) => {
                let i = p.indexOfSelectedItem();
                let i = if *blank { i - 1 } else { i };
                (usize::try_from(i).ok())
                    .and_then(|i| options.get(i))
                    .map(|(v, _)| v.clone())
                    .unwrap_or_default()
            }
            (Control::Checks(boxes), FieldKind::Choices { options, .. }) => (boxes.iter())
                .zip(options)
                .filter(|(b, _)| b.state() == NSControlStateValueOn)
                .map(|(_, (v, _))| v.as_str())
                .collect::<Vec<_>>()
                .join(&ELICIT_SEP.to_string()),
            _ => String::new(),
        }
    }
}

/// How tall a field's control is.
fn control_h(f: &ElicitField) -> f64 {
    match &f.kind {
        FieldKind::Choices { options, .. } => options.len().max(1) as f64 * (FIELD_H - 4.0),
        _ => FIELD_H,
    }
}

/// An MCP server's question as a dialog: a page to open, or a form, its
/// answers checked by the shared model. None when it was dismissed for
/// now, to answer later.
pub(super) fn elicit(e: &Elicitation) -> Option<(ElicitAction, Option<Map<String, Value>>)> {
    let title = match e.title.trim() {
        "" => format!("{} asks", e.server),
        t => t.to_string(),
    };
    if let Some(url) = &e.url {
        let a = alert(&title, &format!("{}\n\n{url}", e.message.trim()));
        a.addButtonWithTitle(&NSString::from_str("Open Page"));
        a.addButtonWithTitle(&NSString::from_str("Decline"));
        a.addButtonWithTitle(&NSString::from_str("Later"));
        return match a.runModal() {
            r if r == NSAlertFirstButtonReturn => {
                super::folio_commands::open_url(url);
                Some((ElicitAction::Accept, None))
            }
            r if r == NSAlertSecondButtonReturn => Some((ElicitAction::Decline, None)),
            _ => None,
        };
    }
    let a = alert(&title, e.message.trim());
    a.addButtonWithTitle(&NSString::from_str("Submit"));
    a.addButtonWithTitle(&NSString::from_str("Decline"));
    a.addButtonWithTitle(&NSString::from_str("Later"));
    let h: f64 = (e.fields.iter())
        .map(|f| LABEL_H + control_h(f) + GAP)
        .sum::<f64>()
        .max(1.0);
    let pane = NSView::initWithFrame(NSView::alloc(mtm()), rect(0.0, 0.0, FIELD_W, h));
    let mut controls = Vec::new();
    // AppKit counts up from the bottom: the first field goes at the top.
    let mut y = h;
    for f in &e.fields {
        y -= LABEL_H;
        let mut said = f.label.clone();
        if f.required {
            said.push_str(" *");
        }
        if !f.description.is_empty() {
            said.push_str(&format!(": {}", f.description));
        }
        pane.addSubview(&label(&said, y));
        let ch = control_h(f);
        y -= ch;
        let control = make_control(f, y);
        match &control {
            Control::Text(t) => pane.addSubview(t),
            Control::Check(b) => pane.addSubview(b),
            Control::Pick(p, _) => pane.addSubview(p),
            Control::Checks(boxes) => boxes.iter().for_each(|b| pane.addSubview(b)),
        }
        controls.push(control);
        y -= GAP;
    }
    a.setAccessoryView(Some(&pane));
    loop {
        match a.runModal() {
            r if r == NSAlertFirstButtonReturn => {
                let got = e.content(|name| {
                    let i = e.fields.iter().position(|f| f.name == name)?;
                    Some(controls[i].raw(&e.fields[i]))
                });
                match got {
                    Ok(content) => return Some((ElicitAction::Accept, Some(content))),
                    Err(why) => super::dialog::error("Not sent", &why),
                }
            }
            r if r == NSAlertSecondButtonReturn => return Some((ElicitAction::Decline, None)),
            _ => return None,
        }
    }
}

fn make_control(f: &ElicitField, y: f64) -> Control {
    let mtm = mtm();
    match &f.kind {
        FieldKind::Boolean => {
            let b = unsafe {
                NSButton::checkboxWithTitle_target_action(
                    &NSString::from_str(&f.label),
                    None,
                    None,
                    mtm,
                )
            };
            b.setFrame(rect(0.0, y, FIELD_W, FIELD_H));
            if f.default == "true" {
                b.setState(NSControlStateValueOn);
            }
            Control::Check(b)
        }
        FieldKind::Choice(options) => {
            let p = NSPopUpButton::initWithFrame_pullsDown(
                NSPopUpButton::alloc(mtm),
                rect(0.0, y, FIELD_W, FIELD_H),
                false,
            );
            let blank = !f.required;
            if blank {
                p.addItemWithTitle(&NSString::from_str(""));
            }
            for (_, l) in options {
                p.addItemWithTitle(&NSString::from_str(l));
            }
            if let Some(i) = options.iter().position(|(v, _)| *v == f.default) {
                p.selectItemAtIndex((i + usize::from(blank)) as isize);
            }
            Control::Pick(p, blank)
        }
        FieldKind::Choices { options, .. } => {
            let on: Vec<&str> = f.default.split(ELICIT_SEP).collect();
            let row = FIELD_H - 4.0;
            let top = y + options.len() as f64 * row;
            let boxes = (options.iter().enumerate())
                .map(|(i, (v, l))| {
                    let b = unsafe {
                        NSButton::checkboxWithTitle_target_action(
                            &NSString::from_str(l),
                            None,
                            None,
                            mtm,
                        )
                    };
                    b.setFrame(rect(0.0, top - (i + 1) as f64 * row, FIELD_W, row));
                    if on.contains(&v.as_str()) {
                        b.setState(NSControlStateValueOn);
                    }
                    b
                })
                .collect();
            Control::Checks(boxes)
        }
        FieldKind::Text { .. } | FieldKind::Number { .. } => Control::Text(field(&f.default, y)),
    }
}
