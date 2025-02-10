use cealn_data::action::{StructuredMessageConfig, StructuredMessageLevel};
use cealn_event::{BuildEventData, EventContext};
use cealn_protocol::query::{StdioLine, StdioStreamType};

pub fn emit_events_for_line(
    events: &mut EventContext,
    stream: StdioStreamType,
    structured_messages: Option<&StructuredMessageConfig>,
    line: &[u8],
) {
    if let Some(structured_message_config) = structured_messages {
        if let Some(serde_json::Value::Object(obj)) = serde_json::from_slice::<serde_json::Value>(line).ok() {
            let mut data = prost_types::Struct::default();
            for (k, v) in &obj {
                data.fields.insert(k.clone(), json_to_protobuf(v));
            }
            let line_json = serde_json::Value::Object(obj);

            let mut level = StructuredMessageLevel::Info;
            for (k, v) in &structured_message_config.level_map {
                if k.extract_match(&line_json).is_some() {
                    level = v.clone();
                    break;
                }
            }

            let mut human_message = None;
            for human_json_path in &structured_message_config.human_messages {
                let m = human_json_path.extract_match(&line_json);
                if let Some(serde_json::Value::String(s)) = m.as_deref() {
                    human_message = Some(s.to_owned());
                }
            }

            events.send(BuildEventData::Message {
                level,
                data,
                human_message,
            });
            return;
        }
    }

    events.send(BuildEventData::Stdio {
        line: StdioLine {
            stream,
            contents: line.to_owned(),
        },
    });
}

fn json_to_protobuf(json: &serde_json::Value) -> prost_types::Value {
    use prost_types::{value::Kind, ListValue, Struct};
    match json {
        serde_json::Value::Null => prost_types::Value {
            kind: Some(Kind::NullValue(0)),
        },
        serde_json::Value::Bool(value) => prost_types::Value {
            kind: Some(Kind::BoolValue(*value)),
        },
        serde_json::Value::Number(value) => prost_types::Value {
            kind: Some(Kind::NumberValue(value.as_f64().unwrap_or(0.0))),
        },
        serde_json::Value::String(value) => prost_types::Value {
            kind: Some(Kind::StringValue(value.clone())),
        },
        serde_json::Value::Array(values) => prost_types::Value {
            kind: Some(Kind::ListValue(ListValue {
                values: values.iter().map(json_to_protobuf).collect(),
            })),
        },
        serde_json::Value::Object(json_obj) => {
            let mut prost_obj = Struct::default();
            for (k, v) in json_obj {
                prost_obj.fields.insert(k.clone(), json_to_protobuf(v));
            }
            prost_types::Value {
                kind: Some(Kind::StructValue(prost_obj)),
            }
        }
    }
}
