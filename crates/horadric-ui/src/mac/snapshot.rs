//! `HORADRIC_SNAPSHOT=<folder>`: every window drawn into a PNG in that
//! folder every two seconds, for a Mac nobody is sitting at. AppKit draws
//! a view into a bitmap the way it draws it on screen, so this needs no
//! screen recording permission, which CI's Macs do not give.

use std::path::{Path, PathBuf};
use std::time::Duration;

use objc2_app_kit::{NSBitmapImageFileType, NSBitmapImageRep};
use objc2_foundation::NSDictionary;

use super::{app, queue};

const EVERY: Duration = Duration::from_secs(2);

pub fn start() {
    let Some(dir) = std::env::var_os("HORADRIC_SNAPSHOT").map(PathBuf::from) else {
        return;
    };
    let _ = std::fs::create_dir_all(&dir);
    schedule(dir);
}

fn schedule(dir: PathBuf) {
    queue::after_main(EVERY, move || {
        take(&dir);
        schedule(dir);
    });
}

fn take(dir: &Path) {
    let Some(views) = app::with(|a| a.views()) else {
        return;
    };
    for (name, view) in views {
        let bounds = view.bounds();
        if bounds.size.width < 1.0 || bounds.size.height < 1.0 {
            continue;
        }
        let Some(rep) = view.bitmapImageRepForCachingDisplayInRect(bounds) else {
            continue;
        };
        view.cacheDisplayInRect_toBitmapImageRep(bounds, &rep);
        let props = NSDictionary::new();
        let Some(png) = (unsafe {
            NSBitmapImageRep::representationUsingType_properties(
                &rep,
                NSBitmapImageFileType::PNG,
                &props,
            )
        }) else {
            continue;
        };
        let tmp = dir.join(format!("{name}.png.tmp"));
        if std::fs::write(&tmp, png.to_vec()).is_ok() {
            let _ = std::fs::rename(&tmp, dir.join(format!("{name}.png")));
        }
    }
}
