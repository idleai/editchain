//! Preserve canonical source bytes without repeatedly escaping unchanged snapshots.

use editchain_protocol::editor::{EditorChange, EditorEvent, EditorEventKind};
use serde_json::{json, Value};
use std::borrow::Cow;

#[derive(Debug, Default)]
pub(crate) struct Encoding {
    previous: Option<(String, Vec<u8>)>,
}

impl Encoding {
    pub(super) fn encode(&mut self, observation: &EditorEvent) -> super::Result<Vec<u8>> {
        let EditorEvent {
            schema,
            session,
            identity,
            sequence,
            time_ms,
            event,
        } = observation;
        let EditorEventKind::DocumentChanged {
            document,
            before_version,
            before,
            after,
            changes,
            reason,
            origin,
        } = event
        else {
            if let EditorEventKind::DocumentSnapshot { text, .. } = event {
                self.previous = Some((text.clone(), serde_json::to_vec(text)?));
            } else if matches!(
                event,
                EditorEventKind::TrackingStopped | EditorEventKind::TrackingGap { .. }
            ) {
                self.previous = None;
            }
            return Ok(serde_json::to_vec(
                &json!({"source":"vscode.editor", "event":observation}),
            )?);
        };
        let old = match &self.previous {
            Some((text, bytes)) if text == before => Cow::Borrowed(bytes.as_slice()),
            _ => Cow::Owned(serde_json::to_vec(before)?),
        };
        let new = match replacement(before, after, &old, changes)? {
            Some(encoded) => encoded,
            None => serde_json::to_vec(after)?,
        };
        // Both maps contain only small metadata. Their sorted keys and the
        // explicit text positions match serde_json::Value's canonical encoding.
        let mut payload = json!({"before_version":before_version,"changes":changes,
            "document":document,"reason":reason,"type":"document_changed"});
        if let Some(origin) = origin {
            drop(
                payload
                    .as_object_mut()
                    .ok_or("missing payload object")?
                    .insert("origin".into(), serde_json::to_value(origin)?),
            );
        }
        let mut envelope =
            json!({"schema":schema,"sequence":sequence,"session":session,"time_ms":time_ms});
        if let Some(identity) = identity {
            drop(
                envelope
                    .as_object_mut()
                    .ok_or("missing envelope object")?
                    .insert("identity".into(), serde_json::to_value(identity)?),
            );
        }
        let mut raw = Vec::with_capacity(old.len().saturating_add(new.len()).saturating_add(1024));
        raw.extend_from_slice(b"{\"event\":{\"event\":{\"after\":");
        raw.extend_from_slice(&new);
        raw.extend_from_slice(b",\"before\":");
        raw.extend_from_slice(&old);
        append_fields(&mut raw, &payload)?;
        append_fields(&mut raw, &envelope)?;
        raw.extend_from_slice(b",\"source\":\"vscode.editor\"}");
        self.previous = Some((after.clone(), new));
        Ok(raw)
    }
}

fn append_fields(raw: &mut Vec<u8>, fields: &Value) -> super::Result<()> {
    let encoded = serde_json::to_vec(fields)?;
    raw.push(b',');
    raw.extend_from_slice(encoded.get(1..).ok_or("missing metadata object")?);
    Ok(())
}

fn replacement(
    before: &str,
    after: &str,
    json: &[u8],
    changes: &[EditorChange],
) -> super::Result<Option<Vec<u8>>> {
    let [change] = changes else {
        return Ok(None);
    };
    let Some(range) = change.byte_range(before) else {
        return Ok(None);
    };
    let prefix = before.get(..range.start).ok_or("missing source prefix")?;
    let suffix = before.get(range.end..).ok_or("missing source suffix")?;
    // Protocol validation already checked the full replacement. Retain a local
    // check so this cache can never change the supplied observation's contents.
    let cut = prefix.len().saturating_add(change.text.len());
    if after.get(..prefix.len()) != Some(prefix)
        || after.get(prefix.len()..cut) != Some(change.text.as_str())
        || after.get(cut..) != Some(suffix)
    {
        return Ok(None);
    }
    let start = if range.start <= before.len() / 2 {
        serde_json::to_vec(prefix)?.len().saturating_sub(1)
    } else {
        let tail = before.get(range.start..).ok_or("missing source tail")?;
        json.len()
            .saturating_sub(serde_json::to_vec(tail)?.len().saturating_sub(1))
    };
    let removed = serde_json::to_vec(before.get(range).ok_or("missing replacement range")?)?
        .len()
        .saturating_sub(2);
    let text = serde_json::to_vec(&change.text)?;
    let mut encoded = Vec::with_capacity(
        json.len()
            .saturating_sub(removed)
            .saturating_add(text.len().saturating_sub(2)),
    );
    encoded.extend_from_slice(json.get(..start).ok_or("invalid JSON prefix")?);
    encoded.extend_from_slice(
        text.get(1..text.len().saturating_sub(1))
            .ok_or("invalid JSON string")?,
    );
    encoded.extend_from_slice(
        json.get(start.saturating_add(removed)..)
            .ok_or("invalid JSON suffix")?,
    );
    Ok(Some(encoded))
}
