//! The property lists a Mac install is made of: the app bundle's
//! `Info.plist` and the LaunchAgent that opens Horadric at login. Pure, so
//! both are tested on any machine; the Mac code writes them.

/// The bundle id, and the LaunchAgent's label.
pub const BUNDLE_ID: &str = "dev.horadric.app";

/// The oldest macOS the app is built for. Big Sur brought SF Symbols,
/// which the tiles draw their icons with.
pub const MIN_MACOS: &str = "11.0";

/// `Info.plist` for `Horadric.app` at `version`. The executable is the
/// command line binary itself: started from Finder it has no arguments,
/// which it reads as "start the app". `icon` names the `.icns` in
/// `Contents/Resources`, without its extension.
pub fn info_plist(version: &str, icon: Option<&str>) -> String {
    let icon = icon.map_or_else(String::new, |i| {
        format!(
            "\t<key>CFBundleIconFile</key>\n\t<string>{}</string>\n",
            escape(i)
        )
    });
    let v = escape(version);
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleDevelopmentRegion</key>
	<string>en</string>
	<key>CFBundleDisplayName</key>
	<string>Horadric</string>
	<key>CFBundleExecutable</key>
	<string>horadric</string>
{icon}	<key>CFBundleIdentifier</key>
	<string>{BUNDLE_ID}</string>
	<key>CFBundleInfoDictionaryVersion</key>
	<string>6.0</string>
	<key>CFBundleName</key>
	<string>Horadric</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleShortVersionString</key>
	<string>{v}</string>
	<key>CFBundleVersion</key>
	<string>{v}</string>
	<key>LSApplicationCategoryType</key>
	<string>public.app-category.developer-tools</string>
	<key>LSMinimumSystemVersion</key>
	<string>{MIN_MACOS}</string>
	<key>NSHighResolutionCapable</key>
	<true/>
	<key>NSSupportsAutomaticGraphicsSwitching</key>
	<true/>
</dict>
</plist>
"#
    )
}

/// The LaunchAgent that runs `exe app` at login. Not kept alive: Quit
/// means quit until the next login.
pub fn launch_agent(exe: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{BUNDLE_ID}</string>
	<key>ProgramArguments</key>
	<array>
		<string>{}</string>
		<string>app</string>
	</array>
	<key>RunAtLoad</key>
	<true/>
	<key>KeepAlive</key>
	<false/>
	<key>ProcessType</key>
	<string>Interactive</string>
</dict>
</plist>
"#,
        escape(exe)
    )
}

/// Text as a plist string holds it.
fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bundle_names_its_binary_version_and_icon() {
        let p = info_plist("0.17.0", Some("horadric"));
        assert!(p.contains("<key>CFBundleExecutable</key>\n\t<string>horadric</string>"));
        assert!(p.contains("<key>CFBundleShortVersionString</key>\n\t<string>0.17.0</string>"));
        assert!(p.contains("<key>CFBundleIconFile</key>\n\t<string>horadric</string>"));
        assert!(p.contains(BUNDLE_ID));
        assert!(!info_plist("1", None).contains("CFBundleIconFile"));
    }

    #[test]
    fn the_launch_agent_starts_the_app_and_escapes_the_path() {
        let p = launch_agent("/Users/a & b/Applications/Horadric.app/Contents/MacOS/horadric");
        assert!(p.contains("<string>/Users/a &amp; b/Applications/Horadric.app/Contents/MacOS/horadric</string>\n\t\t<string>app</string>"));
        assert!(p.contains("<key>RunAtLoad</key>\n\t<true/>"));
    }
}
