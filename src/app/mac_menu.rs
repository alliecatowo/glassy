//! macOS global menu bar (NSMenu) wired to glassy's [`KeyAction`]s.
//!
//! On macOS apps own the system menu bar at the top of the screen; without one,
//! the menu shows only a bare bold app name and none of the standard
//! glassy/File/Edit/View/Window commands. This module builds that menu via
//! AppKit (`objc2-app-kit`, already a dependency for the dock icon) and routes
//! each item back into the winit event loop as a [`UserEvent::MenuAction`], so a
//! menu click runs through the *exact* same `run_key_action` path as the
//! equivalent keychord — the menu and the keyboard can never diverge.
//!
//! Items carry a `keyEquivalent` (the bare character; AppKit defaults its
//! modifier to ⌘, and an uppercase character means ⌘⇧) so the standard shortcuts
//! render next to each command and AppKit dispatches them even when the menu is
//! closed. The whole module is `cfg(target_os = "macos")`; it is never compiled
//! elsewhere.

#![cfg(target_os = "macos")]

use crate::config::KeyAction;
use crate::pty::UserEvent;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol, Sel};
use objc2::{ClassType, DeclaredClass, declare_class, msg_send_id, mutability, sel};
use objc2_app_kit::{NSApplication, NSMenu, NSMenuItem};
use objc2_foundation::{MainThreadMarker, NSString};
use winit::event_loop::EventLoopProxy;

/// Per-instance state for the menu target: the proxy used to forward a clicked
/// item's action into the winit event loop.
struct Ivars {
    proxy: EventLoopProxy<UserEvent>,
}

declare_class!(
    /// The Objective-C target object that every menu item points at. Each action
    /// selector maps to a fixed [`KeyAction`] forwarded over the proxy.
    struct MenuTarget;

    // SAFETY:
    // - The superclass NSObject has no subclassing requirements.
    // - Interior mutability is the safe default (we never mutate ivars).
    // - `MenuTarget` does not implement `Drop`.
    unsafe impl ClassType for MenuTarget {
        type Super = objc2::runtime::NSObject;
        type Mutability = mutability::InteriorMutable;
        const NAME: &'static str = "GlassyMenuTarget";
    }

    impl DeclaredClass for MenuTarget {
        type Ivars = Ivars;
    }

    unsafe impl MenuTarget {
        #[method(glassyNewTab:)]
        fn new_tab(&self, _sender: Option<&AnyObject>) {
            self.fire(KeyAction::NewTab);
        }
        #[method(glassyClosePane:)]
        fn close_pane(&self, _sender: Option<&AnyObject>) {
            self.fire(KeyAction::ClosePane);
        }
        #[method(glassySplitV:)]
        fn split_v(&self, _sender: Option<&AnyObject>) {
            self.fire(KeyAction::SplitVertical);
        }
        #[method(glassySplitH:)]
        fn split_h(&self, _sender: Option<&AnyObject>) {
            self.fire(KeyAction::SplitHorizontal);
        }
        #[method(glassyCopy:)]
        fn copy(&self, _sender: Option<&AnyObject>) {
            self.fire(KeyAction::Copy);
        }
        #[method(glassyPaste:)]
        fn paste(&self, _sender: Option<&AnyObject>) {
            self.fire(KeyAction::Paste);
        }
        #[method(glassyFind:)]
        fn find(&self, _sender: Option<&AnyObject>) {
            self.fire(KeyAction::Search);
        }
        #[method(glassyPalette:)]
        fn palette(&self, _sender: Option<&AnyObject>) {
            self.fire(KeyAction::CommandPalette);
        }
        #[method(glassySettings:)]
        fn settings(&self, _sender: Option<&AnyObject>) {
            self.fire(KeyAction::Settings);
        }
        #[method(glassyNextTab:)]
        fn next_tab(&self, _sender: Option<&AnyObject>) {
            self.fire(KeyAction::NextTab);
        }
        #[method(glassyPrevTab:)]
        fn prev_tab(&self, _sender: Option<&AnyObject>) {
            self.fire(KeyAction::PrevTab);
        }
        #[method(glassyFullscreen:)]
        fn fullscreen(&self, _sender: Option<&AnyObject>) {
            self.fire(KeyAction::ToggleFullscreen);
        }
        #[method(glassyZoom:)]
        fn zoom(&self, _sender: Option<&AnyObject>) {
            self.fire(KeyAction::ToggleZoom);
        }
        #[method(glassyHelp:)]
        fn help(&self, _sender: Option<&AnyObject>) {
            self.fire(KeyAction::Help);
        }
    }

    unsafe impl NSObjectProtocol for MenuTarget {}
);

impl MenuTarget {
    fn new(proxy: EventLoopProxy<UserEvent>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(Ivars { proxy });
        unsafe { msg_send_id![super(this), init] }
    }

    /// Forward `action` to the winit loop. The handler runs it via the shared
    /// `run_key_action` path. A send failure (loop gone) is a harmless no-op.
    fn fire(&self, action: KeyAction) {
        let _ = self.ivars().proxy.send_event(UserEvent::MenuAction(action));
    }
}

/// Build a menu item titled `title`, targeting `target`'s `selector`, with the
/// bare `key` as its ⌘ key-equivalent (empty for none; an uppercase char renders
/// and dispatches as ⌘⇧). The retained target is shared by every item so its
/// proxy outlives the menu.
fn item(
    mtm: MainThreadMarker,
    target: &MenuTarget,
    title: &str,
    selector: Sel,
    key: &str,
) -> Retained<NSMenuItem> {
    let ns_title = NSString::from_str(title);
    let ns_key = NSString::from_str(key);
    // SAFETY: standard NSMenuItem construction with valid NSStrings + selector.
    let mi = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            mtm.alloc(),
            &ns_title,
            Some(selector),
            &ns_key,
        )
    };
    // Deref-coerce the custom class reference down to `&AnyObject` (the chain is
    // MenuTarget → NSObject → AnyObject) at an explicit-type coercion site.
    let any: &AnyObject = target;
    unsafe {
        mi.setTarget(Some(any));
    }
    mi
}

/// Append a titled submenu (a top-level bar entry) built from `items` to `bar`.
fn submenu(mtm: MainThreadMarker, bar: &NSMenu, title: &str, items: &[Retained<NSMenuItem>]) {
    let ns_title = NSString::from_str(title);
    // Use the SAFE `NSMenu::new` constructor (no unsafe): the inner submenu's own
    // title is cosmetic — the visible top-level bar label is the entry item's
    // title, set just below — so we don't need the unsafe `initWithTitle`.
    let menu = NSMenu::new(mtm);
    for it in items {
        menu.addItem(it);
    }
    // The bar entry is an item whose submenu is `menu`; its own title is the bar
    // label (AppKit shows the submenu's title for app menus, but setting both is
    // harmless and keeps non-app menus labeled).
    let entry = NSMenuItem::new(mtm);
    unsafe {
        entry.setTitle(&ns_title);
    }
    entry.setSubmenu(Some(&menu));
    bar.addItem(&entry);
}

/// A separator menu item.
fn sep(mtm: MainThreadMarker) -> Retained<NSMenuItem> {
    NSMenuItem::separatorItem(mtm)
}

/// Install the glassy global menu bar (glassy / File / Edit / View / Window).
/// Called once at startup after `NSApplication` exists. Holds the menu target
/// alive by handing it to AppKit's retained menu graph (each item retains its
/// target). A no-op if the main-thread marker can't be obtained.
pub fn install_menu_bar(proxy: EventLoopProxy<UserEvent>) {
    let Some(mtm) = MainThreadMarker::new() else {
        log::warn!("glassy: NSMenu install skipped (not on the main thread)");
        return;
    };
    let target = MenuTarget::new(proxy);
    let t = &*target;

    objc2::rc::autoreleasepool(|_| {
        let bar = NSMenu::new(mtm);

        // glassy (app) menu: About-style first entry is the app name. We add
        // Settings + Quit; Quit uses AppKit's standard terminate: so ⌘Q always
        // works even before our window has focus.
        let app_items = [
            item(mtm, t, "Settings…", sel!(glassySettings:), ","),
            sep(mtm),
            quit_item(mtm),
        ];
        submenu(mtm, &bar, "Glassy", &app_items);

        // File menu.
        let file_items = [
            item(mtm, t, "New Tab", sel!(glassyNewTab:), "t"),
            item(mtm, t, "Split Vertically", sel!(glassySplitV:), "d"),
            // Uppercase "D" → ⌘⇧D (no NSEvent feature needed for the mask).
            item(mtm, t, "Split Horizontally", sel!(glassySplitH:), "D"),
            sep(mtm),
            item(mtm, t, "Close", sel!(glassyClosePane:), "w"),
        ];
        submenu(mtm, &bar, "File", &file_items);

        // Edit menu.
        let edit_items = [
            item(mtm, t, "Copy", sel!(glassyCopy:), "c"),
            item(mtm, t, "Paste", sel!(glassyPaste:), "v"),
            sep(mtm),
            item(mtm, t, "Find…", sel!(glassyFind:), "f"),
            item(mtm, t, "Command Palette…", sel!(glassyPalette:), "P"),
        ];
        submenu(mtm, &bar, "Edit", &edit_items);

        // View menu.
        let view_items = [
            item(mtm, t, "Enter Full Screen", sel!(glassyFullscreen:), ""),
            item(mtm, t, "Zoom Pane", sel!(glassyZoom:), ""),
            sep(mtm),
            item(mtm, t, "Glassy Help", sel!(glassyHelp:), "?"),
        ];
        submenu(mtm, &bar, "View", &view_items);

        // Window menu (tab navigation).
        let window_items = [
            item(mtm, t, "Next Tab", sel!(glassyNextTab:), "}"),
            item(mtm, t, "Previous Tab", sel!(glassyPrevTab:), "{"),
        ];
        submenu(mtm, &bar, "Window", &window_items);

        let app = NSApplication::sharedApplication(mtm);
        app.setMainMenu(Some(&bar));
    });

    // Leak the target so it lives for the whole process: AppKit's items hold a
    // weak-ish target reference; keeping our Retained alive guarantees the proxy
    // is valid for every later menu click. The menu bar persists for the app's
    // lifetime, so this one-time leak is intentional and bounded.
    std::mem::forget(target);
}

/// The standard Quit item, wired to AppKit's own `terminate:` so ⌘Q quits the
/// app the way every macOS user expects (clean NSApplication shutdown).
fn quit_item(mtm: MainThreadMarker) -> Retained<NSMenuItem> {
    let title = NSString::from_str("Quit Glassy");
    let key = NSString::from_str("q");
    // SAFETY: terminate: is a standard NSApplication action; nil target lets
    // AppKit route it up the responder chain to the application object.
    unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            mtm.alloc(),
            &title,
            Some(sel!(terminate:)),
            &key,
        )
    }
}
