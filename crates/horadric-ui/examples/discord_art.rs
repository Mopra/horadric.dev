//! Writes the Rich Presence art into `docs/discord/`: the rune stone as
//! `horadric.png` and a lamp per state as `waits.png`, `working.png` and
//! `idle.png`, 512 by 512. Run it again whenever the icon or the lamps
//! change: `cargo run -p horadric-ui --example discord_art`.

use std::fs;
use std::path::Path;

use horadric_ui::{art, icon};

const SIZE: u32 = 512;

fn main() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/discord");
    fs::create_dir_all(&dir).expect("make docs/discord");
    let stone = dir.join("horadric.png");
    fs::write(&stone, icon::png(SIZE, false).expect("the 512 icon is baked")).expect("write the png");
    println!("{}", stone.display());
    for (name, pixels) in art::LAMPS.map(|(name, lamp)| (name, art::lamp(SIZE, lamp))) {
        let path = dir.join(format!("{name}.png"));
        fs::write(&path, art::png(SIZE, &pixels)).expect("write the png");
        println!("{}", path.display());
    }
}
