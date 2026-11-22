//! Tests against the real default output card. Ignored by default because they cut off other
//! applications' audio. Run with
//! `cargo test alsa::hardware_tests -- --ignored --nocapture --test-threads=1`.

use std::time::{Duration, Instant};

use crate::devices::{
    format::{ChannelSpec, FormatInfo},
    traits::{Device, DeviceProvider},
};

use crate::devices::builtin::exclusive::PAUSE_FADE_WAIT;

use super::{AlsaProvider, PROVIDER_NAME, device::AlsaDevice, reserve};

fn default_device() -> (Box<dyn Device>, i32) {
    let mut provider = AlsaProvider::default();
    provider.initialize().unwrap();
    let device = provider.get_default_device().unwrap();
    // find the default device's card in the provider's list
    let uid = device.get_uid().unwrap();
    let card = provider
        .devices()
        .into_iter()
        .find(|d: &AlsaDevice| d.get_uid().is_ok_and(|u| u == uid))
        .unwrap()
        .card;
    (device, card)
}

fn holder(card: i32) -> Option<String> {
    reserve::holder_property(card, "ApplicationName")
}

fn wait_for_holder(card: i32, expected: Option<&str>, timeout: Duration) -> Option<String> {
    let deadline = Instant::now() + timeout;
    loop {
        let current = holder(card);
        if current.as_deref() == expected || Instant::now() >= deadline {
            return current;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
#[ignore = "takes the default card from the sound server"]
fn alsa_device_summary() {
    // ALSA_TEST_DEVICE=hw:CARD=HDMI,DEV=3 summarizes another device
    if let Ok(uid) = std::env::var("ALSA_TEST_DEVICE") {
        let device = AlsaProvider::default()
            .devices()
            .into_iter()
            .find(|d| d.get_uid().is_ok_and(|u| u == uid))
            .expect("no such device");
        for probed in device.formats().unwrap() {
            println!("  {probed:?}");
        }
        return;
    }
    let (device, card) = default_device();
    println!(
        "default device: {} ({})",
        device.get_name().unwrap(),
        device.get_uid().unwrap()
    );
    println!("card {card} held by {:?}", holder(card));
    println!("default format: {:?}", device.get_default_format().unwrap());
    for format in device.get_supported_formats().unwrap() {
        println!(
            "  {:?} {} Hz, {} ch",
            format.sample_type,
            format.sample_rates.0,
            format.channels.count()
        );
    }
}

/// Plays silence through the reservation flow and checks that pausing gives the card back to the
/// sound server.
#[test]
#[ignore = "takes the default card from the sound server"]
fn alsa_pause_returns_card() {
    let (mut device, card) = default_device();
    let original = holder(card);
    println!("card {card} initially held by {original:?}");

    let default = device.get_default_format().unwrap();
    let mut stream = device
        .open_device(FormatInfo {
            originating_provider: PROVIDER_NAME,
            channels: ChannelSpec::Count(default.channels.count()),
            ..default
        })
        .unwrap();
    assert_eq!(
        holder(card),
        original,
        "opening a stream must not take the card"
    );

    let started = Instant::now();
    stream.play().unwrap();
    println!("acquired in {:?}", started.elapsed());
    assert_eq!(holder(card).as_deref(), Some("Hummingbird"));

    std::thread::sleep(Duration::from_millis(500));
    stream.pause().unwrap();
    std::thread::sleep(PAUSE_FADE_WAIT + Duration::from_millis(10));
    stream.poll().unwrap();
    let paused = Instant::now();
    assert_eq!(
        holder(card).as_deref(),
        Some("Hummingbird"),
        "the card is kept briefly after a pause"
    );
    // 3 s hold, ~300 ms to close a USB PCM, plus WirePlumber's setup time
    let after = wait_for_holder(card, original.as_deref(), Duration::from_secs(8));
    println!(
        "after pause, card {card} held by {after:?} after {:?}",
        paused.elapsed()
    );
    assert_ne!(
        after.as_deref(),
        Some("Hummingbird"),
        "pausing must hand the card back"
    );
    assert_eq!(
        after, original,
        "the sound server should take the card back"
    );

    // resuming should take the card again, and closing the stream should release it
    let resumed = Instant::now();
    stream.play().unwrap();
    println!("reacquired in {:?}", resumed.elapsed());
    assert_eq!(holder(card).as_deref(), Some("Hummingbird"));
    let closed = Instant::now();
    stream.close_stream().unwrap();
    let after = wait_for_holder(card, original.as_deref(), Duration::from_secs(6));
    println!(
        "after close, card {card} held by {after:?} after {:?}",
        closed.elapsed()
    );
    assert_eq!(after, original, "closing must hand the card back");
}

/// Plays a quiet 440 Hz tone at 16-bit 44.1 kHz and checks the device never underruns.
#[test]
#[ignore = "takes the default card from the sound server and plays a quiet tone"]
fn alsa_plays_tone_without_underruns() {
    use crate::{
        devices::{format::SampleFormat, traits::OutputStream},
        media::pipeline::ChannelBuffers,
    };

    use super::{
        format::{Encoding, StreamFormat},
        stream,
    };

    let mut provider = AlsaProvider::default();
    provider.initialize().unwrap();
    let uid = provider.get_default_device().unwrap().get_uid().unwrap();
    let device = provider
        .devices()
        .into_iter()
        .find(|d| d.get_uid().is_ok_and(|u| u == uid))
        .unwrap();
    let supports = device
        .get_supported_formats()
        .unwrap()
        .iter()
        .any(|f| f.sample_type == SampleFormat::Signed16 && f.sample_rates.0 == 44_100);
    if !supports {
        println!("default device has no 16-bit 44.1 kHz format; skipping");
        return;
    }

    let rate = 44_100;
    let mut stream = stream::open::<i16>(
        device.target(),
        StreamFormat {
            encoding: Encoding::S16,
            rate,
            channels: 2,
        },
        None,
    )
    .unwrap();
    stream.set_conversion_check(true);

    let (mut producers, mut consumers) = ChannelBuffers::<f64>::new(2, 4096).split();
    let amplitude = 0.01;
    let total = rate as usize * 2;
    let mut generated = 0;
    let generate = |producers: &mut crate::media::pipeline::ChannelProducers<f64>,
                    generated: &mut usize| {
        let frames = (total - *generated).min(1024);
        let room = producers.available();
        if frames == 0 || room < frames {
            return;
        }
        let plane: Vec<f64> = (*generated..*generated + frames)
            .map(|i| {
                let value = amplitude
                    * (2.0 * std::f64::consts::PI * 440.0 * i as f64 / f64::from(rate)).sin();
                // quantize so the 16-bit conversion is exact
                (value * 32_768.0).round() / 32_768.0
            })
            .collect();
        producers.write_vecs(&[plane.clone(), plane]).unwrap();
        *generated += frames;
    };

    generate(&mut producers, &mut generated);
    while stream.needs_input() && stream.consume_from(&mut consumers).unwrap() > 0 {
        generate(&mut producers, &mut generated);
    }
    stream.play().unwrap();
    let start = Instant::now();
    while generated < total || consumers.potentially_available() > 0 {
        generate(&mut producers, &mut generated);
        stream.consume_from(&mut consumers).unwrap();
        std::thread::sleep(Duration::from_millis(5));
    }
    let underruns = stream.underruns();
    println!(
        "fed {:.2} s of audio in {:?}, {underruns} underruns, conversion exact: {:?}",
        total as f64 / f64::from(rate),
        start.elapsed(),
        stream.conversion_exact()
    );
    std::thread::sleep(Duration::from_millis(250));
    stream.close_stream().unwrap();

    assert_eq!(underruns, 0);
    assert_eq!(stream.conversion_exact(), Some(true));
}

/// Reopening in another format mid-playback should keep the card and the default device, and
/// give the card back if nothing reopens.
#[test]
#[ignore = "takes the default card from the sound server"]
fn alsa_reopen_keeps_card() {
    let mut provider = AlsaProvider::default();
    provider.initialize().unwrap();
    let mut device = provider.get_default_device().unwrap();
    let uid = device.get_uid().unwrap();
    let card = provider
        .devices()
        .into_iter()
        .find(|d| d.get_uid().is_ok_and(|u| u == uid))
        .unwrap()
        .card;
    let original = holder(card);

    let formats = device.get_supported_formats().unwrap();
    let rate = formats[0].sample_rates.0;
    let mut at_rate: Vec<_> = formats
        .iter()
        .filter(|f| f.sample_rates.0 == rate)
        .map(|f| f.sample_type)
        .collect();
    at_rate.dedup();
    let open = |device: &mut Box<dyn Device>, sample_type| {
        device
            .open_device(FormatInfo {
                originating_provider: PROVIDER_NAME,
                sample_type,
                sample_rate: rate,
                buffer_size: crate::devices::format::BufferSize::Unknown,
                channels: ChannelSpec::Count(2),
            })
            .unwrap()
    };

    let mut first = open(&mut device, at_rate[0]);
    first.play().unwrap();
    assert_eq!(holder(card).as_deref(), Some("Hummingbird"));
    std::thread::sleep(Duration::from_millis(300));

    let reopened = Instant::now();
    first.close_stream().unwrap();
    drop(first);
    assert_eq!(
        holder(card).as_deref(),
        Some("Hummingbird"),
        "closing mid-play keeps the card"
    );
    let mut device = provider.get_default_device().unwrap();
    assert_eq!(
        device.get_uid().unwrap(),
        uid,
        "default device must not change mid-reopen"
    );
    let mut second = open(&mut device, *at_rate.last().unwrap());
    second.play().unwrap();
    println!(
        "reopened {:?} -> {:?} in {:?}",
        at_rate[0],
        at_rate.last(),
        reopened.elapsed()
    );
    assert_eq!(holder(card).as_deref(), Some("Hummingbird"));
    std::thread::sleep(Duration::from_millis(300));

    // closing without reopening should give the card back once the linger time is up
    second.close_stream().unwrap();
    let after = wait_for_holder(card, original.as_deref(), Duration::from_secs(5));
    assert_eq!(after, original);

    // turning bit-perfect off (which drops the provider) should give it back immediately
    let mut third = open(&mut device, at_rate[0]);
    third.play().unwrap();
    third.close_stream().unwrap();
    drop(device);
    drop(provider);
    let after = wait_for_holder(card, original.as_deref(), Duration::from_secs(5));
    assert_eq!(after, original);
}

/// Quick resumes should be instant, and resuming just after the card was given back should wait
/// for WirePlumber. Afterwards, check WirePlumber's log for `alsa.lua` errors and the sink
/// name for a `.2`-style suffix.
#[test]
#[ignore = "takes the default card from the sound server repeatedly"]
fn alsa_quick_pause_play_cycles() {
    let (mut device, card) = default_device();
    let original = holder(card);
    let default = device.get_default_format().unwrap();
    let mut stream = device
        .open_device(FormatInfo {
            originating_provider: PROVIDER_NAME,
            channels: ChannelSpec::Count(default.channels.count()),
            ..default
        })
        .unwrap();

    for cycle in 0..4 {
        let started = Instant::now();
        stream.play().unwrap();
        let resumed_in = started.elapsed();
        println!("cycle {cycle}: playing after {resumed_in:?}");
        if cycle % 2 == 1 {
            assert!(
                resumed_in < Duration::from_millis(100),
                "quick resume must be instant"
            );
        }
        std::thread::sleep(Duration::from_millis(300));
        stream.pause().unwrap();
        std::thread::sleep(PAUSE_FADE_WAIT + Duration::from_millis(10));
        stream.poll().unwrap();
        // alternate a quick resume with one while WirePlumber is setting the card up
        let wait = if cycle % 2 == 0 { 400 } else { 3_400 };
        std::thread::sleep(Duration::from_millis(wait));
    }
    stream.close_stream().unwrap();
    let after = wait_for_holder(card, original.as_deref(), Duration::from_secs(8));
    assert_eq!(after, original);
}

/// Prints what PipeWire's sinks and the HDMI ELDs report for every playback PCM. Doesn't take
/// any card.
#[test]
#[ignore = "talks to PipeWire and the sound cards"]
fn alsa_sink_formats_and_eld() {
    use super::{eld, pipewire_info};

    for card in alsa::card::Iter::new().filter_map(Result::ok) {
        let index = card.get_index();
        for device in 0..16 {
            if std::path::Path::new(&format!("/proc/asound/card{index}/pcm{device}p")).exists() {
                println!("hw:{index},{device}");
                println!(
                    "  PipeWire: {:?}",
                    pipewire_info::sink_formats(index, device)
                );
                println!("  ELD: {:?}", eld::read(index, device));
            }
        }
    }
}
