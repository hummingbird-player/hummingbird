use tracing::error;
use windows::{
    Win32::{
        Foundation::{CloseHandle, HANDLE},
        System::{
            Power::{
                PowerClearRequest, PowerCreateRequest, PowerRequestExecutionRequired,
                PowerSetRequest,
            },
            SystemServices::POWER_REQUEST_CONTEXT_VERSION,
            Threading::{POWER_REQUEST_CONTEXT_SIMPLE_STRING, REASON_CONTEXT, REASON_CONTEXT_0},
        },
    },
    core::{PWSTR, w},
};

pub struct PlatformPower {
    handle: Option<HANDLE>,
}

impl PlatformPower {
    pub fn new() -> Self {
        Self { handle: None }
    }

    pub fn inhibit(&mut self) {
        let reason = w!("Playing music");

        unsafe {
            let context = REASON_CONTEXT {
                Version: POWER_REQUEST_CONTEXT_VERSION,
                Flags: POWER_REQUEST_CONTEXT_SIMPLE_STRING,
                Reason: REASON_CONTEXT_0 {
                    // very cool that this requires a mut pointer even though the string is never
                    // mutated! i love windows
                    SimpleReasonString: PWSTR(reason.as_ptr() as *mut _),
                },
            };

            let Ok(handle) = PowerCreateRequest(&context) else {
                error!("Failed to create power request handle, not inhibiting");
                return;
            };

            if let Err(e) = PowerSetRequest(handle, PowerRequestExecutionRequired) {
                error!("Failed to set power request: {:?}", e)
            }

            self.handle = Some(handle);
        }
    }

    pub fn uninhibit(&mut self) {
        unsafe {
            if let Some(handle) = self.handle.take() {
                if let Err(e) = PowerClearRequest(handle, PowerRequestExecutionRequired) {
                    error!("Failed to clear power request: {:?}", e)
                }

                if let Err(e) = CloseHandle(handle) {
                    error!("Failed to close power request handle: {:?}", e)
                }
            }
        }
    }
}
