use windows::{
    Win32::{
        Devices::FunctionDiscovery::PKEY_Device_FriendlyName,
        Foundation::PROPERTYKEY,
        Media::Audio::{IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator},
        System::{
            Com::{
                CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
                CoUninitialize, STGM_READ,
                StructuredStorage::{PROPVARIANT, PropVariantClear},
            },
            Variant::VT_LPWSTR,
        },
        UI::Shell::PropertiesSystem::IPropertyStore,
    },
    core::PWSTR,
};

pub(super) struct ComGuard(bool);

impl ComGuard {
    pub(super) fn new() -> Self {
        // RPC_E_CHANGED_MODE means something else already initialized this thread as
        // single-threaded (STA). That's fine, since the WASAPI interfaces are free-threaded and
        // work in either threading model, but the failed call must not be balanced with
        // CoUninitialize.
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        ComGuard(hr.is_ok())
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.0 {
            unsafe { CoUninitialize() };
        }
    }
}

thread_local!(static COM: ComGuard = ComGuard::new());

fn ensure_com() {
    COM.with(|_| {});
}

fn take_com_string(value: PWSTR) -> Option<String> {
    if value.is_null() {
        return None;
    }
    let string = unsafe { value.to_string() }.ok();
    unsafe { CoTaskMemFree(Some(value.as_ptr() as *const _)) };
    string
}

pub(super) fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

pub(super) fn enumerator() -> windows::core::Result<IMMDeviceEnumerator> {
    ensure_com();
    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
}

pub(super) fn device_id(device: &IMMDevice) -> Option<String> {
    take_com_string(unsafe { device.GetId() }.ok()?)
}

pub(super) fn read_property<R>(
    store: &IPropertyStore,
    key: &PROPERTYKEY,
    read: impl FnOnce(&PROPVARIANT) -> Option<R>,
) -> Option<R> {
    let mut value = unsafe { store.GetValue(key) }.ok()?;
    let result = read(&value);
    unsafe { PropVariantClear(&mut value) }.ok();
    result
}

pub(super) fn friendly_name(device: &IMMDevice) -> Option<String> {
    let store = unsafe { device.OpenPropertyStore(STGM_READ) }.ok()?;
    read_property(&store, &PKEY_Device_FriendlyName, |value| unsafe {
        let inner = &value.Anonymous.Anonymous;
        if inner.vt != VT_LPWSTR || inner.Anonymous.pwszVal.is_null() {
            return None;
        }
        inner.Anonymous.pwszVal.to_string().ok()
    })
}
