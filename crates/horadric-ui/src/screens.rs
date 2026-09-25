//! Which screen the columns stand on. The choice is saved by the screen's
//! device name, and None follows whichever screen is primary, which is
//! what undocking a laptop wants.

/// One monitor as Windows reports it, in physical pixels.
#[derive(Debug, Clone, PartialEq)]
pub struct Screen {
    /// The device name, `\\.\DISPLAY2`. It stays with the output the
    /// screen is plugged into.
    pub name: String,
    /// The whole screen as (left, top, right, bottom).
    pub bounds: [i32; 4],
    /// The part the taskbar leaves free.
    pub work: [i32; 4],
    pub primary: bool,
}

/// The screen the columns stand on: the one chosen while it is plugged in,
/// otherwise the primary one. A chosen screen that went away is not
/// forgotten, so plugging it in again brings the columns back to it.
pub fn pick<'a>(screens: &'a [Screen], chosen: Option<&str>) -> Option<&'a Screen> {
    chosen
        .and_then(|name| screens.iter().find(|s| s.name == name))
        .or_else(|| screens.iter().find(|s| s.primary))
        .or_else(|| screens.first())
}

/// Left to right, then top to bottom, the way they stand on the desk and
/// in Display settings.
pub fn in_order(mut screens: Vec<Screen>) -> Vec<Screen> {
    screens.sort_by_key(|s| (s.bounds[0], s.bounds[1]));
    screens
}

/// A screen's line in the menu, numbered from 1 in `in_order`, with its
/// size right aligned after a tab.
pub fn label(n: usize, screen: &Screen) -> String {
    let w = screen.bounds[2] - screen.bounds[0];
    let h = screen.bounds[3] - screen.bounds[1];
    let primary = if screen.primary { ", primary" } else { "" };
    format!("Screen {n}\t{w} \u{d7} {h}{primary}")
}

/// What to save for a pick from the menu: nothing for the primary screen,
/// so the columns keep following it when another becomes primary.
pub fn choice(screen: &Screen) -> Option<String> {
    (!screen.primary).then(|| screen.name.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(name: &str, left: i32, primary: bool) -> Screen {
        Screen {
            name: name.into(),
            bounds: [left, 0, left + 1920, 1080],
            work: [left, 0, left + 1920, 1032],
            primary,
        }
    }

    #[test]
    fn picks_the_chosen_screen_while_it_is_there() {
        let screens = [screen("A", 0, true), screen("B", 1920, false)];
        assert_eq!(pick(&screens, Some("B")).unwrap().name, "B");
    }

    #[test]
    fn falls_back_to_the_primary_one() {
        let screens = [screen("B", -1920, false), screen("A", 0, true)];
        assert_eq!(pick(&screens, None).unwrap().name, "A");
        assert_eq!(pick(&screens, Some("gone")).unwrap().name, "A");
    }

    #[test]
    fn takes_any_screen_when_none_says_primary() {
        let screens = [screen("B", 0, false)];
        assert_eq!(pick(&screens, None).unwrap().name, "B");
        assert_eq!(pick(&[], None), None);
    }

    #[test]
    fn orders_left_to_right() {
        let order = in_order(vec![screen("B", 1920, false), screen("A", -1920, true)]);
        assert_eq!(order[0].name, "A");
        assert_eq!(order[1].name, "B");
    }

    #[test]
    fn labels_number_and_size() {
        assert_eq!(
            label(1, &screen("A", 0, true)),
            "Screen 1\t1920 \u{d7} 1080, primary"
        );
        assert_eq!(
            label(2, &screen("B", 1920, false)),
            "Screen 2\t1920 \u{d7} 1080"
        );
    }

    #[test]
    fn choosing_the_primary_screen_saves_nothing() {
        assert_eq!(choice(&screen("A", 0, true)), None);
        assert_eq!(choice(&screen("B", 1920, false)), Some("B".into()));
    }
}
