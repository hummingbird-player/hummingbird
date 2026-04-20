use objc2::rc::Retained;
use objc2::runtime::{NSObjectProtocol, ProtocolObject};
use objc2_foundation::{NSActivityOptions, NSProcessInfo, NSString};

pub struct PlatformPower {
    activity: Option<Retained<ProtocolObject<dyn NSObjectProtocol>>>,
}

impl PlatformPower {
    pub fn new() -> Self {
        Self { activity: None }
    }

    pub fn inhibit(&mut self) {
        if self.activity.is_some() {
            return;
        }
        let process_info = NSProcessInfo::processInfo();
        let reason = NSString::from_str("Hummingbird is playing media");
        self.activity =
            Some(process_info.beginActivityWithOptions_reason(
                NSActivityOptions::IdleDisplaySleepDisabled,
                &reason,
            ));
    }

    pub fn uninhibit(&mut self) {
        let Some(activity) = self.activity.take() else {
            return;
        };
        unsafe { NSProcessInfo::processInfo().endActivity(&activity) };
    }
}
