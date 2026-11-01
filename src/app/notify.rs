//! Native notifications through `UNUserNotificationCenter`.
//!
//! Every public call blocks on the framework's completion handlers for at
//! most [`DEADLINE`] in total. The handlers run on a private framework queue, never the
//! main queue, so blocking the main thread here cannot deadlock. A handler
//! that fires after the deadline sends into a dropped channel, which is
//! ignored. Outside an app bundle the framework raises an exception on
//! first use, so every call checks for a bundle identifier first.

use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::Bool;
use objc2_foundation::{NSBundle, NSDictionary, NSError, NSString};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNAuthorizationStatus, UNMutableNotificationContent,
    UNNotificationRequest, UNNotificationSettings, UNUserNotificationCenter,
};

use super::Authorization;

/// How long to wait for a completion handler.
pub const DEADLINE: Duration = Duration::from_secs(5);

/// Why a notification was not handed to the system.
#[derive(Debug, thiserror::Error)]
pub enum NotifyError {
    #[error("not running from an app bundle")]
    NoBundle,
    #[error("no answer from the notification center within {} s", DEADLINE.as_secs())]
    Deadline,
    #[error("the notification center refused: {0}")]
    Refused(String),
}

/// Asks once for alert and sound permission (macOS shows its prompt only
/// while the answer is not determined) and returns the resulting state.
pub fn request_authorization() -> Authorization {
    let Some(center) = center() else {
        return Authorization::Unknown;
    };
    let (tx, rx) = mpsc::channel();
    let handler = RcBlock::new(move |granted: Bool, _error: *mut NSError| {
        let _ = tx.send(granted.as_bool());
    });
    let options = UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound;
    let deadline = Instant::now() + DEADLINE;
    center.requestAuthorizationWithOptions_completionHandler(options, &handler);
    match rx.recv_timeout(DEADLINE) {
        Ok(true) => Authorization::Authorized,
        // Not granted: read the real state in whatever time is left.
        Ok(false) => settings_within(&center, deadline.saturating_duration_since(Instant::now())),
        Err(_) => Authorization::Unknown,
    }
}

/// The current authorization, without prompting.
pub fn authorization() -> Authorization {
    center().map_or(Authorization::Unknown, |center| {
        settings_within(&center, DEADLINE)
    })
}

/// The authorization status from the center's settings, or `Unknown` when
/// they do not arrive within `timeout`.
fn settings_within(center: &UNUserNotificationCenter, timeout: Duration) -> Authorization {
    let (tx, rx) = mpsc::channel();
    let handler = RcBlock::new(move |settings: std::ptr::NonNull<UNNotificationSettings>| {
        // SAFETY: the framework passes a valid settings object that lives
        // for the duration of the handler.
        let status = unsafe { settings.as_ref() }.authorizationStatus();
        let _ = tx.send(from_status(status));
    });
    center.getNotificationSettingsWithCompletionHandler(&handler);
    rx.recv_timeout(timeout).unwrap_or(Authorization::Unknown)
}

/// Posts one notification for repository `id`: grouped by `id`
/// (`threadIdentifier`) and carrying it as `userInfo["repo"]`.
pub fn post(id: &str, title: &str, body: &str) -> Result<(), NotifyError> {
    let center = center().ok_or(NotifyError::NoBundle)?;
    let request = request(id, title, body);
    let (tx, rx) = mpsc::channel();
    let handler = RcBlock::new(move |error: *mut NSError| {
        // SAFETY: `error` is either null or a valid `NSError` for the
        // duration of the handler.
        let message = unsafe { error.as_ref() }.map(|err| err.localizedDescription().to_string());
        let _ = tx.send(message);
    });
    center.addNotificationRequest_withCompletionHandler(&request, Some(&handler));
    match rx.recv_timeout(DEADLINE) {
        Ok(None) => Ok(()),
        Ok(Some(message)) => Err(NotifyError::Refused(message)),
        Err(_) => Err(NotifyError::Deadline),
    }
}

fn request(id: &str, title: &str, body: &str) -> Retained<UNNotificationRequest> {
    let content = UNMutableNotificationContent::new();
    content.setTitle(&NSString::from_str(title));
    content.setBody(&NSString::from_str(body));
    let repo = NSString::from_str(id);
    content.setThreadIdentifier(&repo);
    let user_info = NSDictionary::from_slices(&[&*NSString::from_str("repo")], &[&*repo]);
    // SAFETY: `userInfo` must hold property-list objects; both key and
    // value are `NSString`, and the cast only forgets their static types.
    unsafe { content.setUserInfo(Retained::cast_unchecked::<NSDictionary>(user_info).as_ref()) };
    // A unique identifier: the same one would replace the earlier banner.
    let identifier = NSString::from_str(&format!("{id}.{}", unique_suffix()));
    UNNotificationRequest::requestWithIdentifier_content_trigger(&identifier, &content, None)
}

fn unique_suffix() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos())
}

/// The shared center, or `None` outside an app bundle (where asking for it
/// raises an Objective-C exception and aborts the process).
fn center() -> Option<Retained<UNUserNotificationCenter>> {
    NSBundle::mainBundle().bundleIdentifier()?;
    Some(UNUserNotificationCenter::currentNotificationCenter())
}

/// `Provisional` delivers silently to Notification Center without banners,
/// and the user has not decided yet, so it counts as not determined.
fn from_status(status: UNAuthorizationStatus) -> Authorization {
    match status {
        UNAuthorizationStatus::Authorized | UNAuthorizationStatus::Ephemeral => {
            Authorization::Authorized
        }
        UNAuthorizationStatus::Denied => Authorization::Denied,
        UNAuthorizationStatus::NotDetermined | UNAuthorizationStatus::Provisional => {
            Authorization::NotDetermined
        }
        _ => Authorization::Unknown,
    }
}
