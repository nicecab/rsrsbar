//! Turning a [`super::menu::MenuModel`] into AppKit objects: the `NSMenu`
//! tree and the status item button's symbol, tint and title.

use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::sel;
use objc2_app_kit::{NSColor, NSImage, NSMenu, NSMenuItem, NSStatusBarButton};
use objc2_foundation::{NSString, ns_string};

use super::actions::Action;
use super::menu::{IconState, Item};
use crate::health::Color;

/// Bits of a menu item tag below the render generation: the action index.
pub(super) const TAG_INDEX_BITS: u32 = 20;

/// `NSMenu` for `items`; every action item's tag is `generation` above the
/// index of its action, appended to `actions`.
pub(super) fn ns_menu(
    items: &[Item],
    target: &AnyObject,
    generation: isize,
    actions: &mut Vec<Action>,
    mtm: MainThreadMarker,
) -> Retained<NSMenu> {
    let menu = NSMenu::new(mtm);
    menu.setAutoenablesItems(false);
    for item in items {
        let entry = match item {
            Item::Separator => NSMenuItem::separatorItem(mtm),
            Item::Label(title) => titled(title, false, mtm),
            Item::Action {
                title,
                action,
                enabled,
            } => {
                let entry = titled(title, *enabled, mtm);
                let index = isize::try_from(actions.len()).unwrap_or(0);
                actions.push(action.clone());
                entry.setTag((generation << TAG_INDEX_BITS) | index);
                // SAFETY: `target` implements `menuAction:` and outlives
                // the menu (it lives for the whole process).
                unsafe {
                    entry.setAction(Some(sel!(menuAction:)));
                    entry.setTarget(Some(target));
                }
                entry
            }
            Item::Submenu {
                title,
                color,
                items,
            } => {
                let entry = titled(&format!("{} {title}", dot(*color)), true, mtm);
                entry.setSubmenu(Some(&ns_menu(items, target, generation, actions, mtm)));
                entry
            }
        };
        menu.addItem(&entry);
    }
    menu
}

fn titled(title: &str, enabled: bool, mtm: MainThreadMarker) -> Retained<NSMenuItem> {
    let item = NSMenuItem::new(mtm);
    item.setTitle(&NSString::from_str(title));
    item.setEnabled(enabled);
    item
}

fn dot(color: Color) -> &'static str {
    match color {
        Color::Green => "🟢",
        Color::Grey => "⚪️",
        Color::Yellow => "🟡",
        Color::Red => "🔴",
    }
}

/// Normal symbol when all is well; a red warning triangle with the count
/// when anything is red; a crossed-out bell / "!" when notifications are off.
pub(super) fn render_icon(button: &NSStatusBarButton, icon: IconState) {
    let (symbol, red) = match (icon.red, icon.warning) {
        (0, false) => ("arrow.triangle.2.circlepath", false),
        (0, true) => ("bell.slash", false),
        _ => ("exclamationmark.triangle.fill", true),
    };
    let title = match (icon.red, icon.warning) {
        (0, false) => String::new(),
        (0, true) => "!".to_owned(),
        (n, false) => n.to_string(),
        (n, true) => format!("{n} !"),
    };
    let image = NSImage::imageWithSystemSymbolName_accessibilityDescription(
        &NSString::from_str(symbol),
        Some(ns_string!("rsrsbar")),
    );
    if let Some(image) = &image {
        image.setTemplate(!red);
    }
    button.setImage(image.as_deref());
    let tint = red.then(NSColor::systemRedColor);
    button.setContentTintColor(tint.as_deref());
    button.setTitle(&NSString::from_str(&title));
}
