//! The application's delegate: Quit from the Dock asks the way Cmd+Q
//! does, and a click on the Dock icon brings the stage back.

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{define_class, msg_send, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationDelegate, NSApplicationTerminateReply, NSEvent,
    NSEventModifierFlags, NSEventType,
};
use objc2_foundation::{NSNotification, NSObject, NSObjectProtocol, NSPoint};

use super::app::{self, Input};

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and this does not
    // implement Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    pub struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl NSApplicationDelegate for Delegate {
        #[unsafe(method(applicationShouldTerminate:))]
        fn application_should_terminate(
            &self,
            _sender: &NSApplication,
        ) -> NSApplicationTerminateReply {
            // Asked here, answered by the app once it has asked the human.
            app::input(Input::Quit);
            NSApplicationTerminateReply::TerminateCancel
        }

        #[unsafe(method(applicationShouldTerminateAfterLastWindowClosed:))]
        fn after_last_window(&self, _sender: &NSApplication) -> bool {
            false
        }

        // The tiles come up with the app, so Horadric shows as one, as
        // the Windows stage brings its tiles when it activates.
        #[unsafe(method(applicationDidBecomeActive:))]
        fn did_become_active(&self, _note: &NSNotification) {
            app::input(Input::Menu(app::FRONT_TAG));
        }

        #[unsafe(method(applicationShouldHandleReopen:hasVisibleWindows:))]
        fn reopen(&self, _sender: &NSApplication, _visible: bool) -> bool {
            app::input(Input::Menu(app::SHOW_STAGE_TAG));
            true
        }
    }
);

thread_local! {
    static DELEGATE: std::cell::OnceCell<Retained<Delegate>> = const { std::cell::OnceCell::new() };
}

pub fn install(mtm: MainThreadMarker) {
    let d: Retained<Delegate> = unsafe { msg_send![Delegate::alloc(mtm), init] };
    let app = NSApplication::sharedApplication(mtm);
    app.setDelegate(Some(ProtocolObject::from_ref(&*d)));
    DELEGATE.with(|cell| {
        let _ = cell.set(d);
    });
}

/// Posts an event of the app's own, so a `stop` takes effect now rather
/// than at the next event from outside.
pub fn wake(mtm: MainThreadMarker) {
    let event = NSEvent::otherEventWithType_location_modifierFlags_timestamp_windowNumber_context_subtype_data1_data2(
        NSEventType::ApplicationDefined,
        NSPoint::new(0.0, 0.0),
        NSEventModifierFlags::empty(),
        0.0,
        0,
        None,
        0,
        0,
        0,
    );
    if let Some(e) = event {
        NSApplication::sharedApplication(mtm).postEvent_atStart(&e, true);
    }
}
