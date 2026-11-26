//! Sign in differently here: the browser pane's line that splits the
//! shown site off for its project, so it signs in there as another
//! account while every other site stays as the shared browser has it.
//! What a split means is `horadric_core::vault::split`; copying the
//! cookies is `crate::web`.

use std::path::Path;

use horadric_core::vault::split;

use super::identity::{self, Identity};
use crate::menu::Item;

const FRESH: usize = 0;
const GIVE_BACK: usize = 1;
const JOIN: usize = 10;
/// How many ids past its base the menu's lines take.
pub const SPAN: usize = 100;

/// The site the project's shown page is on.
fn shown_site(key: &str) -> Option<String> {
    let (_, url) = crate::web::label(key)?;
    split::site_of(&url)
}

/// The project's names, by folder, for a line to say who a group is.
fn names(keys: &[String]) -> String {
    let names: Vec<String> = keys
        .iter()
        .filter_map(|k| Some(Path::new(k).file_name()?.to_string_lossy().into_owned()))
        .collect();
    match names.as_slice() {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// What a group is called in a line: the projects in it, else its name.
fn group_label(g: &Identity) -> String {
    match names(&identity::members(&g.slug)) {
        n if n.is_empty() => g.name.clone(),
        n => n,
    }
}

/// The project's group, when it has one made by splitting. One made by
/// hand in Settings is a whole browser already, so it has no line.
enum Now {
    None,
    Group(Identity),
    Whole,
}

fn now(key: &str) -> Now {
    match crate::town::identity_for(key).and_then(|s| identity::get(&s)) {
        None => Now::None,
        Some(g) if g.sites.is_some() => Now::Group(g),
        Some(_) => Now::Whole,
    }
}

/// The groups the project could take `site` as, other than its own.
fn offers(key: &str, site: &str) -> Vec<Identity> {
    let all = identity::all();
    let own = crate::town::identity_for(key);
    split::offers(&all, site)
        .into_iter()
        .filter(|g| Some(&g.slug) != own.as_ref())
        .take(SPAN - JOIN)
        .cloned()
        .collect()
}

/// The browser menu's line for the shown site, None where there is none
/// to split (a file, a blank page, a linked machine's project).
pub fn menu_item(key: &str, base: usize) -> Option<Item> {
    if key.is_empty() || horadric_core::afar::split_key(key).is_some() {
        return None;
    }
    let site = shown_site(key)?;
    match now(key) {
        Now::Whole => None,
        Now::Group(g) if g.sites.as_ref().is_some_and(|s| s.contains(&site)) => {
            let members = identity::members(&g.slug);
            let back = match members.len() {
                0 | 1 => "Sign in as everywhere else".to_string(),
                _ => format!("Sign in as everywhere else, for {}", names(&members)),
            };
            Some(Item::Submenu(
                format!("Signed in differently on {site}"),
                vec![Item::action(base + GIVE_BACK, back)],
            ))
        }
        Now::Group(g) => {
            let members = identity::members(&g.slug);
            let label = match members.len() {
                0 | 1 => format!("Sign in differently on {site}"),
                _ => format!("Sign in differently on {site}, for {}", names(&members)),
            };
            Some(Item::action(base + FRESH, label))
        }
        Now::None => {
            let offers = offers(key, &site);
            if offers.is_empty() {
                return Some(Item::action(
                    base + FRESH,
                    format!("Sign in differently on {site}"),
                ));
            }
            let mut lines = vec![Item::action(base + FRESH, "Sign in fresh")];
            lines.push(Item::Separator);
            for (i, g) in offers.iter().enumerate() {
                let more: Vec<&str> = g
                    .sites
                    .iter()
                    .flatten()
                    .filter(|s| **s != site)
                    .map(String::as_str)
                    .collect();
                let also = match more.as_slice() {
                    [] => String::new(),
                    m => format!(" (also {})", m.join(", ")),
                };
                lines.push(Item::action(
                    base + JOIN + i,
                    format!("As in {}{also}", group_label(g)),
                ));
            }
            Some(Item::Submenu(
                format!("Sign in differently on {site}"),
                lines,
            ))
        }
    }
}

/// Does what was picked from [`menu_item`]'s lines. False when `picked`
/// is not one of them.
pub fn menu_picked(key: &str, base: usize, picked: usize) -> bool {
    if !(base..base + SPAN).contains(&picked) {
        return false;
    }
    let Some(site) = shown_site(key) else {
        return true;
    };
    let before = crate::town::identity_for(key);
    let result = match picked - base {
        GIVE_BACK => match before.as_deref() {
            Some(slug) => identity::unsplit_site(slug, &site).map(|()| {
                crate::web::unsplit(&crate::town::identity_profile(slug), &site);
            }),
            None => Ok(()),
        },
        FRESH => identity::split_site(key, &site, None).map(|_| ()),
        i => match offers(key, &site).get(i - JOIN) {
            Some(g) => identity::split_site(key, &site, Some(&g.slug)).map(|_| ()),
            None => return true,
        },
    };
    let ok = result.is_ok();
    super::settings::done("Sign in differently", result);
    if !ok {
        return true;
    }
    super::settings::stale();
    if crate::town::identity_for(key) != before {
        // A new group: its browser moves to the group's profile and its
        // sessions start again on the group's tools.
        crate::app::push(crate::app::Input::IdentityChanged(key.to_string()));
    } else if picked - base != GIVE_BACK {
        crate::web::resplit();
    }
    true
}
