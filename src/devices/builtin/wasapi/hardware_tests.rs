//! Diagnostics that talk to the real default output device. They are ignored by default because
//! they briefly take exclusive control of it, cutting off other applications' audio.

use std::time::{Duration, Instant};

use windows::Win32::{
    Media::Audio::{
        AUDCLNT_E_DEVICE_IN_USE, AUDCLNT_SHAREMODE_SHARED, IAudioClient, IMMDevice, eConsole,
        eRender,
    },
    System::Com::{CLSCTX_ALL, CoTaskMemFree},
};

use crate::devices::traits::DeviceProvider;

use super::{
    WasapiProvider,
    com::{ComGuard, device_id, enumerator, friendly_name},
    device::{FormatCache, PROBE_RATES, WasapiDevice},
    format::{ALL_ENCODINGS, StreamFormat, is_format_supported},
    session::{AcquireError, Session},
    stream::PAUSE_FADE_WAIT,
};

/// Describes whether a shared-mode client can open `device`, which fails while another client
/// holds it in exclusive mode.
fn shared_lockout(device: &IMMDevice) -> String {
    let shared: IAudioClient = unsafe { device.Activate(CLSCTX_ALL, None) }.unwrap();
    let mix = unsafe { shared.GetMixFormat() }.unwrap();
    let result = unsafe { shared.Initialize(AUDCLNT_SHAREMODE_SHARED, 0, 200_000, 0, mix, None) };
    unsafe { CoTaskMemFree(Some(mix as *const _)) };
    match result {
        Err(e) if e.code() == AUDCLNT_E_DEVICE_IN_USE => "locked out (exclusive)".to_string(),
        Ok(()) => "shared client opened".to_string(),
        Err(e) => format!("shared init error {e}"),
    }
}

/// Reports whether some application currently holds the default device in exclusive mode.
/// Run with `cargo test -- --ignored wasapi_lockout_probe --nocapture`.
#[test]
#[ignore]
fn wasapi_lockout_probe() {
    let _com = ComGuard::new();
    let device = unsafe {
        enumerator()
            .unwrap()
            .GetDefaultAudioEndpoint(eRender, eConsole)
    }
    .unwrap();
    println!("LOCKOUT: {}", shared_lockout(&device));
}

/// For every probed format, compares what `IsFormatSupported` claims with what an exclusive
/// `Initialize` really does, and checks that a shared-mode client is locked out while the
/// session is held. Run with `cargo test -- --ignored wasapi_exclusivity --nocapture`.
#[test]
#[ignore]
fn wasapi_exclusivity_report() {
    let _com = ComGuard::new();
    let device = unsafe {
        enumerator()
            .unwrap()
            .GetDefaultAudioEndpoint(eRender, eConsole)
    }
    .unwrap();
    let id = device_id(&device).unwrap();
    let base = WasapiDevice::new(device.clone(), FormatCache::default())
        .device_format()
        .unwrap();
    println!("device: {:?}", friendly_name(&device));
    println!("device format: {base:?}");
    let probe: IAudioClient = unsafe { device.Activate(CLSCTX_ALL, None) }.unwrap();

    let mut rates = PROBE_RATES.to_vec();
    rates.sort();
    for rate in rates {
        for encoding in ALL_ENCODINGS {
            let format = StreamFormat {
                encoding,
                rate,
                ..base
            };
            let claimed = is_format_supported(&probe, format);
            let opened = Session::open(&id, format);
            let verdict = match &opened {
                Err(AcquireError::Busy) => "init: device busy".to_string(),
                Err(AcquireError::NotAllowed) => "init: exclusive not allowed".to_string(),
                Err(AcquireError::Other(e)) => format!("init failed: {e}"),
                Ok(session) => format!(
                    "init ok, {} frames; shared client: {}",
                    session.buffer_frames,
                    shared_lockout(&device)
                ),
            };
            if claimed || opened.is_ok() {
                println!("{rate:>6} Hz {encoding:?}: claimed={claimed} {verdict}");
            }
        }
    }
}

/// Lists the default device's exclusive formats, then acquires it and runs a play, pause,
/// reset and close cycle. Run with `cargo test -- --ignored wasapi_hardware`.
#[test]
#[ignore]
fn wasapi_hardware_smoke() {
    let mut provider = WasapiProvider::default();
    provider.initialize().unwrap();
    let mut device = provider.get_default_device().unwrap();
    println!("device: {:?} ({:?})", device.get_name(), device.get_uid());
    let default = device.get_default_format().unwrap();
    println!("default format: {default:?}");
    let formats = device.get_supported_formats().unwrap();
    for format in &formats {
        println!("  {} Hz {:?}", format.sample_rates.0, format.sample_type);
    }
    assert!(!formats.is_empty());

    let mut stream = device.open_device(default).unwrap();
    let started = Instant::now();
    if let Err(error) = stream.play() {
        panic!("exclusive acquire failed: {error}");
    }
    println!("acquired in {:?}", started.elapsed());
    std::thread::sleep(Duration::from_millis(300));
    stream.pause().unwrap();
    std::thread::sleep(PAUSE_FADE_WAIT);
    stream.poll().unwrap();
    stream.reset().unwrap();
    stream.play().unwrap();
    std::thread::sleep(Duration::from_millis(100));
    stream.close_stream().unwrap();
}
