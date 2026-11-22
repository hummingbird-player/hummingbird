use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::Once,
    time::Duration,
};

use pipewire::{
    self as pw,
    context::ContextRc,
    core::{CoreRc, PW_ID_CORE},
    main_loop::MainLoopRc,
    metadata::{Metadata, MetadataListener},
    node::{Node, NodeListener},
    registry::RegistryRc,
    spa::{
        param::{ParamType, audio::AudioFormat},
        pod::{ChoiceValue, Pod, Value, deserialize::PodDeserializer},
        sys as spa_sys,
        utils::{Choice, ChoiceEnum, Id},
    },
    types::ObjectType,
};
use tracing::debug;

use super::format::Encoding;

/// How long to wait for PipeWire to answer.
const TIMEOUT: Duration = Duration::from_secs(1);

/// The default sink and ALSA cards reported by PipeWire.
#[derive(Debug, Default)]
pub(super) struct PipeWireOutputs {
    /// The user's default sink. Read from `default.configured.audio.sink`, which (unlike
    /// `default.audio.sink`) still names it while we hold its card.
    pub(super) sink: Option<String>,
    /// `(device name, ALSA card ID)`. Cards we hold aren't listed.
    pub(super) cards: Vec<(String, String)>,
    /// ALSA card IDs with a playback node, i.e. that WirePlumber has finished setting up.
    pub(super) cards_with_sinks: Vec<String>,
}

struct Connection {
    mainloop: MainLoopRc,
    core: CoreRc,
    registry: RegistryRc,
    _context: ContextRc,
}

impl Connection {
    fn new() -> Option<Self> {
        static INIT: Once = Once::new();
        INIT.call_once(pw::init);

        let mainloop = MainLoopRc::new(None).ok()?;
        let context = ContextRc::new(&mainloop, None).ok()?;
        let core = context.connect_rc(None).ok()?;
        let registry = core.get_registry_rc().ok()?;
        Some(Self {
            mainloop,
            core,
            registry,
            _context: context,
        })
    }

    /// Waits until PipeWire has answered every request sent so far, including the events those
    /// requests trigger. `None` on timeout.
    fn roundtrip(&self) -> Option<()> {
        let seq = self.core.sync(0).ok()?;
        let done = Rc::new(Cell::new(false));
        let _listener = self
            .core
            .add_listener_local()
            .done({
                let done = done.clone();
                let mainloop = self.mainloop.clone();
                move |id, answered| {
                    if id == PW_ID_CORE && answered == seq {
                        done.set(true);
                        mainloop.quit();
                    }
                }
            })
            .register();

        // in case the server connects but never responds
        let (timeout_tx, timeout_rx) = pw::channel::channel::<()>();
        let _timeout = timeout_rx.attach(self.mainloop.loop_(), {
            let mainloop = self.mainloop.clone();
            move |_| mainloop.quit()
        });
        let (cancel_tx, cancel_rx) = std::sync::mpsc::channel::<()>();
        std::thread::spawn(move || {
            if cancel_rx.recv_timeout(TIMEOUT).is_err() {
                let _ = timeout_tx.send(());
            }
        });

        self.mainloop.run();
        let _ = cancel_tx.send(());
        done.get().then_some(())
    }

    /// Two roundtrips: the first lists every object (and sends any binds made while listing),
    /// the second delivers the bound objects' events.
    fn settle(&self) {
        if self.roundtrip().is_some() {
            self.roundtrip();
        }
    }
}

/// Queries PipeWire. `None` if it isn't running.
pub(super) fn query() -> Option<PipeWireOutputs> {
    let pw = Connection::new()?;
    let configured: Rc<RefCell<Option<String>>> = Rc::default();
    let current: Rc<RefCell<Option<String>>> = Rc::default();
    let cards: Rc<RefCell<Vec<(String, String)>>> = Rc::default();
    let cards_with_sinks: Rc<RefCell<Vec<String>>> = Rc::default();
    let bound: Rc<RefCell<Vec<(Metadata, MetadataListener)>>> = Rc::default();

    let _registry_listener = pw
        .registry
        .add_listener_local()
        .global({
            let registry = pw.registry.clone();
            let configured = configured.clone();
            let current = current.clone();
            let cards = cards.clone();
            let cards_with_sinks = cards_with_sinks.clone();
            let bound = bound.clone();
            move |global| {
                let Some(props) = global.props else {
                    return;
                };
                match global.type_ {
                    ObjectType::Node => {
                        if let Some(card_id) =
                            props.get("object.path").and_then(card_of_playback_node)
                        {
                            cards_with_sinks.borrow_mut().push(card_id);
                        }
                    }
                    ObjectType::Device => {
                        if let Some(card) = props
                            .get("device.name")
                            .zip(props.get("object.path"))
                            .and_then(|(name, path)| card_of_device(name, path))
                        {
                            cards.borrow_mut().push(card);
                        }
                    }
                    ObjectType::Metadata if props.get("metadata.name") == Some("default") => {
                        let Ok(metadata) = registry.bind::<Metadata, _>(global) else {
                            return;
                        };
                        let configured = configured.clone();
                        let current = current.clone();
                        let listener = metadata
                            .add_listener_local()
                            .property(move |_, key, _, value| {
                                let target = match key {
                                    Some("default.configured.audio.sink") => &configured,
                                    Some("default.audio.sink") => &current,
                                    _ => return 0,
                                };
                                *target.borrow_mut() = value.and_then(sink_name_from_json);
                                0
                            })
                            .register();
                        bound.borrow_mut().push((metadata, listener));
                    }
                    _ => {}
                }
            }
        })
        .register();
    pw.settle();

    let outputs = PipeWireOutputs {
        sink: configured.take().or(current.take()),
        cards: cards.take(),
        cards_with_sinks: cards_with_sinks.take(),
    };
    debug!("PipeWire outputs: {outputs:?}");
    Some(outputs)
}

/// Values a format property allows.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Allowed {
    Values(Vec<u32>),
    Range(u32, u32),
}

impl Allowed {
    pub(super) fn contains(&self, value: u32) -> bool {
        match self {
            Self::Values(values) => values.contains(&value),
            Self::Range(min, max) => (*min..=*max).contains(&value),
        }
    }

    pub(super) fn min(&self) -> Option<u32> {
        match self {
            Self::Values(values) => values.iter().copied().min(),
            Self::Range(min, _) => Some(*min),
        }
    }
}

/// One raw-PCM `EnumFormat` entry of a sink.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct SinkFormat {
    pub(super) encodings: Vec<Encoding>,
    pub(super) rates: Allowed,
    pub(super) channels: Allowed,
}

/// A sink node kept bound, with its listener, until its info and params arrive.
type BoundSink = (Node, NodeListener, Rc<RefCell<SinkNode>>);

#[derive(Default)]
struct SinkNode {
    /// `(alsa.card, alsa.device)`
    pcm: Option<(String, String)>,
    formats: Vec<SinkFormat>,
}

/// The formats PipeWire's sink for `hw:<card>,<device>` accepts. PipeWire reads these from the
/// device itself, so this works while it has the card open. `None` if there's no such sink.
pub(super) fn sink_formats(card: i32, device: u32) -> Option<Vec<SinkFormat>> {
    let pw = Connection::new()?;
    let nodes: Rc<RefCell<Vec<BoundSink>>> = Rc::default();

    let _registry_listener = pw
        .registry
        .add_listener_local()
        .global({
            let registry = pw.registry.clone();
            let nodes = nodes.clone();
            move |global| {
                let is_alsa_sink = global.type_ == ObjectType::Node
                    && global.props.is_some_and(|props| {
                        props.get("media.class") == Some("Audio/Sink")
                            && props
                                .get("object.path")
                                .is_some_and(|p| p.starts_with("alsa:"))
                    });
                if !is_alsa_sink {
                    return;
                }
                let Ok(node) = registry.bind::<Node, _>(global) else {
                    return;
                };
                let sink: Rc<RefCell<SinkNode>> = Rc::default();
                let listener = node
                    .add_listener_local()
                    .info({
                        let sink = sink.clone();
                        move |info| {
                            if let Some(props) = info.props()
                                && let (Some(card), Some(device)) =
                                    (props.get("alsa.card"), props.get("alsa.device"))
                            {
                                sink.borrow_mut().pcm = Some((card.into(), device.into()));
                            }
                        }
                    })
                    .param({
                        let sink = sink.clone();
                        move |_, _, _, _, param| {
                            if let Some(format) = param.and_then(parse_sink_format) {
                                sink.borrow_mut().formats.push(format);
                            }
                        }
                    })
                    .register();
                node.enum_params(0, Some(ParamType::EnumFormat), 0, u32::MAX);
                nodes.borrow_mut().push((node, listener, sink));
            }
        })
        .register();
    pw.settle();

    let pcm = (card.to_string(), device.to_string());
    let formats = nodes.borrow().iter().find_map(|(_, _, sink)| {
        let sink = sink.borrow();
        (sink.pcm.as_ref() == Some(&pcm)).then(|| sink.formats.clone())
    })?;
    debug!("PipeWire formats for hw:{card},{device}: {formats:?}");
    (!formats.is_empty()).then_some(formats)
}

fn parse_sink_format(pod: &Pod) -> Option<SinkFormat> {
    let (_, Value::Object(object)) = PodDeserializer::deserialize_any_from(pod.as_bytes()).ok()?
    else {
        return None;
    };
    let property = |key| {
        object
            .properties
            .iter()
            .find(|property| property.key == key)
            .map(|property| &property.value)
    };
    // iec958 (passthrough) entries are skipped
    if ids(property(spa_sys::SPA_FORMAT_mediaSubtype)?) != [spa_sys::SPA_MEDIA_SUBTYPE_raw] {
        return None;
    }
    let encodings = ids(property(spa_sys::SPA_FORMAT_AUDIO_format)?)
        .into_iter()
        .filter_map(|id| encoding_of(AudioFormat::from_raw(id)))
        .fold(Vec::new(), |mut encodings, encoding| {
            if !encodings.contains(&encoding) {
                encodings.push(encoding);
            }
            encodings
        });
    Some(SinkFormat {
        encodings,
        rates: ints(property(spa_sys::SPA_FORMAT_AUDIO_rate)?)?,
        channels: ints(property(spa_sys::SPA_FORMAT_AUDIO_channels)?)?,
    })
}

fn encoding_of(format: AudioFormat) -> Option<Encoding> {
    Some(match format {
        AudioFormat::U8 => Encoding::U8,
        AudioFormat::S16LE => Encoding::S16,
        AudioFormat::S24LE => Encoding::S24Packed,
        AudioFormat::S24_32LE => Encoding::S24In32,
        AudioFormat::S32LE => Encoding::S32,
        AudioFormat::F32LE => Encoding::F32,
        _ => return None,
    })
}

fn ids(value: &Value) -> Vec<u32> {
    match value {
        Value::Id(Id(id)) => vec![*id],
        Value::Choice(ChoiceValue::Id(Choice(_, choice))) => match choice {
            ChoiceEnum::None(Id(id)) => vec![*id],
            ChoiceEnum::Enum {
                default,
                alternatives,
            } => std::iter::once(default)
                .chain(alternatives)
                .map(|Id(id)| *id)
                .collect(),
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}

fn ints(value: &Value) -> Option<Allowed> {
    let to_u32 = |value: &i32| u32::try_from(*value).ok();
    match value {
        Value::Int(value) => Some(Allowed::Values(vec![to_u32(value)?])),
        Value::Choice(ChoiceValue::Int(Choice(_, choice))) => match choice {
            ChoiceEnum::None(value) => Some(Allowed::Values(vec![to_u32(value)?])),
            ChoiceEnum::Range { min, max, .. } | ChoiceEnum::Step { min, max, .. } => {
                Some(Allowed::Range(to_u32(min)?, to_u32(max)?))
            }
            ChoiceEnum::Enum {
                default,
                alternatives,
            } => Some(Allowed::Values(
                std::iter::once(default)
                    .chain(alternatives)
                    .filter_map(to_u32)
                    .collect(),
            )),
            _ => None,
        },
        _ => None,
    }
}

/// ALSA playback nodes have an object path of `alsa:acp:<ALSA card ID>:<n>:playback`.
fn card_of_playback_node(object_path: &str) -> Option<String> {
    let rest = object_path.strip_prefix("alsa:acp:")?;
    let (card_id, rest) = rest.split_once(':')?;
    rest.ends_with(":playback").then(|| card_id.to_string())
}

/// Whether `card_id` has playback nodes again. `None` if PipeWire isn't running.
pub(super) fn card_has_sink(card_id: &str) -> Option<bool> {
    Some(query()?.cards_with_sinks.iter().any(|id| id == card_id))
}

/// ALSA cards show up as devices named `alsa_card.<id>`, with an object path of
/// `alsa:acp:<ALSA card ID>`.
fn card_of_device(name: &str, object_path: &str) -> Option<(String, String)> {
    let card_id = object_path.strip_prefix("alsa:acp:")?;
    (name.starts_with("alsa_card.") && !card_id.is_empty() && !card_id.contains(':'))
        .then(|| (name.to_string(), card_id.to_string()))
}

/// Metadata values are JSON, for example `{"name":"alsa_output.pci-0000_00_1f.3.analog-stereo"}`.
fn sink_name_from_json(value: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(value).ok()?;
    value.get("name")?.as_str().map(str::to_string)
}

/// Whether the PipeWire sink `sink` belongs to the card WirePlumber calls `card_device_name`. A
/// card named `alsa_card.<id>` has sinks named `alsa_output.<id>.<profile>`.
pub(super) fn sink_matches_card(sink: &str, card_device_name: &str) -> bool {
    let (Some(sink_id), Some(card_id)) = (
        sink.strip_prefix("alsa_output."),
        card_device_name.strip_prefix("alsa_card."),
    ) else {
        return false;
    };
    sink_id
        .strip_prefix(card_id)
        .is_some_and(|profile| profile.starts_with('.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_choices() {
        use pipewire::spa::utils::ChoiceFlags;

        let range = Value::Choice(ChoiceValue::Int(Choice(
            ChoiceFlags::empty(),
            ChoiceEnum::Range {
                default: 48_000,
                min: 32_000,
                max: 192_000,
            },
        )));
        assert_eq!(ints(&range), Some(Allowed::Range(32_000, 192_000)));
        assert_eq!(ints(&Value::Int(2)), Some(Allowed::Values(vec![2])));

        let formats = Value::Choice(ChoiceValue::Id(Choice(
            ChoiceFlags::empty(),
            ChoiceEnum::Enum {
                default: Id(AudioFormat::S32LE.as_raw()),
                alternatives: vec![
                    Id(AudioFormat::S32LE.as_raw()),
                    Id(AudioFormat::S16LE.as_raw()),
                ],
            },
        )));
        let encodings: Vec<_> = ids(&formats)
            .into_iter()
            .filter_map(|id| encoding_of(AudioFormat::from_raw(id)))
            .collect();
        assert_eq!(encodings, [Encoding::S32, Encoding::S32, Encoding::S16]);
    }

    #[test]
    fn parses_metadata_json() {
        assert_eq!(
            sink_name_from_json(r#"{"name":"alsa_output.x.analog-stereo"}"#).as_deref(),
            Some("alsa_output.x.analog-stereo")
        );
        assert_eq!(sink_name_from_json("nonsense"), None);
    }

    #[test]
    fn maps_pipewire_devices_to_card_ids() {
        assert_eq!(
            card_of_device("alsa_card.usb-X-00", "alsa:acp:K7"),
            Some(("alsa_card.usb-X-00".into(), "K7".into()))
        );
        assert_eq!(card_of_device("v4l2_device.x", "v4l2:/dev/video0"), None);
        assert_eq!(
            card_of_device("alsa_card.x", "alsa:acp:K7:1:playback"),
            None
        );
    }

    #[test]
    fn matches_sinks_to_cards() {
        let card = "alsa_card.usb-GuangZhou_FiiO_Electronics_Co._Ltd_FiiO_K7-00";
        assert!(sink_matches_card(
            "alsa_output.usb-GuangZhou_FiiO_Electronics_Co._Ltd_FiiO_K7-00.analog-stereo",
            card
        ));
        assert!(!sink_matches_card(
            "alsa_output.usb-GuangZhou_FiiO_Electronics_Co._Ltd_FiiO_K7-000.analog-stereo",
            card
        ));
        assert!(!sink_matches_card("bluez_output.AA_BB.1", card));
    }
}
