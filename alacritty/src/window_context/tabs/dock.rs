//! The menu of the application's icon in the Dock.

use std::cell::OnceCell;
use std::ffi::c_char;

use alacritty_tabbed_deps::{objc2, objc2_app_kit, objc2_foundation};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Imp, NSObject, Sel};
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{NSApplication, NSMenu, NSMenuItem};
use objc2_foundation::ns_string;
use winit::event_loop::EventLoopProxy;

use crate::cli::WindowOptions;
use crate::event::{Event, EventType};

struct Dock {
    menu: Retained<NSMenu>,
    _target: Retained<Target>,
    proxy: EventLoopProxy<Event>,
}

thread_local! {
    static DOCK: OnceCell<Dock> = const { OnceCell::new() };
}

define_class!(
    /// Receives the actions of the menu's items.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "AlacrittyTabbedDockTarget"]
    struct Target;

    impl Target {
        #[unsafe(method(newWindow:))]
        fn new_window(&self, _sender: Option<&AnyObject>) {
            DOCK.with(|dock| {
                if let Some(dock) = dock.get() {
                    let event = EventType::CreateWindow(WindowOptions::default());
                    let _ = dock.proxy.send_event(Event::new(event, None));
                }
            });
        }
    }
);

/// `applicationDockMenu:`, added to the application delegate winit declares.
unsafe extern "C-unwind" fn dock_menu(_: &AnyObject, _: Sel, _: *mut AnyObject) -> *mut NSMenu {
    DOCK.with(|dock| {
        dock.get().map_or(std::ptr::null_mut(), |dock| Retained::as_ptr(&dock.menu).cast_mut())
    })
}

/// Give the Dock's icon a menu with a New Window item, once.
pub fn install(proxy: &EventLoopProxy<Event>) {
    let Some(mtm) = MainThreadMarker::new() else { return };
    DOCK.with(|dock| {
        if dock.get().is_some() {
            return;
        }
        let app = NSApplication::sharedApplication(mtm);
        let Some(delegate) = app.delegate() else { return };

        let target: Retained<Target> = unsafe { msg_send![Target::alloc(mtm), init] };
        let menu = NSMenu::new(mtm);
        let item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                ns_string!("New Window"),
                Some(sel!(newWindow:)),
                ns_string!(""),
            )
        };
        unsafe { item.setTarget(Some(&target)) };
        menu.addItem(&item);
        let _ = dock.set(Dock { menu, _target: target, proxy: proxy.clone() });

        let class = AsRef::<AnyObject>::as_ref(&*delegate).class();
        let types: *const c_char = c"@@:@".as_ptr();
        unsafe {
            let imp: Imp =
                std::mem::transmute(dock_menu as unsafe extern "C-unwind" fn(_, _, _) -> _);
            objc2::ffi::class_addMethod(
                class as *const _ as *mut _,
                sel!(applicationDockMenu:),
                imp,
                types,
            );
        }
        // AppKit asks the delegate what it answers to when it is set.
        app.setDelegate(Some(&delegate));
    });
}
